mod agent_ws;
mod auth;
mod crypto;
mod deployments;
mod domains;
mod error;
mod hub;
mod ledger;
mod observability;
mod projects;
mod server_keys;
mod servers;
mod services;

use std::path::PathBuf;

use axum::{
    extract::{Path, State},
    http::{header, StatusCode},
    response::IntoResponse,
    routing::{delete, get, post},
    Json, Router,
};
use sqlx::PgPool;
use trailway_proto::Health;

pub use error::ApiError;
pub use observability::prune_metrics;

const DEFAULT_SESSION_TTL_SECS: i64 = 30 * 24 * 60 * 60;

#[derive(Debug, Clone)]
pub struct Config {
    /// Add the `Secure` attribute to the session cookie (set behind HTTPS).
    pub cookie_secure: bool,
    pub session_ttl_secs: i64,
    /// Directory holding `trailway-agent-linux-<arch>` binaries served under
    /// `/downloads`. Unset disables the downloads.
    pub agent_dist_dir: Option<PathBuf>,
    /// Domain services get their public host name under. `{ip}` stands for
    /// the server's public IP with dashes, so the default needs no DNS setup;
    /// a real wildcard domain such as `apps.example.com` replaces it later.
    pub domain_base: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            cookie_secure: false,
            session_ttl_secs: DEFAULT_SESSION_TTL_SECS,
            agent_dist_dir: None,
            domain_base: domains::DEFAULT_DOMAIN_BASE.into(),
        }
    }
}

impl Config {
    /// Reads `API_COOKIE_SECURE`, `API_SESSION_TTL_SECS`, `API_AGENT_DIST_DIR` and
    /// `API_DOMAIN_BASE`.
    pub fn from_env() -> anyhow::Result<Self> {
        let mut config = Self::default();
        if let Ok(v) = std::env::var("API_COOKIE_SECURE") {
            config.cookie_secure = matches!(v.as_str(), "1" | "true");
        }
        if let Ok(v) = std::env::var("API_SESSION_TTL_SECS") {
            config.session_ttl_secs = v.parse()?;
        }
        config.agent_dist_dir = std::env::var_os("API_AGENT_DIST_DIR").map(PathBuf::from);
        if let Ok(v) = std::env::var("API_DOMAIN_BASE") {
            let base = v.trim().trim_matches('.');
            anyhow::ensure!(
                !base.is_empty()
                    && base
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '{' | '}')),
                "API_DOMAIN_BASE is not a valid domain"
            );
            config.domain_base = base.to_string();
        }
        Ok(config)
    }
}

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub config: Config,
    /// Live agent connections, by server.
    pub hub: hub::Hub,
}

impl AppState {
    pub fn new(pool: PgPool, config: Config) -> Self {
        Self {
            pool,
            config,
            hub: hub::Hub::default(),
        }
    }
}

pub fn router(state: AppState) -> Router {
    let v1 = Router::new()
        .route("/auth/signup", post(auth::signup))
        .route("/auth/login", post(auth::login))
        .route("/auth/logout", post(auth::logout))
        .route("/me", get(auth::me))
        .route(
            "/server-keys",
            post(server_keys::create).get(server_keys::list),
        )
        .route("/server-keys/{id}", delete(server_keys::revoke))
        .route("/servers", get(servers::list))
        .route("/agent/register", post(servers::register))
        .route("/agent/heartbeat", post(servers::heartbeat))
        .route("/agent/ws", get(agent_ws::connect))
        .route("/projects", post(projects::create).get(projects::list))
        .route(
            "/projects/{id}",
            get(projects::get)
                .patch(projects::rename)
                .delete(projects::delete),
        )
        .route(
            "/projects/{id}/environments",
            post(projects::create_environment).get(projects::list_environments),
        )
        .route(
            "/environments/{id}",
            get(projects::get_environment)
                .patch(projects::rename_environment)
                .delete(projects::delete_environment),
        )
        .route(
            "/environments/{id}/services",
            post(services::create).get(services::list),
        )
        .route(
            "/services/{id}",
            get(services::get)
                .patch(services::update)
                .delete(services::delete),
        )
        .route("/services/{id}/deploy", post(deployments::deploy))
        .route("/services/{id}/stop", post(deployments::stop))
        .route("/services/{id}/deployments", get(deployments::list))
        .route("/deployments/{id}", get(deployments::get))
        .route("/deployments/{id}/logs", get(deployments::logs))
        .route("/agent/usage", post(ledger::ingest_usage))
        .route("/servers/{id}/usage", get(ledger::server_usage))
        .route("/ledger/balance", get(ledger::balance))
        .route("/ledger/entries", get(ledger::entries))
        .route("/ledger/servers", get(observability::ledger_servers))
        .route(
            "/environments/{id}/metrics",
            get(observability::environment_metrics),
        )
        .route(
            "/environments/{id}/logs",
            get(observability::environment_logs),
        );
    Router::new()
        .route("/healthz", get(healthz))
        .route("/install.sh", get(install_script))
        .route("/downloads/{name}", get(download))
        .nest("/api/v1", v1)
        .with_state(state)
}

const INSTALL_SCRIPT: &str = include_str!("../assets/install.sh");
const AGENT_BINARIES: [&str; 2] = [
    "trailway-agent-linux-x86_64",
    "trailway-agent-linux-aarch64",
];

async fn install_script() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/x-shellscript; charset=utf-8")],
        INSTALL_SCRIPT,
    )
}

/// Serves a prebuilt agent binary. Only the known file names are allowed, so
/// the name can never escape the dist directory.
async fn download(State(state): State<AppState>, Path(name): Path<String>) -> impl IntoResponse {
    let Some(dir) = state.config.agent_dist_dir.as_ref() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if !AGENT_BINARIES.contains(&name.as_str()) {
        return StatusCode::NOT_FOUND.into_response();
    }
    match tokio::fs::read(dir.join(&name)).await {
        Ok(bytes) => ([(header::CONTENT_TYPE, "application/octet-stream")], bytes).into_response(),
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn healthz() -> Json<Health> {
    Json(Health {
        status: "ok".into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request};
    use http_body_util::BodyExt;
    use sqlx::postgres::PgPoolOptions;
    use tower::ServiceExt;

    #[tokio::test]
    async fn healthz_returns_ok() {
        // Lazy pool: /healthz never touches the database.
        let pool = PgPoolOptions::new()
            .connect_lazy("postgres://localhost/unused")
            .unwrap();
        let state = AppState::new(pool, Config::default());
        let res = router(state)
            .oneshot(Request::get("/healthz").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), 200);
        let body = res.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["status"], "ok");
    }
}
