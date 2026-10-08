//! Deploying a service to its server, stopping it, and reading deployments and logs.

use std::time::Duration;

use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use sqlx::{types::Json as Jsonb, PgPool};
use trailway_proto::{
    ApiMessage, DeployJob, Deployment, DeploymentStatus, GitSource, VmSpec, MILLICORES_PER_VCPU,
    OFFLINE_AFTER_SECS,
};
use uuid::Uuid;

use crate::{
    auth::AuthUser,
    error::ApiError,
    servers::status_for,
    services::{git_of, owned_service, ServiceRow},
    AppState,
};

const MIB: i64 = 1024 * 1024;
const LIST_LIMIT: i64 = 50;
const LOG_BATCH: i64 = 500;
const LOG_POLL: Duration = Duration::from_millis(500);

#[derive(sqlx::FromRow)]
struct DeploymentRow {
    id: Uuid,
    service_id: Option<Uuid>,
    server_id: Uuid,
    status: String,
    spec: Jsonb<VmSpec>,
    source: Option<Jsonb<GitSource>>,
    commit_sha: Option<String>,
    host_port: Option<i32>,
    error: Option<String>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl From<DeploymentRow> for Deployment {
    fn from(r: DeploymentRow) -> Self {
        Self {
            id: r.id,
            service_id: r.service_id,
            server_id: r.server_id,
            status: DeploymentStatus::parse(&r.status).unwrap_or(DeploymentStatus::Failed),
            image: r.spec.0.image,
            git: r.source.map(|s| s.0),
            commit: r.commit_sha,
            vcpus: r.spec.0.vcpus,
            memory_mib: r.spec.0.mem_mib,
            host_port: r.host_port.map(|p| p as u16),
            error: r.error,
            created_at: r.created_at,
            updated_at: r.updated_at,
        }
    }
}

const COLS: &str = "d.id, d.service_id, d.server_id, d.status, d.spec, d.source, d.commit_sha, d.host_port, d.error, \
                    d.created_at, d.updated_at";

async fn owned_deployment(pool: &PgPool, user: Uuid, id: Uuid) -> Result<DeploymentRow, ApiError> {
    sqlx::query_as(&format!(
        "SELECT {COLS} FROM deployments d JOIN servers s ON s.id = d.server_id \
         WHERE d.id = $1 AND s.user_id = $2"
    ))
    .bind(id)
    .bind(user)
    .fetch_optional(pool)
    .await?
    .ok_or(ApiError::NotFound)
}

/// True while the service has a deployment that should exist and is not over.
pub async fn has_active(pool: &PgPool, service_id: Uuid) -> Result<bool, ApiError> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM deployments WHERE service_id = $1 \
         AND desired = 'running' AND status NOT IN ('failed', 'stopped'))",
    )
    .bind(service_id)
    .fetch_one(pool)
    .await?)
}

/// Free resources, in the heartbeat's units (millicores and bytes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Free {
    pub cpu: i64,
    pub memory: i64,
}

/// Free capacity of a server: what the heartbeat says is unused, plus what the
/// service's own VM holds now (it is replaced), minus what other deployments
/// that have not started yet will take (the heartbeat cannot show them).
pub fn free_capacity(
    total: (i64, i64),
    used: (i64, i64),
    replaced: (i64, i64),
    pending: (i64, i64),
) -> Free {
    Free {
        cpu: total.0 - used.0 + replaced.0 - pending.0,
        memory: total.1 - used.1 + replaced.1 - pending.1,
    }
}

/// Refuses a deploy that does not fit, saying what is missing.
pub fn check_fit(free: Free, vcpus: u8, mem_mib: u32, hostname: &str) -> Result<(), ApiError> {
    let need_cpu = i64::from(vcpus) * MILLICORES_PER_VCPU as i64;
    let need_mem = i64::from(mem_mib) * MIB;
    if need_cpu > free.cpu {
        return Err(ApiError::Refused(
            "insufficient_capacity",
            format!(
                "Server {hostname} does not have enough free CPU: this service needs {vcpus} vCPU ({need_cpu} millicores), {} millicores are free",
                free.cpu.max(0)
            ),
        ));
    }
    if need_mem > free.memory {
        return Err(ApiError::Refused(
            "insufficient_capacity",
            format!(
                "Server {hostname} does not have enough free memory: this service needs {mem_mib} MiB, {} MiB are free",
                free.memory.max(0) / MIB
            ),
        ));
    }
    Ok(())
}

fn job_spec(service: &ServiceRow) -> VmSpec {
    let mut env: Vec<(String, String)> = service
        .env
        .0
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    // Apps built from source (Nixpacks) listen on $PORT.
    if let Some(port) = service.port.filter(|_| service.git_url.is_some()) {
        if !env.iter().any(|(k, _)| k == "PORT") {
            env.push(("PORT".into(), port.to_string()));
        }
    }
    VmSpec {
        // Built by the agent for a git service.
        image: service.image.clone().unwrap_or_default(),
        vcpus: service.vcpus as u8,
        mem_mib: service.memory_mib as u32,
        env,
        cmd: vec![],
        port: service.port.map(|p| p as u16),
        host_port: None,
    }
}

type ServerRow = (i64, i64, i64, i64, bool, Option<DateTime<Utc>>, String);

/// `POST /services/{id}/deploy`: queues a deployment on the service's server
/// and hands it to the agent. Replaces the service's running VM.
pub async fn deploy(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(service_id): Path<Uuid>,
) -> Result<(StatusCode, Json<Deployment>), ApiError> {
    let service = owned_service(&state.pool, user.id, service_id).await?;
    let spec = job_spec(&service);
    let source = git_of(service.git_url.clone(), service.git_branch.clone());
    let domain = service.domain(&state.config.domain_base);

    let mut tx = state.pool.begin().await?;
    // Serialises deploys per server, so two deploys cannot both claim the same free capacity.
    let server: ServerRow = sqlx::query_as(
        "SELECT cpu_total, cpu_used, memory_total, memory_used, kvm, last_heartbeat_at, hostname \
         FROM servers WHERE id = $1 FOR UPDATE",
    )
    .bind(service.server_id)
    .fetch_one(&mut *tx)
    .await?;
    let (cpu_total, cpu_used, mem_total, mem_used, kvm, heartbeat, hostname) = server;
    if status_for(heartbeat, Utc::now()) != trailway_proto::ServerStatus::Online {
        return Err(ApiError::Refused(
            "server_offline",
            format!("Server {hostname} is offline (no heartbeat for {OFFLINE_AFTER_SECS} s)"),
        ));
    }
    if !kvm {
        return Err(ApiError::Refused(
            "kvm_unavailable",
            format!("Server {hostname} has no /dev/kvm, so it cannot run microVMs"),
        ));
    }
    let (replaced_cpu, replaced_mem, pending_cpu, pending_mem): (i64, i64, i64, i64) =
        sqlx::query_as(
            "SELECT \
             COALESCE(SUM((spec->>'vcpus')::bigint * 1000) FILTER (WHERE service_id = $2 \
               AND status IN ('building', 'deploying', 'running')), 0)::bigint, \
             COALESCE(SUM((spec->>'mem_mib')::bigint * 1048576) FILTER (WHERE service_id = $2 \
               AND status IN ('building', 'deploying', 'running')), 0)::bigint, \
             COALESCE(SUM((spec->>'vcpus')::bigint * 1000) FILTER (WHERE service_id IS DISTINCT FROM $2 \
               AND status IN ('queued', 'building', 'deploying')), 0)::bigint, \
             COALESCE(SUM((spec->>'mem_mib')::bigint * 1048576) FILTER (WHERE service_id IS DISTINCT FROM $2 \
               AND status IN ('queued', 'building', 'deploying')), 0)::bigint \
             FROM deployments WHERE server_id = $1 AND desired = 'running'",
        )
        .bind(service.server_id)
        .bind(service_id)
        .fetch_one(&mut *tx)
        .await?;
    let free = free_capacity(
        (cpu_total, mem_total),
        (cpu_used, mem_used),
        (replaced_cpu, replaced_mem),
        (pending_cpu, pending_mem),
    );
    check_fit(free, spec.vcpus, spec.mem_mib, &hostname)?;

    let row: DeploymentRow = sqlx::query_as(&format!(
        "WITH d AS (INSERT INTO deployments (service_id, server_id, spec, source, domain) \
         VALUES ($1, $2, $3, $4, $5) RETURNING *) SELECT {COLS} FROM d"
    ))
    .bind(service_id)
    .bind(service.server_id)
    .bind(Jsonb(&spec))
    .bind(source.as_ref().map(Jsonb))
    .bind(&domain)
    .fetch_one(&mut *tx)
    .await?;
    // The agent stops the old VM when it starts this one; a queued old
    // deployment never reached the agent, so it just ends here.
    sqlx::query(
        "UPDATE deployments SET desired = 'stopped', updated_at = now(), \
         status = CASE WHEN status = 'queued' THEN 'stopped' ELSE status END \
         WHERE service_id = $1 AND id <> $2 AND desired = 'running'",
    )
    .bind(service_id)
    .bind(row.id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    // Not connected right now: it stays queued and goes out when the agent reconnects.
    state.hub.send(
        service.server_id,
        ApiMessage::Deploy(DeployJob {
            deployment_id: row.id,
            service_id,
            spec,
            source,
            domain,
        }),
    );
    Ok((StatusCode::ACCEPTED, Json(row.into())))
}

/// Marks every deployment of the services as unwanted and tells the agents.
/// Returns the ids it changed.
pub async fn stop_services(state: &AppState, service_ids: &[Uuid]) -> Result<Vec<Uuid>, ApiError> {
    let rows: Vec<(Uuid, Uuid, String)> = sqlx::query_as(
        "UPDATE deployments SET desired = 'stopped', updated_at = now(), \
         status = CASE WHEN status = 'queued' THEN 'stopped' ELSE status END \
         WHERE service_id = ANY($1) AND desired = 'running' RETURNING id, server_id, status",
    )
    .bind(service_ids)
    .fetch_all(&state.pool)
    .await?;
    for (id, server_id, status) in &rows {
        let over = DeploymentStatus::parse(status).is_none_or(DeploymentStatus::is_terminal);
        if !over {
            state
                .hub
                .send(*server_id, ApiMessage::Stop { deployment_id: *id });
        }
    }
    Ok(rows.into_iter().map(|(id, _, _)| id).collect())
}

/// `POST /services/{id}/stop`: removes the service's VM; the service stays.
pub async fn stop(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(service_id): Path<Uuid>,
) -> Result<(StatusCode, Json<Vec<Deployment>>), ApiError> {
    owned_service(&state.pool, user.id, service_id).await?;
    let ids = stop_services(&state, &[service_id]).await?;
    let rows: Vec<DeploymentRow> = sqlx::query_as(&format!(
        "SELECT {COLS} FROM deployments d WHERE d.id = ANY($1) ORDER BY d.created_at DESC"
    ))
    .bind(&ids)
    .fetch_all(&state.pool)
    .await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(rows.into_iter().map(Into::into).collect()),
    ))
}

pub async fn list(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(service_id): Path<Uuid>,
) -> Result<Json<Vec<Deployment>>, ApiError> {
    owned_service(&state.pool, user.id, service_id).await?;
    let rows: Vec<DeploymentRow> = sqlx::query_as(&format!(
        "SELECT {COLS} FROM deployments d WHERE d.service_id = $1 \
         ORDER BY d.created_at DESC, d.id LIMIT {LIST_LIMIT}"
    ))
    .bind(service_id)
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(rows.into_iter().map(Into::into).collect()))
}

pub async fn get(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<Deployment>, ApiError> {
    Ok(Json(
        owned_deployment(&state.pool, user.id, id).await?.into(),
    ))
}

#[derive(Deserialize)]
pub struct LogsQuery {
    /// Keep the response open and stream new output until the deployment ends.
    #[serde(default)]
    follow: bool,
}

struct LogCursor {
    pool: PgPool,
    deployment_id: Uuid,
    after: i64,
    follow: bool,
    done: bool,
}

/// Next chunk of log text: what is stored after the cursor, else (when
/// following) waits for more until the deployment is over.
async fn next_chunk(mut c: LogCursor) -> Option<(Result<String, std::io::Error>, LogCursor)> {
    if c.done {
        return None;
    }
    loop {
        let io = |e: sqlx::Error| std::io::Error::other(e.to_string());
        // Read the status first: if it was over before the logs query, that query saw everything.
        let status: Option<String> =
            match sqlx::query_scalar("SELECT status FROM deployments WHERE id = $1")
                .bind(c.deployment_id)
                .fetch_optional(&c.pool)
                .await
            {
                Ok(s) => s,
                Err(e) => {
                    c.done = true;
                    return Some((Err(io(e)), c));
                }
            };
        let rows: Vec<(i64, String)> = match sqlx::query_as(
            "SELECT id, text FROM deployment_logs WHERE deployment_id = $1 AND id > $2 \
             ORDER BY id LIMIT $3",
        )
        .bind(c.deployment_id)
        .bind(c.after)
        .bind(LOG_BATCH)
        .fetch_all(&c.pool)
        .await
        {
            Ok(r) => r,
            Err(e) => {
                c.done = true;
                return Some((Err(io(e)), c));
            }
        };
        if let Some((last, _)) = rows.last() {
            c.after = *last;
            let text = rows.into_iter().map(|(_, t)| t).collect::<String>();
            return Some((Ok(text), c));
        }
        let over = status
            .as_deref()
            .and_then(DeploymentStatus::parse)
            .is_none_or(DeploymentStatus::is_terminal);
        if !c.follow || over {
            return None;
        }
        tokio::time::sleep(LOG_POLL).await;
    }
}

/// `GET /deployments/{id}/logs[?follow=true]`: the app's console output as
/// plain text; with `follow` the response stays open until the deployment ends.
pub async fn logs(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(id): Path<Uuid>,
    Query(q): Query<LogsQuery>,
) -> Result<Response, ApiError> {
    owned_deployment(&state.pool, user.id, id).await?;
    let cursor = LogCursor {
        pool: state.pool.clone(),
        deployment_id: id,
        after: 0,
        follow: q.follow,
        done: false,
    };
    let stream = futures_util::stream::unfold(cursor, next_chunk);
    Ok((
        [
            (header::CONTENT_TYPE, "text/plain; charset=utf-8"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        Body::from_stream(stream),
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: i64 = 1024 * MIB;

    #[test]
    fn free_capacity_counts_replaced_and_pending() {
        let f = free_capacity((4000, 8 * GIB), (1000, 2 * GIB), (500, GIB), (250, GIB / 2));
        assert_eq!(
            f,
            Free {
                cpu: 3250,
                memory: 6 * GIB + GIB / 2
            }
        );
    }

    #[test]
    fn fits_or_says_what_is_missing() {
        let free = Free {
            cpu: 2000,
            memory: GIB,
        };
        assert!(check_fit(free, 2, 1024, "box").is_ok());
        let err = check_fit(free, 3, 128, "box").unwrap_err();
        assert!(
            matches!(&err, ApiError::Refused("insufficient_capacity", m) if m.contains("CPU") && m.contains("2000"))
        );
        let err = check_fit(free, 1, 2048, "box").unwrap_err();
        assert!(
            matches!(&err, ApiError::Refused("insufficient_capacity", m) if m.contains("memory") && m.contains("1024 MiB are free"))
        );
    }
}
