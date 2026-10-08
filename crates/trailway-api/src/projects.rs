//! Projects and their environments.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use chrono::{DateTime, Utc};
use sqlx::PgPool;
use trailway_proto::{Environment, NameBody, Project};
use uuid::Uuid;

use crate::{
    auth::AuthUser,
    deployments,
    error::{conflict_on_unique, ApiError, ApiJson},
    servers::check_text,
    AppState,
};

#[derive(sqlx::FromRow)]
struct ProjectRow {
    id: Uuid,
    name: String,
    created_at: DateTime<Utc>,
}

impl From<ProjectRow> for Project {
    fn from(r: ProjectRow) -> Self {
        Self {
            id: r.id,
            name: r.name,
            created_at: r.created_at,
        }
    }
}

#[derive(sqlx::FromRow)]
struct EnvironmentRow {
    id: Uuid,
    project_id: Uuid,
    name: String,
    created_at: DateTime<Utc>,
}

impl From<EnvironmentRow> for Environment {
    fn from(r: EnvironmentRow) -> Self {
        Self {
            id: r.id,
            project_id: r.project_id,
            name: r.name,
            created_at: r.created_at,
        }
    }
}

const PROJECT_COLS: &str = "id, name, created_at";
const ENV_COLS: &str = "e.id, e.project_id, e.name, e.created_at";

async fn owned_project(pool: &PgPool, user: Uuid, id: Uuid) -> Result<ProjectRow, ApiError> {
    sqlx::query_as(&format!(
        "SELECT {PROJECT_COLS} FROM projects WHERE id = $1 AND user_id = $2"
    ))
    .bind(id)
    .bind(user)
    .fetch_optional(pool)
    .await?
    .ok_or(ApiError::NotFound)
}

async fn owned_environment(
    pool: &PgPool,
    user: Uuid,
    id: Uuid,
) -> Result<EnvironmentRow, ApiError> {
    sqlx::query_as(&format!(
        "SELECT {ENV_COLS} FROM environments e JOIN projects p ON p.id = e.project_id \
         WHERE e.id = $1 AND p.user_id = $2"
    ))
    .bind(id)
    .bind(user)
    .fetch_optional(pool)
    .await?
    .ok_or(ApiError::NotFound)
}

/// The environment must exist and belong to `user`; used by the service routes.
pub async fn require_environment(pool: &PgPool, user: Uuid, id: Uuid) -> Result<(), ApiError> {
    owned_environment(pool, user, id).await.map(|_| ())
}

pub async fn create(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    ApiJson(body): ApiJson<NameBody>,
) -> Result<(StatusCode, Json<Project>), ApiError> {
    let name = check_text("name", &body.name)?;
    let row: ProjectRow = sqlx::query_as(&format!(
        "INSERT INTO projects (user_id, name) VALUES ($1, $2) RETURNING {PROJECT_COLS}"
    ))
    .bind(user.id)
    .bind(&name)
    .fetch_one(&state.pool)
    .await
    .map_err(|e| conflict_on_unique(e, "A project with this name already exists"))?;
    Ok((StatusCode::CREATED, Json(row.into())))
}

pub async fn list(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
) -> Result<Json<Vec<Project>>, ApiError> {
    let rows: Vec<ProjectRow> = sqlx::query_as(&format!(
        "SELECT {PROJECT_COLS} FROM projects WHERE user_id = $1 ORDER BY created_at, id"
    ))
    .bind(user.id)
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(rows.into_iter().map(Into::into).collect()))
}

pub async fn get(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<Project>, ApiError> {
    Ok(Json(owned_project(&state.pool, user.id, id).await?.into()))
}

pub async fn rename(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(id): Path<Uuid>,
    ApiJson(body): ApiJson<NameBody>,
) -> Result<Json<Project>, ApiError> {
    let name = check_text("name", &body.name)?;
    let row: Option<ProjectRow> = sqlx::query_as(&format!(
        "UPDATE projects SET name = $3 WHERE id = $1 AND user_id = $2 RETURNING {PROJECT_COLS}"
    ))
    .bind(id)
    .bind(user.id)
    .bind(&name)
    .fetch_optional(&state.pool)
    .await
    .map_err(|e| conflict_on_unique(e, "A project with this name already exists"))?;
    row.map(|r| Json(r.into())).ok_or(ApiError::NotFound)
}

/// Deleting a project stops everything running in it first.
pub async fn delete(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    owned_project(&state.pool, user.id, id).await?;
    let services: Vec<Uuid> = sqlx::query_scalar(
        "SELECT s.id FROM services s JOIN environments e ON e.id = s.environment_id \
         WHERE e.project_id = $1",
    )
    .bind(id)
    .fetch_all(&state.pool)
    .await?;
    deployments::stop_services(&state, &services).await?;
    sqlx::query("DELETE FROM projects WHERE id = $1")
        .bind(id)
        .execute(&state.pool)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn create_environment(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(project_id): Path<Uuid>,
    ApiJson(body): ApiJson<NameBody>,
) -> Result<(StatusCode, Json<Environment>), ApiError> {
    let name = check_text("name", &body.name)?;
    owned_project(&state.pool, user.id, project_id).await?;
    let row: EnvironmentRow = sqlx::query_as(
        "INSERT INTO environments (project_id, name) VALUES ($1, $2) \
         RETURNING id, project_id, name, created_at",
    )
    .bind(project_id)
    .bind(&name)
    .fetch_one(&state.pool)
    .await
    .map_err(|e| conflict_on_unique(e, "An environment with this name already exists"))?;
    Ok((StatusCode::CREATED, Json(row.into())))
}

pub async fn list_environments(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(project_id): Path<Uuid>,
) -> Result<Json<Vec<Environment>>, ApiError> {
    owned_project(&state.pool, user.id, project_id).await?;
    let rows: Vec<EnvironmentRow> = sqlx::query_as(&format!(
        "SELECT {ENV_COLS} FROM environments e WHERE e.project_id = $1 ORDER BY e.created_at, e.id"
    ))
    .bind(project_id)
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(rows.into_iter().map(Into::into).collect()))
}

pub async fn get_environment(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<Environment>, ApiError> {
    Ok(Json(
        owned_environment(&state.pool, user.id, id).await?.into(),
    ))
}

pub async fn rename_environment(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(id): Path<Uuid>,
    ApiJson(body): ApiJson<NameBody>,
) -> Result<Json<Environment>, ApiError> {
    let name = check_text("name", &body.name)?;
    owned_environment(&state.pool, user.id, id).await?;
    let row: EnvironmentRow = sqlx::query_as(
        "UPDATE environments SET name = $2 WHERE id = $1 \
         RETURNING id, project_id, name, created_at",
    )
    .bind(id)
    .bind(&name)
    .fetch_one(&state.pool)
    .await
    .map_err(|e| conflict_on_unique(e, "An environment with this name already exists"))?;
    Ok(Json(row.into()))
}

pub async fn delete_environment(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    owned_environment(&state.pool, user.id, id).await?;
    let services: Vec<Uuid> =
        sqlx::query_scalar("SELECT id FROM services WHERE environment_id = $1")
            .bind(id)
            .fetch_all(&state.pool)
            .await?;
    deployments::stop_services(&state, &services).await?;
    sqlx::query("DELETE FROM environments WHERE id = $1")
        .bind(id)
        .execute(&state.pool)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
