//! Services: the image, size, env and target server of something to deploy.

use std::collections::BTreeMap;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use chrono::{DateTime, Utc};
use sqlx::{types::Json as Jsonb, PgPool};
use trailway_proto::{CreateService, Service, UpdateService};
use uuid::Uuid;

use crate::{
    auth::AuthUser,
    deployments, domains,
    error::{conflict_on_unique, ApiError, ApiJson},
    projects::require_environment,
    servers::check_text,
    AppState,
};

pub const DEFAULT_VCPUS: u8 = 1;
pub const DEFAULT_MEMORY_MIB: u32 = 256;
pub const MAX_VCPUS: u8 = 32;
pub const MIN_MEMORY_MIB: u32 = 64;
pub const MAX_MEMORY_MIB: u32 = 262_144;
const MAX_ENV_VARS: usize = 100;
const MAX_ENV_VALUE_LEN: usize = 8192;

#[derive(sqlx::FromRow)]
pub struct ServiceRow {
    pub id: Uuid,
    pub environment_id: Uuid,
    pub name: String,
    pub image: String,
    pub vcpus: i32,
    pub memory_mib: i32,
    pub env: Jsonb<BTreeMap<String, String>>,
    pub port: Option<i32>,
    pub server_id: Uuid,
    pub host_label: String,
    pub public_ip: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl ServiceRow {
    /// The public host name, once the service has a port and the server's IP is known.
    pub fn domain(&self, domain_base: &str) -> Option<String> {
        self.port?;
        domains::domain(&self.host_label, domain_base, self.public_ip.as_deref())
    }

    pub fn into_service(self, domain_base: &str) -> Service {
        let url = self.domain(domain_base).map(|d| format!("https://{d}"));
        Service { url, ..self.into() }
    }
}

impl From<ServiceRow> for Service {
    fn from(r: ServiceRow) -> Self {
        Self {
            id: r.id,
            environment_id: r.environment_id,
            name: r.name,
            image: r.image,
            vcpus: r.vcpus as u8,
            memory_mib: r.memory_mib as u32,
            env: r.env.0,
            port: r.port.map(|p| p as u16),
            server_id: r.server_id,
            url: None,
            created_at: r.created_at,
            updated_at: r.updated_at,
        }
    }
}

const COLS: &str = "s.id, s.environment_id, s.name, s.image, s.vcpus, s.memory_mib, s.env, \
                    s.port, s.server_id, s.host_label, \
                    (SELECT public_ip FROM servers WHERE id = s.server_id) AS public_ip, \
                    s.created_at, s.updated_at";

/// Loads a service the user owns (through its environment and project).
pub async fn owned_service(pool: &PgPool, user: Uuid, id: Uuid) -> Result<ServiceRow, ApiError> {
    sqlx::query_as(&format!(
        "SELECT {COLS} FROM services s \
         JOIN environments e ON e.id = s.environment_id \
         JOIN projects p ON p.id = e.project_id \
         WHERE s.id = $1 AND p.user_id = $2"
    ))
    .bind(id)
    .bind(user)
    .fetch_optional(pool)
    .await?
    .ok_or(ApiError::NotFound)
}

fn check_image(image: &str) -> Result<String, ApiError> {
    let image = image.trim();
    if image.is_empty()
        || image.len() > 255
        || image.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return Err(ApiError::validation("image is not valid"));
    }
    Ok(image.to_string())
}

fn check_vcpus(vcpus: u8) -> Result<i32, ApiError> {
    if vcpus == 0 || vcpus > MAX_VCPUS {
        return Err(ApiError::validation(format!(
            "vcpus must be between 1 and {MAX_VCPUS}"
        )));
    }
    Ok(i32::from(vcpus))
}

fn check_memory(mib: u32) -> Result<i32, ApiError> {
    if !(MIN_MEMORY_MIB..=MAX_MEMORY_MIB).contains(&mib) {
        return Err(ApiError::validation(format!(
            "memory_mib must be between {MIN_MEMORY_MIB} and {MAX_MEMORY_MIB}"
        )));
    }
    Ok(mib as i32)
}

fn check_port(port: u16) -> Result<i32, ApiError> {
    if port == 0 {
        return Err(ApiError::validation("port must be between 1 and 65535"));
    }
    Ok(i32::from(port))
}

fn check_env(env: &BTreeMap<String, String>) -> Result<(), ApiError> {
    if env.len() > MAX_ENV_VARS {
        return Err(ApiError::validation(format!(
            "at most {MAX_ENV_VARS} env vars are allowed"
        )));
    }
    for (k, v) in env {
        if k.is_empty() || k.contains(['=', '\0']) || k.chars().any(char::is_whitespace) {
            return Err(ApiError::validation(format!("invalid env var name {k:?}")));
        }
        if v.len() > MAX_ENV_VALUE_LEN || v.contains('\0') {
            return Err(ApiError::validation(format!(
                "invalid value for env var {k}"
            )));
        }
    }
    Ok(())
}

/// The server must exist and belong to the user.
async fn check_server(pool: &PgPool, user: Uuid, server_id: Uuid) -> Result<(), ApiError> {
    let found: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM servers WHERE id = $1 AND user_id = $2")
            .bind(server_id)
            .bind(user)
            .fetch_optional(pool)
            .await?;
    found
        .map(|_| ())
        .ok_or_else(|| ApiError::validation("server_id is not one of your servers"))
}

/// The label a service gets on a server: `<service>-<environment>`, made
/// unique among the server's services.
async fn free_label(
    pool: &PgPool,
    server_id: Uuid,
    service_id: Uuid,
    service: &str,
    environment: &str,
) -> Result<String, ApiError> {
    let base = domains::base_label(service, environment);
    let taken: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM services WHERE server_id = $1 AND host_label = $2 AND id <> $3)",
    )
    .bind(server_id)
    .bind(&base)
    .bind(service_id)
    .fetch_one(pool)
    .await?;
    Ok(domains::unique_label(base, taken, service_id))
}

const NAME_TAKEN: &str = "A service with this name already exists in the environment";

pub async fn create(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(environment_id): Path<Uuid>,
    ApiJson(body): ApiJson<CreateService>,
) -> Result<(StatusCode, Json<Service>), ApiError> {
    let name = check_text("name", &body.name)?;
    let image = check_image(&body.image)?;
    let vcpus = check_vcpus(body.vcpus.unwrap_or(DEFAULT_VCPUS))?;
    let memory = check_memory(body.memory_mib.unwrap_or(DEFAULT_MEMORY_MIB))?;
    let port = body.port.map(check_port).transpose()?;
    check_env(&body.env)?;
    let environment = require_environment(&state.pool, user.id, environment_id).await?;
    check_server(&state.pool, user.id, body.server_id).await?;

    let id = Uuid::new_v4();
    let label = free_label(&state.pool, body.server_id, id, &name, &environment).await?;
    sqlx::query(
        "INSERT INTO services (id, environment_id, name, image, vcpus, memory_mib, env, port, \
         server_id, host_label) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
    )
    .bind(id)
    .bind(environment_id)
    .bind(&name)
    .bind(&image)
    .bind(vcpus)
    .bind(memory)
    .bind(Jsonb(&body.env))
    .bind(port)
    .bind(body.server_id)
    .bind(&label)
    .execute(&state.pool)
    .await
    .map_err(|e| conflict_on_unique(e, NAME_TAKEN))?;
    let row = owned_service(&state.pool, user.id, id).await?;
    Ok((
        StatusCode::CREATED,
        Json(row.into_service(&state.config.domain_base)),
    ))
}

pub async fn list(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(environment_id): Path<Uuid>,
) -> Result<Json<Vec<Service>>, ApiError> {
    require_environment(&state.pool, user.id, environment_id).await?;
    let rows: Vec<ServiceRow> = sqlx::query_as(&format!(
        "SELECT {COLS} FROM services s WHERE s.environment_id = $1 ORDER BY s.created_at, s.id"
    ))
    .bind(environment_id)
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(
        rows.into_iter()
            .map(|r| r.into_service(&state.config.domain_base))
            .collect(),
    ))
}

pub async fn get(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<Service>, ApiError> {
    let row = owned_service(&state.pool, user.id, id).await?;
    Ok(Json(row.into_service(&state.config.domain_base)))
}

/// Changes the config of a service. Running VMs keep their old config until
/// the next deploy.
pub async fn update(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(id): Path<Uuid>,
    ApiJson(body): ApiJson<UpdateService>,
) -> Result<Json<Service>, ApiError> {
    let current = owned_service(&state.pool, user.id, id).await?;
    let name = body
        .name
        .as_deref()
        .map(|n| check_text("name", n))
        .transpose()?;
    let image = body.image.as_deref().map(check_image).transpose()?;
    let vcpus = body.vcpus.map(check_vcpus).transpose()?;
    let memory = body.memory_mib.map(check_memory).transpose()?;
    let port = body.port.map(check_port).transpose()?;
    if let Some(env) = &body.env {
        check_env(env)?;
    }
    // A move to another server gets a label that is free there; otherwise the URL stays.
    let mut label = None;
    if let Some(server_id) = body.server_id {
        check_server(&state.pool, user.id, server_id).await?;
        if server_id != current.server_id {
            if deployments::has_active(&state.pool, id).await? {
                return Err(ApiError::Conflict(
                    "Stop the service before moving it to another server".into(),
                ));
            }
            let base = current
                .host_label
                .strip_suffix(&format!("-{}", &id.simple().to_string()[..6]))
                .unwrap_or(&current.host_label)
                .to_string();
            let taken: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM services WHERE server_id = $1 AND host_label = $2)",
            )
            .bind(server_id)
            .bind(&base)
            .fetch_one(&state.pool)
            .await?;
            label = Some(domains::unique_label(base, taken, id));
        }
    }

    let id: Uuid = sqlx::query_scalar(
        "UPDATE services SET name = COALESCE($2, name), image = COALESCE($3, image), \
         vcpus = COALESCE($4, vcpus), memory_mib = COALESCE($5, memory_mib), \
         env = COALESCE($6, env), port = COALESCE($7, port), \
         server_id = COALESCE($8, server_id), host_label = COALESCE($9, host_label), \
         updated_at = now() WHERE id = $1 RETURNING id",
    )
    .bind(id)
    .bind(name)
    .bind(image)
    .bind(vcpus)
    .bind(memory)
    .bind(body.env.as_ref().map(Jsonb))
    .bind(port)
    .bind(body.server_id)
    .bind(label)
    .fetch_one(&state.pool)
    .await
    .map_err(|e| conflict_on_unique(e, NAME_TAKEN))?;
    let row = owned_service(&state.pool, user.id, id).await?;
    Ok(Json(row.into_service(&state.config.domain_base)))
}

/// Deleting a service stops its VM first (once the agent is reachable).
pub async fn delete(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    owned_service(&state.pool, user.id, id).await?;
    deployments::stop_services(&state, &[id]).await?;
    sqlx::query("DELETE FROM services WHERE id = $1")
        .bind(id)
        .execute(&state.pool)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_limits() {
        assert!(check_vcpus(0).is_err());
        assert!(check_vcpus(33).is_err());
        assert_eq!(check_vcpus(2).unwrap(), 2);
        assert!(check_memory(63).is_err());
        assert_eq!(check_memory(64).unwrap(), 64);
        assert!(check_port(0).is_err());
        assert!(check_image("a b").is_err());
        assert_eq!(check_image(" nginx:1 ").unwrap(), "nginx:1");
    }

    #[test]
    fn validates_env() {
        let env = |k: &str, v: &str| BTreeMap::from([(k.to_string(), v.to_string())]);
        assert!(check_env(&env("A", "b c")).is_ok());
        assert!(check_env(&env("", "x")).is_err());
        assert!(check_env(&env("A=B", "x")).is_err());
        assert!(check_env(&env("A B", "x")).is_err());
        assert!(check_env(&env("A", "x\0")).is_err());
    }
}
