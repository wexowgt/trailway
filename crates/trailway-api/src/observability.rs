//! What services are doing: CPU and memory series, environment logs, and the
//! per-server credit breakdown the web shows next to the balance.

use axum::{
    extract::{Path, Query, State},
    Json,
};
use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;
use sqlx::PgPool;
use trailway_proto::{
    EnvironmentLogs, EnvironmentMetrics, LogChunk, MetricPoint, ServerLedger, ServiceSeries,
    VmMetric,
};
use uuid::Uuid;

use crate::{
    auth::AuthUser, error::ApiError, ledger::compute, projects::require_environment, AppState,
};

const MIB: u64 = 1024 * 1024;
/// How long CPU and memory samples are kept.
const METRICS_KEEP_DAYS: i64 = 8;
const DEFAULT_LOG_LIMIT: i64 = 200;
const MAX_LOG_LIMIT: i64 = 1000;

/// A chart's time range and the width of one point in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Range {
    pub span: Duration,
    pub bucket_secs: i64,
}

impl Range {
    pub fn parse(text: &str) -> Option<Self> {
        let (hours, bucket_secs) = match text {
            "1h" => (1, 30),
            "24h" => (24, 900),
            "7d" => (7 * 24, 3600),
            _ => return None,
        };
        Some(Self {
            span: Duration::hours(hours),
            bucket_secs,
        })
    }
}

/// Stores what the agent reported about its running VMs. Reports for
/// deployments of other servers, or without a service, are dropped.
pub async fn store_metrics(
    pool: &PgPool,
    server_id: Uuid,
    samples: &[VmMetric],
) -> Result<(), sqlx::Error> {
    for m in samples {
        let (Ok(cpu), Ok(memory)) = (
            i32::try_from(m.cpu_millicores),
            i64::try_from(m.memory_bytes),
        ) else {
            continue;
        };
        sqlx::query(
            "INSERT INTO service_metrics (service_id, deployment_id, cpu_millicores, memory_bytes) \
             SELECT service_id, id, $3, $4 FROM deployments \
             WHERE id = $1 AND server_id = $2 AND service_id IS NOT NULL",
        )
        .bind(m.deployment_id)
        .bind(server_id)
        .bind(cpu)
        .bind(memory)
        .execute(pool)
        .await?;
    }
    Ok(())
}

/// Drops metrics older than the longest range the charts offer.
pub async fn prune_metrics(pool: &PgPool) -> Result<u64, sqlx::Error> {
    let cutoff = Utc::now() - Duration::days(METRICS_KEEP_DAYS);
    Ok(sqlx::query("DELETE FROM service_metrics WHERE ts < $1")
        .bind(cutoff)
        .execute(pool)
        .await?
        .rows_affected())
}

#[derive(Deserialize)]
pub struct MetricsQuery {
    range: Option<String>,
}

#[derive(sqlx::FromRow)]
struct ServiceLimits {
    id: Uuid,
    name: String,
    vcpus: i32,
    memory_mib: i32,
}

#[derive(sqlx::FromRow)]
struct BucketRow {
    service_id: Uuid,
    bucket: DateTime<Utc>,
    cpu: f64,
    memory: f64,
}

/// `GET /environments/{id}/metrics?range=1h|24h|7d`: one series per service
/// (mean CPU and memory per bucket) with the limits the service is set to.
pub async fn environment_metrics(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(environment_id): Path<Uuid>,
    Query(q): Query<MetricsQuery>,
) -> Result<Json<EnvironmentMetrics>, ApiError> {
    let range = Range::parse(q.range.as_deref().unwrap_or("1h"))
        .ok_or_else(|| ApiError::validation("range must be 1h, 24h or 7d"))?;
    require_environment(&state.pool, user.id, environment_id).await?;
    let to = Utc::now();
    let from = to - range.span;

    let limits: Vec<ServiceLimits> = sqlx::query_as(
        "SELECT id, name, vcpus, memory_mib FROM services WHERE environment_id = $1 ORDER BY name",
    )
    .bind(environment_id)
    .fetch_all(&state.pool)
    .await?;
    let rows: Vec<BucketRow> = sqlx::query_as(
        "SELECT m.service_id, \
           to_timestamp(floor(extract(epoch FROM m.ts) / $3) * $3) AS bucket, \
           avg(m.cpu_millicores)::float8 AS cpu, avg(m.memory_bytes)::float8 AS memory \
         FROM service_metrics m JOIN services s ON s.id = m.service_id \
         WHERE s.environment_id = $1 AND m.ts >= $2 \
         GROUP BY 1, 2 ORDER BY 2",
    )
    .bind(environment_id)
    .bind(from)
    .bind(range.bucket_secs as f64)
    .fetch_all(&state.pool)
    .await?;

    let services = limits
        .into_iter()
        .map(|s| ServiceSeries {
            service_id: s.id,
            name: s.name,
            cpu_limit_millicores: s.vcpus.max(0) as u64 * 1000,
            memory_limit_bytes: s.memory_mib.max(0) as u64 * MIB,
            points: rows
                .iter()
                .filter(|r| r.service_id == s.id)
                .map(|r| MetricPoint {
                    ts: r.bucket,
                    cpu_millicores: r.cpu,
                    memory_bytes: r.memory,
                })
                .collect(),
        })
        .collect();
    Ok(Json(EnvironmentMetrics {
        from,
        to,
        bucket_secs: range.bucket_secs,
        services,
    }))
}

#[derive(Deserialize)]
pub struct LogsQuery {
    /// Only chunks after this cursor (the previous `next_after`), oldest first.
    after: Option<i64>,
    /// Only one service.
    service: Option<Uuid>,
    /// Only chunks written since then.
    since: Option<DateTime<Utc>>,
    limit: Option<i64>,
}

#[derive(sqlx::FromRow)]
struct ChunkRow {
    id: i64,
    created_at: DateTime<Utc>,
    service_id: Uuid,
    service: String,
    text: String,
}

/// `GET /environments/{id}/logs`: console output of the environment's services.
/// Without `after` it returns the newest `limit` chunks; with it, what came
/// after the cursor, so the web can tail by polling.
pub async fn environment_logs(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(environment_id): Path<Uuid>,
    Query(q): Query<LogsQuery>,
) -> Result<Json<EnvironmentLogs>, ApiError> {
    let limit = q.limit.unwrap_or(DEFAULT_LOG_LIMIT);
    if !(1..=MAX_LOG_LIMIT).contains(&limit) {
        return Err(ApiError::validation(format!(
            "limit must be between 1 and {MAX_LOG_LIMIT}"
        )));
    }
    require_environment(&state.pool, user.id, environment_id).await?;
    let tail = q.after.is_some();
    let mut rows: Vec<ChunkRow> = sqlx::query_as(&format!(
        "SELECT l.id, l.created_at, s.id AS service_id, s.name AS service, l.text \
         FROM deployment_logs l \
         JOIN deployments d ON d.id = l.deployment_id \
         JOIN services s ON s.id = d.service_id \
         WHERE s.environment_id = $1 AND l.id > $2 \
         AND ($3::uuid IS NULL OR s.id = $3) \
         AND ($4::timestamptz IS NULL OR l.created_at >= $4) \
         ORDER BY l.id {} LIMIT $5",
        if tail { "ASC" } else { "DESC" }
    ))
    .bind(environment_id)
    .bind(q.after.unwrap_or(0))
    .bind(q.service)
    .bind(q.since)
    .bind(limit)
    .fetch_all(&state.pool)
    .await?;
    if !tail {
        rows.reverse();
    }
    let next_after = rows.last().map_or(q.after.unwrap_or(0), |r| r.id);
    Ok(Json(EnvironmentLogs {
        chunks: rows
            .into_iter()
            .map(|r| LogChunk {
                id: r.id,
                ts: r.created_at,
                service_id: r.service_id,
                service: r.service,
                text: r.text,
            })
            .collect(),
        next_after,
    }))
}

#[derive(sqlx::FromRow)]
struct ServerSums {
    server_id: Option<Uuid>,
    hostname: Option<String>,
    kind: String,
    millicore_seconds: f64,
    byte_seconds: f64,
}

/// `GET /ledger/servers`: contributed, consumed and balance per server. Adds
/// up to the numbers of `/ledger/balance`; servers that were deleted keep
/// their history under no host name.
pub async fn ledger_servers(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
) -> Result<Json<Vec<ServerLedger>>, ApiError> {
    let rows: Vec<ServerSums> = sqlx::query_as(
        "SELECT e.server_id, s.hostname, e.kind, \
         COALESCE(SUM(e.millicore_seconds), 0)::float8 AS millicore_seconds, \
         COALESCE(SUM(e.byte_seconds), 0)::float8 AS byte_seconds \
         FROM ledger_entries e LEFT JOIN servers s ON s.id = e.server_id \
         WHERE e.user_id = $1 GROUP BY e.server_id, s.hostname, e.kind",
    )
    .bind(user.id)
    .fetch_all(&state.pool)
    .await?;
    let mut servers: Vec<ServerLedger> = vec![];
    for r in &rows {
        if !servers.iter().any(|s| s.server_id_opt() == r.server_id) {
            servers.push(ServerLedger {
                server_id: r.server_id.unwrap_or_default(),
                hostname: r.hostname.clone(),
                contributed: compute(0.0, 0.0),
                consumed: compute(0.0, 0.0),
                balance: compute(0.0, 0.0),
            });
        }
    }
    for s in &mut servers {
        let sum = |kind: &str| {
            rows.iter()
                .filter(|r| r.server_id == s.server_id_opt() && r.kind == kind)
                .fold((0.0, 0.0), |a, r| {
                    (a.0 + r.millicore_seconds, a.1 + r.byte_seconds)
                })
        };
        let (cm, cb) = sum("contributed");
        let (um, ub) = sum("consumed");
        s.contributed = compute(cm, cb);
        s.consumed = compute(um, ub);
        s.balance = compute(cm - um, cb - ub);
    }
    servers.sort_by(|a, b| {
        a.hostname
            .cmp(&b.hostname)
            .then(a.server_id.cmp(&b.server_id))
    });
    Ok(Json(servers))
}

trait ServerKey {
    fn server_id_opt(&self) -> Option<Uuid>;
}

impl ServerKey for ServerLedger {
    fn server_id_opt(&self) -> Option<Uuid> {
        (!self.server_id.is_nil()).then_some(self.server_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges_have_a_bounded_point_count() {
        for (text, hours) in [("1h", 1), ("24h", 24), ("7d", 168)] {
            let r = Range::parse(text).unwrap();
            assert_eq!(r.span, Duration::hours(hours));
            let points = r.span.num_seconds() / r.bucket_secs;
            assert!((90..=200).contains(&points), "{text}: {points}");
        }
        assert!(Range::parse("10d").is_none());
    }
}
