use axum::{
    extract::{FromRequestParts, State},
    http::{header, request::Parts, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use chrono::{DateTime, Utc};
use sqlx::PgPool;
use trailway_proto::{Credentials, User};
use uuid::Uuid;

use crate::{
    crypto::{dummy_hash, hash_password, hash_token, random_token, verify_password},
    error::{ApiError, ApiJson},
    AppState,
};

pub const SESSION_COOKIE: &str = "tw_session";
const MIN_PASSWORD_LEN: usize = 8;
const MAX_PASSWORD_LEN: usize = 128;
const MAX_EMAIL_LEN: usize = 254;

#[derive(sqlx::FromRow)]
struct UserRow {
    id: Uuid,
    email: String,
    password_hash: String,
    created_at: DateTime<Utc>,
}

impl UserRow {
    fn into_user(self) -> User {
        User {
            id: self.id,
            email: self.email,
            created_at: self.created_at,
        }
    }
}

fn normalize_email(raw: &str) -> Result<String, ApiError> {
    let email = raw.trim().to_lowercase();
    let valid = email.len() <= MAX_EMAIL_LEN
        && email.split_once('@').is_some_and(|(local, domain)| {
            !local.is_empty()
                && domain.contains('.')
                && !domain.starts_with('.')
                && !domain.ends_with('.')
                && !domain.contains('@')
        })
        && !email.chars().any(|c| c.is_whitespace() || c.is_control());
    if valid {
        Ok(email)
    } else {
        Err(ApiError::validation("Email address is not valid"))
    }
}

fn validate_password(password: &str) -> Result<(), ApiError> {
    let len = password.chars().count();
    if !(MIN_PASSWORD_LEN..=MAX_PASSWORD_LEN).contains(&len) {
        return Err(ApiError::validation(format!(
            "Password must be between {MIN_PASSWORD_LEN} and {MAX_PASSWORD_LEN} characters"
        )));
    }
    Ok(())
}

fn session_cookie(state: &AppState, token: &str, max_age_secs: i64) -> HeaderValue {
    let secure = if state.config.cookie_secure {
        "; Secure"
    } else {
        ""
    };
    HeaderValue::from_str(&format!(
        "{SESSION_COOKIE}={token}; HttpOnly; SameSite=Lax; Path=/; Max-Age={max_age_secs}{secure}"
    ))
    .expect("cookie is ascii")
}

async fn create_session(state: &AppState, user_id: Uuid) -> Result<HeaderValue, ApiError> {
    let token = random_token();
    let ttl = state.config.session_ttl_secs;
    sqlx::query(
        "INSERT INTO sessions (user_id, token_hash, expires_at) \
         VALUES ($1, $2, now() + make_interval(secs => $3))",
    )
    .bind(user_id)
    .bind(hash_token(&token))
    .bind(ttl as f64)
    .execute(&state.pool)
    .await?;
    Ok(session_cookie(state, &token, ttl))
}

pub async fn signup(
    State(state): State<AppState>,
    ApiJson(body): ApiJson<Credentials>,
) -> Result<Response, ApiError> {
    let email = normalize_email(&body.email)?;
    validate_password(&body.password)?;
    let password = body.password;
    let hash = tokio::task::spawn_blocking(move || hash_password(&password))
        .await
        .map_err(ApiError::internal)?
        .map_err(ApiError::internal)?;

    let inserted = sqlx::query_as::<_, UserRow>(
        "INSERT INTO users (email, password_hash) VALUES ($1, $2) \
         RETURNING id, email, password_hash, created_at",
    )
    .bind(&email)
    .bind(&hash)
    .fetch_one(&state.pool)
    .await;
    let user = match inserted {
        Ok(row) => row,
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
            return Err(ApiError::Conflict(
                "An account with this email already exists".into(),
            ))
        }
        Err(e) => return Err(e.into()),
    };

    let cookie = create_session(&state, user.id).await?;
    Ok((
        StatusCode::CREATED,
        [(header::SET_COOKIE, cookie)],
        Json(user.into_user()),
    )
        .into_response())
}

pub async fn login(
    State(state): State<AppState>,
    ApiJson(body): ApiJson<Credentials>,
) -> Result<Response, ApiError> {
    let email = body.email.trim().to_lowercase();
    let row = sqlx::query_as::<_, UserRow>(
        "SELECT id, email, password_hash, created_at FROM users WHERE lower(email) = $1",
    )
    .bind(&email)
    .fetch_optional(&state.pool)
    .await?;

    let password = body.password;
    let stored = row
        .as_ref()
        .map(|r| r.password_hash.clone())
        .unwrap_or_else(|| dummy_hash().to_string());
    let ok = tokio::task::spawn_blocking(move || verify_password(&password, &stored))
        .await
        .map_err(ApiError::internal)?;
    let Some(user) = row.filter(|_| ok) else {
        return Err(ApiError::Unauthorized);
    };

    let cookie = create_session(&state, user.id).await?;
    Ok(([(header::SET_COOKIE, cookie)], Json(user.into_user())).into_response())
}

pub async fn logout(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    if let Some(token) = session_token(&headers) {
        sqlx::query("DELETE FROM sessions WHERE token_hash = $1")
            .bind(hash_token(&token))
            .execute(&state.pool)
            .await?;
    }
    Ok((
        StatusCode::NO_CONTENT,
        [(header::SET_COOKIE, session_cookie(&state, "", 0))],
    )
        .into_response())
}

pub async fn me(user: AuthUser) -> Json<User> {
    Json(user.0)
}

fn session_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(name, value)| *name == SESSION_COOKIE && !value.is_empty())
        .map(|(_, value)| value.to_string())
}

async fn user_for_token(pool: &PgPool, token: &str) -> Result<Option<User>, sqlx::Error> {
    let row = sqlx::query_as::<_, UserRow>(
        "SELECT u.id, u.email, u.password_hash, u.created_at FROM sessions s \
         JOIN users u ON u.id = s.user_id \
         WHERE s.token_hash = $1 AND s.expires_at > now()",
    )
    .bind(hash_token(token))
    .fetch_optional(pool)
    .await?;
    Ok(row.map(UserRow::into_user))
}

/// The user behind a valid, unexpired session cookie.
pub struct AuthUser(pub User);

impl FromRequestParts<AppState> for AuthUser {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let token = session_token(&parts.headers).ok_or(ApiError::Unauthorized)?;
        user_for_token(&state.pool, &token)
            .await?
            .map(AuthUser)
            .ok_or(ApiError::Unauthorized)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn email_validation() {
        assert_eq!(normalize_email("  A@B.co ").unwrap(), "a@b.co");
        for bad in ["", "nope", "a@b", "@b.co", "a@@b.co", "a b@c.co", "a@.co"] {
            assert!(normalize_email(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn password_length_bounds() {
        assert!(validate_password("short").is_err());
        assert!(validate_password("longenough").is_ok());
        assert!(validate_password(&"x".repeat(129)).is_err());
    }

    #[test]
    fn cookie_parsing() {
        let mut h = HeaderMap::new();
        h.insert(
            header::COOKIE,
            HeaderValue::from_static("a=1; tw_session=abc; b=2"),
        );
        assert_eq!(session_token(&h).as_deref(), Some("abc"));
        assert_eq!(session_token(&HeaderMap::new()), None);
    }
}
