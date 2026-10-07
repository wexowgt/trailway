mod client;
mod config;
mod host;

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use client::{ApiClient, ClientError};
use config::{Config, State};

const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(10);
const RETRY_DELAY: Duration = Duration::from_secs(5);

struct Args {
    config: PathBuf,
    state: PathBuf,
}

fn parse_args(args: &[String]) -> Result<Args, String> {
    if args.first().map(String::as_str) != Some("run") {
        return Err("usage: trailway-agent run --config <path> --state <path>".into());
    }
    let mut config = PathBuf::from("/etc/trailway/agent.json");
    let mut state = PathBuf::from("/var/lib/trailway/state.json");
    let mut rest = args[1..].iter();
    while let Some(flag) = rest.next() {
        let value = rest.next().ok_or_else(|| format!("{flag} needs a value"))?;
        match flag.as_str() {
            "--config" => config = value.into(),
            "--state" => state = value.into(),
            other => return Err(format!("unknown argument {other}")),
        }
    }
    Ok(Args { config, state })
}

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

async fn run(args: Args) -> anyhow::Result<()> {
    let config = Config::load(&args.config)?;
    let client = ApiClient::new(&config.api)?;
    let mut state = match State::load(&args.state) {
        Some(s) => s,
        None => register(&client, &config, &args.state).await,
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
                state = register(&client, &config, &args.state).await;
            }
            Err(e) => tracing::warn!("heartbeat failed: {e}"),
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if argv.first().map(String::as_str) == Some("--version") {
        println!("trailway-agent {}", host::AGENT_VERSION);
        return Ok(());
    }
    let args = parse_args(&argv).map_err(|e| anyhow::anyhow!(e))?;
    tokio::select! {
        res = run(args) => res,
        _ = tokio::signal::ctrl_c() => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn args_parse() {
        let a = parse_args(&v(&["run", "--config", "/c", "--state", "/s"])).unwrap();
        assert_eq!(a.config, PathBuf::from("/c"));
        assert_eq!(a.state, PathBuf::from("/s"));
        assert!(parse_args(&v(&[])).is_err());
        assert!(parse_args(&v(&["run", "--bogus", "x"])).is_err());
        assert!(parse_args(&v(&["run", "--config"])).is_err());
    }
}
