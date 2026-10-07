use axum::{
    extract::{FromRequestParts, State},
    http::{header, request::Parts, HeaderMap, StatusCode},
    Json,
};
use chrono::{DateTime, Duration, Utc};
use trailway_proto::{
    Heartbeat, RegisterRequest, RegisterResponse, Resource, Server, ServerStatus,
    OFFLINE_AFTER_SECS, SERVER_TOKEN_PREFIX,
};
use uuid::Uuid;

use crate::{
    auth::AuthUser,
    crypto::{hash_token, random_token},
    error::{ApiError, ApiJson},
    server_keys::KEY_PREFIX,
    AppState,
};

const MAX_FIELD_LEN: usize = 255;

fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .map(str::trim)
}

/// A server key (`tw_sk_`) in the `Authorization` header. Only the register
/// endpoint takes this.
pub struct KeyAuth {
    pub key_id: Uuid,
    pub user_id: Uuid,
}

impl FromRequestParts<AppState> for KeyAuth {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let secret = bearer(&parts.headers)
            .filter(|t| t.starts_with(KEY_PREFIX))
            .ok_or(ApiError::Unauthorized)?;
        let row: Option<(Uuid, Uuid)> = sqlx::query_as(
            "SELECT id, user_id FROM server_keys WHERE key_hash = $1 AND revoked_at IS NULL",
        )
        .bind(hash_token(secret))
        .fetch_optional(&state.pool)
        .await?;
        row.map(|(key_id, user_id)| Self { key_id, user_id })
            .ok_or(ApiError::Unauthorized)
    }
}

/// A per-server token (`tw_st_`) in the `Authorization` header. Only the
/// heartbeat endpoint takes this; user endpoints need a session cookie.
pub struct ServerAuth {
    pub server_id: Uuid,
}

impl FromRequestParts<AppState> for ServerAuth {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let token = bearer(&parts.headers)
            .filter(|t| t.starts_with(SERVER_TOKEN_PREFIX))
            .ok_or(ApiError::Unauthorized)?;
        let id: Option<Uuid> = sqlx::query_scalar("SELECT id FROM servers WHERE token_hash = $1")
            .bind(hash_token(token))
            .fetch_optional(&state.pool)
            .await?;
        id.map(|server_id| Self { server_id })
            .ok_or(ApiError::Unauthorized)
    }
}

fn check_text(name: &str, value: &str) -> Result<String, ApiError> {
    let v = value.trim();
    if v.is_empty() || v.chars().count() > MAX_FIELD_LEN || v.chars().any(char::is_control) {
        return Err(ApiError::validation(format!("{name} is not valid")));
    }
    Ok(v.to_string())
}

fn to_db(name: &str, value: u64) -> Result<i64, ApiError> {
    i64::try_from(value).map_err(|_| ApiError::validation(format!("{name} is out of range")))
}

fn check_resource(name: &str, r: Resource) -> Result<(i64, i64), ApiError> {
    if r.used > r.total {
        return Err(ApiError::validation(format!("{name} used exceeds total")));
    }
    Ok((to_db(name, r.total)?, to_db(name, r.used)?))
}

/// Registers (or re-registers) a host. The same user and `machine_id` always
/// map to the same server row; a re-register issues a fresh token and
/// invalidates the previous one.
pub async fn register(
    State(state): State<AppState>,
    auth: KeyAuth,
    ApiJson(body): ApiJson<RegisterRequest>,
) -> Result<(StatusCode, Json<RegisterResponse>), ApiError> {
    let machine_id = check_text("machine_id", &body.machine_id)?;
    let hostname = check_text("hostname", &body.hostname)?;
    let agent_version = check_text("agent_version", &body.agent_version)?;

    let token = format!("{SERVER_TOKEN_PREFIX}{}", random_token());
    let server_id: Uuid = sqlx::query_scalar(
        "INSERT INTO servers (user_id, key_id, machine_id, hostname, agent_version, token_hash) \
         VALUES ($1, $2, $3, $4, $5, $6) \
         ON CONFLICT (user_id, machine_id) DO UPDATE SET \
           key_id = EXCLUDED.key_id, hostname = EXCLUDED.hostname, \
           agent_version = EXCLUDED.agent_version, token_hash = EXCLUDED.token_hash \
         RETURNING id",
    )
    .bind(auth.user_id)
    .bind(auth.key_id)
    .bind(&machine_id)
    .bind(&hostname)
    .bind(&agent_version)
    .bind(hash_token(&token))
    .fetch_one(&state.pool)
    .await?;

    Ok((
        StatusCode::CREATED,
        Json(RegisterResponse { server_id, token }),
    ))
}

pub async fn heartbeat(
    State(state): State<AppState>,
    auth: ServerAuth,
    ApiJson(body): ApiJson<Heartbeat>,
) -> Result<StatusCode, ApiError> {
    let agent_version = check_text("agent_version", &body.agent_version)?;
    let (cpu_total, cpu_used) = check_resource("cpu", body.cpu)?;
    let (memory_total, memory_used) = check_resource("memory", body.memory)?;
    let (disk_total, disk_used) = check_resource("disk", body.disk)?;

    sqlx::query(
        "UPDATE servers SET agent_version = $2, cpu_total = $3, cpu_used = $4, \
         memory_total = $5, memory_used = $6, disk_total = $7, disk_used = $8, \
         kvm = $9, last_heartbeat_at = now() WHERE id = $1",
    )
    .bind(auth.server_id)
    .bind(&agent_version)
    .bind(cpu_total)
    .bind(cpu_used)
    .bind(memory_total)
    .bind(memory_used)
    .bind(disk_total)
    .bind(disk_used)
    .bind(body.kvm)
    .execute(&state.pool)
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Online while the last heartbeat is at most [`OFFLINE_AFTER_SECS`] old.
pub fn status_for(last_heartbeat: Option<DateTime<Utc>>, now: DateTime<Utc>) -> ServerStatus {
    match last_heartbeat {
        Some(t) if now - t <= Duration::seconds(OFFLINE_AFTER_SECS) => ServerStatus::Online,
        _ => ServerStatus::Offline,
    }
}

#[derive(sqlx::FromRow)]
struct ServerRow {
    id: Uuid,
    hostname: String,
    agent_version: String,
    cpu_total: i64,
    cpu_used: i64,
    memory_total: i64,
    memory_used: i64,
    disk_total: i64,
    disk_used: i64,
    kvm: bool,
    last_heartbeat_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
}

fn resource(total: i64, used: i64) -> Resource {
    Resource {
        total: total.max(0) as u64,
        used: used.max(0) as u64,
    }
}

pub async fn list(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
) -> Result<Json<Vec<Server>>, ApiError> {
    let rows = sqlx::query_as::<_, ServerRow>(
        "SELECT id, hostname, agent_version, cpu_total, cpu_used, memory_total, memory_used, \
         disk_total, disk_used, kvm, last_heartbeat_at, created_at FROM servers \
         WHERE user_id = $1 ORDER BY created_at, id",
    )
    .bind(user.id)
    .fetch_all(&state.pool)
    .await?;
    let now = Utc::now();
    Ok(Json(
        rows.into_iter()
            .map(|r| Server {
                id: r.id,
                hostname: r.hostname,
                status: status_for(r.last_heartbeat_at, now),
                cpu: resource(r.cpu_total, r.cpu_used),
                memory: resource(r.memory_total, r.memory_used),
                disk: resource(r.disk_total, r.disk_used),
                kvm: r.kvm,
                agent_version: r.agent_version,
                last_heartbeat_at: r.last_heartbeat_at,
                created_at: r.created_at,
            })
            .collect(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offline_after_thirty_seconds() {
        let now = Utc::now();
        let ago = |s: i64| Some(now - Duration::seconds(s));
        assert_eq!(status_for(None, now), ServerStatus::Offline);
        assert_eq!(status_for(ago(0), now), ServerStatus::Online);
        assert_eq!(status_for(ago(30), now), ServerStatus::Online);
        assert_eq!(status_for(ago(31), now), ServerStatus::Offline);
    }

    #[test]
    fn bearer_parsing() {
        let mut h = HeaderMap::new();
        assert_eq!(bearer(&h), None);
        h.insert(header::AUTHORIZATION, "Bearer abc".parse().unwrap());
        assert_eq!(bearer(&h), Some("abc"));
        h.insert(header::AUTHORIZATION, "Basic abc".parse().unwrap());
        assert_eq!(bearer(&h), None);
    }

    #[test]
    fn resource_validation() {
        assert!(check_resource("cpu", Resource { total: 1, used: 2 }).is_err());
        assert!(check_resource(
            "cpu",
            Resource {
                total: u64::MAX,
                used: 0
            }
        )
        .is_err());
        assert_eq!(
            check_resource("cpu", Resource { total: 4, used: 1 }).unwrap(),
            (4, 1)
        );
    }
}
