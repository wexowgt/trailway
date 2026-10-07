use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use chrono::{DateTime, Utc};
use trailway_proto::{CreateServerKey, CreatedServerKey, ServerKey};
use uuid::Uuid;

use crate::{
    auth::AuthUser,
    crypto::{hash_token, random_token},
    error::{ApiError, ApiJson},
    AppState,
};

pub const KEY_PREFIX: &str = "tw_sk_";
/// Characters of the secret stored in clear for display (includes `tw_sk_`).
const DISPLAY_PREFIX_LEN: usize = KEY_PREFIX.len() + 6;
const MAX_NAME_LEN: usize = 100;

#[derive(sqlx::FromRow)]
struct KeyRow {
    id: Uuid,
    name: String,
    prefix: String,
    created_at: DateTime<Utc>,
    revoked_at: Option<DateTime<Utc>>,
}

impl From<KeyRow> for ServerKey {
    fn from(r: KeyRow) -> Self {
        Self {
            id: r.id,
            name: r.name,
            prefix: r.prefix,
            created_at: r.created_at,
            revoked_at: r.revoked_at,
        }
    }
}

pub async fn create(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    ApiJson(body): ApiJson<CreateServerKey>,
) -> Result<(StatusCode, Json<CreatedServerKey>), ApiError> {
    let name = body.name.trim().to_string();
    if name.is_empty() || name.chars().count() > MAX_NAME_LEN {
        return Err(ApiError::validation(format!(
            "Name must be between 1 and {MAX_NAME_LEN} characters"
        )));
    }

    let secret = format!("{KEY_PREFIX}{}", random_token());
    let row = sqlx::query_as::<_, KeyRow>(
        "INSERT INTO server_keys (user_id, name, prefix, key_hash) VALUES ($1, $2, $3, $4) \
         RETURNING id, name, prefix, created_at, revoked_at",
    )
    .bind(user.id)
    .bind(&name)
    .bind(&secret[..DISPLAY_PREFIX_LEN])
    .bind(hash_token(&secret))
    .fetch_one(&state.pool)
    .await?;

    Ok((
        StatusCode::CREATED,
        Json(CreatedServerKey {
            key: row.into(),
            secret,
        }),
    ))
}

pub async fn list(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
) -> Result<Json<Vec<ServerKey>>, ApiError> {
    let rows = sqlx::query_as::<_, KeyRow>(
        "SELECT id, name, prefix, created_at, revoked_at FROM server_keys \
         WHERE user_id = $1 ORDER BY created_at DESC, id",
    )
    .bind(user.id)
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(rows.into_iter().map(ServerKey::from).collect()))
}

/// Revoking is idempotent: an already revoked key keeps its first `revoked_at`.
pub async fn revoke(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let result = sqlx::query(
        "UPDATE server_keys SET revoked_at = COALESCE(revoked_at, now()) \
         WHERE id = $1 AND user_id = $2",
    )
    .bind(id)
    .bind(user.id)
    .execute(&state.pool)
    .await?;
    if result.rows_affected() == 0 {
        return Err(ApiError::NotFound);
    }
    Ok(StatusCode::NO_CONTENT)
}
