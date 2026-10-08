use axum::{
    extract::{rejection::JsonRejection, FromRequest, Request},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::de::DeserializeOwned;
use trailway_proto::{ErrorBody, ErrorEnvelope};

#[derive(Debug)]
pub enum ApiError {
    BadRequest(String),
    Unauthorized,
    NotFound,
    Conflict(String),
    /// A refusal with its own machine-readable code, e.g. `insufficient_capacity`.
    Refused(&'static str, String),
    Internal,
}

impl ApiError {
    pub fn validation(message: impl Into<String>) -> Self {
        Self::BadRequest(message.into())
    }

    /// Log the underlying cause and return an opaque 500.
    pub fn internal(err: impl std::fmt::Display) -> Self {
        tracing::error!("internal error: {err}");
        Self::Internal
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(err: sqlx::Error) -> Self {
        Self::internal(err)
    }
}

/// Maps a unique-constraint violation to a 409 with `message`.
pub fn conflict_on_unique(err: sqlx::Error, message: &str) -> ApiError {
    match &err {
        sqlx::Error::Database(db) if db.is_unique_violation() => {
            ApiError::Conflict(message.to_string())
        }
        _ => err.into(),
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, code, message) = match self {
            Self::BadRequest(m) => (StatusCode::BAD_REQUEST, "invalid_request", m),
            Self::Unauthorized => (
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                "Authentication required or credentials invalid".to_string(),
            ),
            Self::NotFound => (StatusCode::NOT_FOUND, "not_found", "Not found".to_string()),
            Self::Conflict(m) => (StatusCode::CONFLICT, "conflict", m),
            Self::Refused(code, m) => (StatusCode::CONFLICT, code, m),
            Self::Internal => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                "Internal server error".to_string(),
            ),
        };
        let body = ErrorEnvelope {
            error: ErrorBody {
                code: code.into(),
                message,
            },
        };
        (status, Json(body)).into_response()
    }
}

/// `Json` extractor whose rejections use the API error envelope.
pub struct ApiJson<T>(pub T);

impl<S, T> FromRequest<S> for ApiJson<T>
where
    S: Send + Sync,
    T: DeserializeOwned,
{
    type Rejection = ApiError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        match Json::<T>::from_request(req, state).await {
            Ok(Json(v)) => Ok(Self(v)),
            Err(JsonRejection::MissingJsonContentType(_)) => Err(ApiError::validation(
                "Content-Type must be application/json",
            )),
            Err(_) => Err(ApiError::validation("Request body is not valid JSON")),
        }
    }
}
