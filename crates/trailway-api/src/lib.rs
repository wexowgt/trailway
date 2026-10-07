mod auth;
mod crypto;
mod error;
mod server_keys;

use axum::{
    routing::{delete, get, post},
    Json, Router,
};
use sqlx::PgPool;
use trailway_proto::Health;

pub use error::ApiError;

const DEFAULT_SESSION_TTL_SECS: i64 = 30 * 24 * 60 * 60;

#[derive(Debug, Clone)]
pub struct Config {
    /// Add the `Secure` attribute to the session cookie (set behind HTTPS).
    pub cookie_secure: bool,
    pub session_ttl_secs: i64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            cookie_secure: false,
            session_ttl_secs: DEFAULT_SESSION_TTL_SECS,
        }
    }
}

impl Config {
    /// Reads `API_COOKIE_SECURE` and `API_SESSION_TTL_SECS`.
    pub fn from_env() -> anyhow::Result<Self> {
        let mut config = Self::default();
        if let Ok(v) = std::env::var("API_COOKIE_SECURE") {
            config.cookie_secure = matches!(v.as_str(), "1" | "true");
        }
        if let Ok(v) = std::env::var("API_SESSION_TTL_SECS") {
            config.session_ttl_secs = v.parse()?;
        }
        Ok(config)
    }
}

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub config: Config,
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
        .route("/server-keys/{id}", delete(server_keys::revoke));
    Router::new()
        .route("/healthz", get(healthz))
        .nest("/api/v1", v1)
        .with_state(state)
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
        let state = AppState {
            pool,
            config: Config::default(),
        };
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
