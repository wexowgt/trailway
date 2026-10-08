use std::{path::Path, time::Duration};

use crate::{
    client::{ApiClient, ClientError},
    config::{Config, State},
    host,
};

const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(10);
const RETRY_DELAY: Duration = Duration::from_secs(5);

/// Registers with the key and stores the server token. Retries until it
/// works, except that a rejected key is an error worth surfacing loudly.
async fn register(client: &ApiClient, config: &Config, state_path: &Path) -> State {
    loop {
        let result = match host::register_request() {
            Ok(req) => client.register(&config.key, &req).await,
            Err(e) => Err(ClientError::Other(e)),
        };
        match result {
            Ok(res) => {
                let state = State {
                    server_id: res.server_id,
                    token: res.token,
                };
                match state.save(state_path) {
                    Ok(()) => tracing::info!(server_id = %state.server_id, "registered"),
                    Err(e) => tracing::error!("could not store server token: {e:#}"),
                }
                return state;
            }
            Err(ClientError::Unauthorized) => {
                tracing::error!("server key rejected (revoked or wrong); retrying")
            }
            Err(e) => tracing::warn!("register failed: {e}"),
        }
        tokio::time::sleep(RETRY_DELAY).await;
    }
}

async fn run_loop(config_path: &Path, state_path: &Path) -> anyhow::Result<()> {
    let config = Config::load(config_path)?;
    let client = ApiClient::new(&config.api)?;
    let mut state = match State::load(state_path) {
        Some(s) => s,
        None => register(&client, &config, state_path).await,
    };
    let mut sampler = host::Sampler::new();
    let mut ticker = tokio::time::interval(HEARTBEAT_INTERVAL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;
        let hb = sampler.heartbeat();
        match client.heartbeat(&state.token, &hb).await {
            Ok(()) => tracing::debug!("heartbeat sent"),
            Err(ClientError::Unauthorized) => {
                tracing::warn!("server token rejected, registering again");
                state = register(&client, &config, state_path).await;
            }
            Err(e) => tracing::warn!("heartbeat failed: {e}"),
        }
    }
}

/// Runs the register + heartbeat daemon until interrupted.
pub fn run(config_path: &Path, state_path: &Path) -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(async {
            tokio::select! {
                res = run_loop(config_path, state_path) => res,
                _ = tokio::signal::ctrl_c() => Ok(()),
            }
        })
}
