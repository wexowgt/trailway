use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use trailway_agent::{
    deploy::{Manager, SharedProxy, SharedRuntime},
    firecracker::{Config as FcConfig, FirecrackerRuntime},
    proxy::{CaddyProxy, NoProxy},
    runtime::FakeRuntime,
    session::{self, SessionError},
};

use chrono::Utc;

use crate::{
    client::{ApiClient, ClientError},
    config::{Config, State},
    host,
    usage::UsageWindow,
};

const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(10);
/// One usage sample is sent per this many seconds, aggregated from heartbeats.
const USAGE_INTERVAL_SECS: i64 = 60;
/// How long to wait before looking for the public IP again.
const PUBLIC_IP_RETRY: Duration = Duration::from_secs(60);
const RETRY_DELAY: Duration = Duration::from_secs(5);
const MIN_RECONNECT_DELAY: Duration = Duration::from_secs(1);
const MAX_RECONNECT_DELAY: Duration = Duration::from_secs(30);

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

/// Server credentials shared by the heartbeat and the WebSocket. When the API
/// rejects the token, whoever notices first registers again.
struct Shared {
    client: ApiClient,
    config: Config,
    state_path: PathBuf,
    state: tokio::sync::Mutex<State>,
}

impl Shared {
    async fn token(&self) -> String {
        self.state.lock().await.token.clone()
    }

    async fn reregister(&self, rejected: &str) {
        let mut state = self.state.lock().await;
        // Someone else already got a new token.
        if state.token == rejected {
            *state = register(&self.client, &self.config, &self.state_path).await;
        }
    }
}

async fn heartbeats(shared: Arc<Shared>) {
    let mut sampler = host::Sampler::new();
    let mut ticker = tokio::time::interval(HEARTBEAT_INTERVAL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut window = UsageWindow::new(Utc::now());
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap_or_default();
    let mut public_ip: Option<String> = None;
    let mut ip_checked: Option<tokio::time::Instant> = None;
    loop {
        ticker.tick().await;
        // Public URLs depend on it: look again (not every beat) until it is known.
        if public_ip.is_none() && ip_checked.is_none_or(|t| t.elapsed() >= PUBLIC_IP_RETRY) {
            ip_checked = Some(tokio::time::Instant::now());
            public_ip = host::public_ip(&http).await;
            if public_ip.is_none() {
                tracing::warn!("could not find this host's public IP; set TRAILWAY_PUBLIC_IP");
            }
        }
        let mut hb = sampler.heartbeat();
        hb.public_ip = public_ip.clone();
        window.push(&hb);
        let token = shared.token().await;
        match shared.client.heartbeat(&token, &hb).await {
            Ok(()) => tracing::debug!("heartbeat sent"),
            Err(ClientError::Unauthorized) => {
                tracing::warn!("server token rejected, registering again");
                shared.reregister(&token).await;
            }
            Err(e) => tracing::warn!("heartbeat failed: {e}"),
        }
        let now = Utc::now();
        if (now - window.start()).num_seconds() >= USAGE_INTERVAL_SECS {
            let (sample, next) = window.flush(now);
            window = next;
            if let Some(sample) = sample {
                // Not buffered: a sample that misses its window earns nothing.
                let token = shared.token().await;
                match shared.client.usage(&token, &sample).await {
                    Ok(ack) => tracing::debug!(?ack.outcome, "usage sample sent"),
                    Err(ClientError::Unauthorized) => {
                        tracing::warn!("server token rejected, registering again");
                        shared.reregister(&token).await;
                    }
                    Err(e) => tracing::warn!("usage sample failed: {e}"),
                }
            }
        }
    }
}

/// Keeps one WebSocket to the API open, reconnecting with a growing delay.
async fn jobs(shared: Arc<Shared>, manager: Arc<Manager>) {
    let mut delay = MIN_RECONNECT_DELAY;
    loop {
        let token = shared.token().await;
        let started = tokio::time::Instant::now();
        match session::run_once(&shared.config.api, &token, &manager).await {
            Ok(()) => tracing::warn!("API closed the connection"),
            Err(SessionError::Unauthorized) => {
                tracing::warn!("server token rejected, registering again");
                shared.reregister(&token).await;
            }
            Err(SessionError::Other(e)) => tracing::warn!("API connection failed: {e:#}"),
        }
        // A connection that held for a while starts the backoff over.
        if started.elapsed() > MAX_RECONNECT_DELAY {
            delay = MIN_RECONNECT_DELAY;
        }
        tokio::time::sleep(delay).await;
        delay = (delay * 2).min(MAX_RECONNECT_DELAY);
    }
}

fn runtime() -> SharedRuntime {
    if host::fake_runtime() {
        tracing::warn!("TRAILWAY_FAKE_RUNTIME=1: VMs are simulated, nothing really runs");
        return Arc::new(FakeRuntime::default());
    }
    Arc::new(FirecrackerRuntime::new(FcConfig::from_env()))
}

/// Public routes go through the Caddy this host runs. `TRAILWAY_PROXY=off`
/// turns them off (development without Caddy).
fn proxy() -> SharedProxy {
    if std::env::var("TRAILWAY_PROXY").is_ok_and(|v| v == "off") {
        return Arc::new(NoProxy);
    }
    Arc::new(CaddyProxy::from_env())
}

async fn run_loop(config_path: &Path, state_path: &Path) -> anyhow::Result<()> {
    let config = Config::load(config_path)?;
    let client = ApiClient::new(&config.api)?;
    let state = match State::load(state_path) {
        Some(s) => s,
        None => register(&client, &config, state_path).await,
    };
    let record = state_path.with_file_name("deployments.json");
    let manager = Manager::new(runtime(), proxy(), Some(record));
    let shared = Arc::new(Shared {
        client,
        config,
        state_path: state_path.to_path_buf(),
        state: tokio::sync::Mutex::new(state),
    });
    tokio::join!(heartbeats(shared.clone()), jobs(shared, manager));
    Ok(())
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
