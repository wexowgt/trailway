//! The agent's WebSocket: the API pushes jobs, the agent reports status and logs.

use std::{collections::HashMap, time::Duration};

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    response::Response,
};
use sqlx::{types::Json as Jsonb, PgPool};
use trailway_proto::{
    AgentMessage, ApiMessage, DeployJob, DeploymentReport, DeploymentStatus, GitSource, VmSpec,
};
use uuid::Uuid;

use crate::{servers::ServerAuth, AppState};

const PING_EVERY: Duration = Duration::from_secs(20);

/// `GET /api/v1/agent/ws` with the server token.
pub async fn connect(
    State(state): State<AppState>,
    auth: ServerAuth,
    ws: WebSocketUpgrade,
) -> Response {
    ws.on_upgrade(move |socket| session(state, auth.server_id, socket))
}

async fn session(state: AppState, server_id: Uuid, mut socket: WebSocket) {
    let (conn, mut rx) = state.hub.connect(server_id);
    tracing::info!(%server_id, "agent connected");
    let mut ping = tokio::time::interval(PING_EVERY);
    loop {
        tokio::select! {
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Text(text))) => {
                    match serde_json::from_str::<AgentMessage>(text.as_str()) {
                        Ok(msg) => {
                            if let Err(e) = handle(&state, server_id, msg).await {
                                tracing::error!(%server_id, "agent message failed: {e}");
                            }
                        }
                        Err(e) => tracing::warn!(%server_id, "bad agent message: {e}"),
                    }
                }
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                Some(Ok(_)) => {}
            },
            outgoing = rx.recv() => match outgoing {
                Some(msg) => {
                    let Ok(json) = serde_json::to_string(&msg) else { continue };
                    if socket.send(Message::Text(json.into())).await.is_err() {
                        break;
                    }
                }
                // Replaced by a newer connection of the same server.
                None => break,
            },
            _ = ping.tick() => {
                if socket.send(Message::Ping(Vec::new().into())).await.is_err() {
                    break;
                }
            }
        }
    }
    state.hub.disconnect(server_id, conn);
    tracing::info!(%server_id, "agent disconnected");
}

async fn handle(state: &AppState, server_id: Uuid, msg: AgentMessage) -> Result<(), sqlx::Error> {
    match msg {
        AgentMessage::Hello { deployments } => reconcile(state, server_id, deployments).await,
        AgentMessage::Status(report) => apply_report(state, server_id, &report).await,
        AgentMessage::Logs {
            deployment_id,
            offset,
            len,
            text,
        } => store_logs(&state.pool, server_id, deployment_id, offset, len, &text).await,
    }
}

/// Records what the agent says about one deployment. A finished deployment
/// stays finished; a deployment that should be gone but still runs is stopped.
async fn apply_report(
    state: &AppState,
    server_id: Uuid,
    r: &DeploymentReport,
) -> Result<(), sqlx::Error> {
    let status = r.status.as_str();
    let updated: Option<(String, Option<Uuid>, Option<String>)> = sqlx::query_as(
        "UPDATE deployments SET status = $3, vm_id = COALESCE($4, vm_id), error = $6, \
         commit_sha = COALESCE($7, commit_sha), \
         host_port = CASE WHEN $3 IN ('failed', 'stopped') THEN NULL ELSE COALESCE($5, host_port) END, \
         updated_at = now() \
         WHERE id = $1 AND server_id = $2 AND status NOT IN ('failed', 'stopped') \
         RETURNING desired, service_id, vm_id",
    )
    .bind(r.deployment_id)
    .bind(server_id)
    .bind(status)
    .bind(&r.vm_id)
    .bind(r.host_port.map(i32::from))
    .bind(&r.error)
    .bind(&r.commit)
    .fetch_optional(&state.pool)
    .await?;
    let desired = updated.as_ref().map(|(d, _, _)| d.clone());
    if let Some((_, Some(service_id), None)) = &updated {
        // A build that never produced a VM must not take the running version down.
        if r.status == DeploymentStatus::Failed && desired.as_deref() == Some("running") {
            keep_previous(&state.pool, *service_id, r.deployment_id).await?;
        }
    }
    if desired.as_deref() == Some("stopped") && !r.status.is_terminal() {
        state.hub.send(
            server_id,
            ApiMessage::Stop {
                deployment_id: r.deployment_id,
            },
        );
    }
    Ok(())
}

/// A deployment failed before it started a VM: its predecessor, which the
/// deploy marked as unwanted but the agent has not stopped (it only does that
/// once the new image is ready), becomes wanted again, unless a newer
/// deployment or a stop has taken over since.
async fn keep_previous(pool: &PgPool, service_id: Uuid, failed: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE deployments SET desired = 'running', updated_at = now() WHERE id = ( \
           SELECT p.id FROM deployments p WHERE p.service_id = $1 AND p.id <> $2 \
           AND p.desired = 'stopped' AND p.status = 'running' \
           AND p.created_at < (SELECT created_at FROM deployments WHERE id = $2) \
           ORDER BY p.created_at DESC LIMIT 1) \
         AND NOT EXISTS (SELECT 1 FROM deployments n WHERE n.service_id = $1 AND n.id <> $2 \
           AND n.desired = 'running' AND n.status NOT IN ('failed', 'stopped'))",
    )
    .bind(service_id)
    .bind(failed)
    .execute(pool)
    .await?;
    Ok(())
}

/// Appends a log chunk, unless the API already has it (the agent resends from
/// the start after a reconnect, so only the chunk that continues the log counts).
async fn store_logs(
    pool: &PgPool,
    server_id: Uuid,
    deployment_id: Uuid,
    offset: u64,
    len: u64,
    text: &str,
) -> Result<(), sqlx::Error> {
    let (Ok(offset), Ok(len)) = (i64::try_from(offset), i64::try_from(len)) else {
        return Ok(());
    };
    let mut tx = pool.begin().await?;
    let advanced = sqlx::query(
        "UPDATE deployments SET log_bytes = $4 \
         WHERE id = $1 AND server_id = $2 AND log_bytes = $3",
    )
    .bind(deployment_id)
    .bind(server_id)
    .bind(offset)
    .bind(offset + len)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if advanced == 1 {
        sqlx::query("INSERT INTO deployment_logs (deployment_id, text) VALUES ($1, $2)")
            .bind(deployment_id)
            .bind(text.replace('\0', ""))
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await
}

type Pending = (
    Uuid,
    Option<Uuid>,
    String,
    String,
    Jsonb<VmSpec>,
    Option<Jsonb<GitSource>>,
    Option<String>,
);

/// Makes the database and the agent agree after a (re)connect: the agent's
/// report wins for what is running, the database decides what should be.
async fn reconcile(
    state: &AppState,
    server_id: Uuid,
    reports: Vec<DeploymentReport>,
) -> Result<(), sqlx::Error> {
    for r in &reports {
        apply_report(state, server_id, r).await?;
    }
    let reported: HashMap<Uuid, &DeploymentReport> =
        reports.iter().map(|r| (r.deployment_id, r)).collect();

    let rows: Vec<Pending> = sqlx::query_as(
        "SELECT id, service_id, status, desired, spec, source, domain FROM deployments \
         WHERE server_id = $1 AND status NOT IN ('failed', 'stopped')",
    )
    .bind(server_id)
    .fetch_all(&state.pool)
    .await?;
    for (id, service_id, status, desired, spec, source, domain) in &rows {
        let report = reported.get(id);
        if desired == "stopped" {
            match report {
                Some(r) if !r.status.is_terminal() => {
                    state
                        .hub
                        .send(server_id, ApiMessage::Stop { deployment_id: *id });
                }
                Some(_) => {}
                None => set_status(&state.pool, *id, DeploymentStatus::Stopped, None).await?,
            }
        } else if report.is_none() {
            if status == DeploymentStatus::Running.as_str() {
                let why = "The agent no longer has this VM (the server may have rebooted)";
                set_status(&state.pool, *id, DeploymentStatus::Failed, Some(why)).await?;
            } else {
                // Never started, or lost mid-way: hand it over again.
                set_status(&state.pool, *id, DeploymentStatus::Queued, None).await?;
                state.hub.send(
                    server_id,
                    ApiMessage::Deploy(DeployJob {
                        deployment_id: *id,
                        service_id: service_id.unwrap_or(*id),
                        spec: spec.0.clone(),
                        source: source.as_ref().map(|s| s.0.clone()),
                        domain: domain.clone(),
                    }),
                );
            }
        }
    }
    // VMs the API has no live record of at all: remove them.
    for r in &reports {
        if !r.status.is_terminal() && !rows.iter().any(|(id, ..)| *id == r.deployment_id) {
            state.hub.send(
                server_id,
                ApiMessage::Stop {
                    deployment_id: r.deployment_id,
                },
            );
        }
    }
    Ok(())
}

async fn set_status(
    pool: &PgPool,
    id: Uuid,
    status: DeploymentStatus,
    error: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE deployments SET status = $2, error = $3, updated_at = now(), \
         host_port = CASE WHEN $2 IN ('failed', 'stopped') THEN NULL ELSE host_port END \
         WHERE id = $1",
    )
    .bind(id)
    .bind(status.as_str())
    .bind(error)
    .execute(pool)
    .await?;
    Ok(())
}
