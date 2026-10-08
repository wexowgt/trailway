//! Usage samples, the append-only ledger and the endpoints that read them.

use axum::{
    extract::{Path, Query, State},
    Json,
};
use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;
use trailway_proto::{
    Compute, LedgerBalance, LedgerEntries, LedgerEntry, LedgerKind, Resource, UsageAck,
    UsageOutcome, UsagePoint, UsageSample, MAX_SAMPLE_SECS, OFFLINE_AFTER_SECS,
};
use uuid::Uuid;

use crate::{
    auth::AuthUser,
    error::{ApiError, ApiJson},
    servers::{status_for, ServerAuth},
    AppState,
};

const BYTES_PER_GB: f64 = (1u64 << 30) as f64;
/// How far a sample's end may lie in the future (clock skew).
const FUTURE_SKEW_SECS: i64 = 5;
const DEFAULT_PAGE: i64 = 50;
const MAX_PAGE: i64 = 200;
const DEFAULT_SERIES_SECS: i64 = 3600;
const MAX_SERIES_POINTS: i64 = 5000;

/// Idle capacity of one sample in the two ledger units.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Credit {
    pub millicore_seconds: i64,
    pub byte_seconds: i128,
}

impl Credit {
    pub const ZERO: Self = Self {
        millicore_seconds: 0,
        byte_seconds: 0,
    };

    pub fn is_zero(&self) -> bool {
        *self == Self::ZERO
    }
}

/// What a sample earns. Nothing unless the server was online when the sample
/// arrived, has KVM, and the sample is fresh. Only idle capacity counts: what
/// the owner's own workloads used is never credited.
pub fn credit_for(sample: &UsageSample, online: bool, received_at: DateTime<Utc>) -> Credit {
    let fresh = received_at - sample.period_end <= Duration::seconds(OFFLINE_AFTER_SECS);
    if !online || !sample.kvm || !fresh {
        return Credit::ZERO;
    }
    let secs = (sample.period_end - sample.period_start).num_seconds();
    let idle = |r: Resource| r.total.saturating_sub(r.used);
    Credit {
        millicore_seconds: (idle(sample.cpu) as i64).saturating_mul(secs),
        byte_seconds: i128::from(idle(sample.memory)) * i128::from(secs),
    }
}

/// Half-open intervals `[a0, a1)` and `[b0, b1)` share time. Mirrors the SQL
/// predicate in `ingest_usage` that rejects duplicate and overlapping samples.
#[cfg(test)]
fn overlaps(a: (DateTime<Utc>, DateTime<Utc>), b: (DateTime<Utc>, DateTime<Utc>)) -> bool {
    a.0 < b.1 && b.0 < a.1
}

fn validate(sample: &UsageSample, now: DateTime<Utc>) -> Result<(), ApiError> {
    let secs = (sample.period_end - sample.period_start).num_seconds();
    if secs <= 0 || secs > MAX_SAMPLE_SECS {
        return Err(ApiError::validation(format!(
            "period must be between 1 and {MAX_SAMPLE_SECS} seconds"
        )));
    }
    if sample.period_end > now + Duration::seconds(FUTURE_SKEW_SECS) {
        return Err(ApiError::validation("period_end is in the future"));
    }
    for (name, r) in [("cpu", sample.cpu), ("memory", sample.memory)] {
        if r.used > r.total || i64::try_from(r.total).is_err() {
            return Err(ApiError::validation(format!("{name} is not valid")));
        }
    }
    Ok(())
}

pub async fn ingest_usage(
    State(state): State<AppState>,
    auth: ServerAuth,
    ApiJson(sample): ApiJson<UsageSample>,
) -> Result<Json<UsageAck>, ApiError> {
    let now = Utc::now();
    validate(&sample, now)?;

    let mut tx = state.pool.begin().await?;
    // The row lock serialises samples of one server, so the overlap check
    // below cannot race with a concurrent identical sample.
    let (user_id, last_heartbeat): (Uuid, Option<DateTime<Utc>>) =
        sqlx::query_as("SELECT user_id, last_heartbeat_at FROM servers WHERE id = $1 FOR UPDATE")
            .bind(auth.server_id)
            .fetch_one(&mut *tx)
            .await?;

    let overlapping: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM usage_samples \
         WHERE server_id = $1 AND period_start < $3 AND period_end > $2)",
    )
    .bind(auth.server_id)
    .bind(sample.period_start)
    .bind(sample.period_end)
    .fetch_one(&mut *tx)
    .await?;
    if overlapping {
        return Ok(Json(UsageAck {
            outcome: UsageOutcome::Duplicate,
        }));
    }

    let online = status_for(last_heartbeat, now) == trailway_proto::ServerStatus::Online;
    let credit = credit_for(&sample, online, now);
    let sample_id: Uuid = sqlx::query_scalar(
        "INSERT INTO usage_samples (server_id, user_id, period_start, period_end, cpu_total, \
         cpu_used, memory_total, memory_used, kvm, credited) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) RETURNING id",
    )
    .bind(auth.server_id)
    .bind(user_id)
    .bind(sample.period_start)
    .bind(sample.period_end)
    .bind(sample.cpu.total as i64)
    .bind(sample.cpu.used as i64)
    .bind(sample.memory.total as i64)
    .bind(sample.memory.used as i64)
    .bind(sample.kvm)
    .bind(!credit.is_zero())
    .fetch_one(&mut *tx)
    .await?;

    if !credit.is_zero() {
        sqlx::query(
            "INSERT INTO ledger_entries (user_id, server_id, sample_id, kind, period_start, \
             period_end, millicore_seconds, byte_seconds) \
             VALUES ($1, $2, $3, 'contributed', $4, $5, $6, $7::text::numeric)",
        )
        .bind(user_id)
        .bind(auth.server_id)
        .bind(sample_id)
        .bind(sample.period_start)
        .bind(sample.period_end)
        .bind(credit.millicore_seconds)
        .bind(credit.byte_seconds.to_string())
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;

    Ok(Json(UsageAck {
        outcome: if credit.is_zero() {
            UsageOutcome::Uncredited
        } else {
            UsageOutcome::Credited
        },
    }))
}

pub(crate) fn compute(millicore_seconds: f64, byte_seconds: f64) -> Compute {
    Compute {
        vcpu_seconds: millicore_seconds / 1000.0,
        gb_seconds: byte_seconds / BYTES_PER_GB,
    }
}

pub async fn balance(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
) -> Result<Json<LedgerBalance>, ApiError> {
    let rows: Vec<(String, f64, f64)> = sqlx::query_as(
        "SELECT kind, COALESCE(SUM(millicore_seconds), 0)::float8, \
         COALESCE(SUM(byte_seconds), 0)::float8 \
         FROM ledger_entries WHERE user_id = $1 GROUP BY kind",
    )
    .bind(user.id)
    .fetch_all(&state.pool)
    .await?;
    let sum = |kind: &str| {
        rows.iter()
            .find(|r| r.0 == kind)
            .map_or((0.0, 0.0), |r| (r.1, r.2))
    };
    let (cm, cb) = sum("contributed");
    let (um, ub) = sum("consumed");
    Ok(Json(LedgerBalance {
        contributed: compute(cm, cb),
        consumed: compute(um, ub),
        balance: compute(cm - um, cb - ub),
    }))
}

#[derive(Deserialize)]
pub struct EntriesQuery {
    limit: Option<i64>,
    /// Only entries with a `seq` below this one (the previous page's `next_before`).
    before: Option<i64>,
}

#[derive(sqlx::FromRow)]
struct EntryRow {
    seq: i64,
    kind: String,
    server_id: Option<Uuid>,
    period_start: DateTime<Utc>,
    period_end: DateTime<Utc>,
    millicore_seconds: i64,
    byte_seconds: f64,
    created_at: DateTime<Utc>,
}

pub async fn entries(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Query(q): Query<EntriesQuery>,
) -> Result<Json<LedgerEntries>, ApiError> {
    let limit = q.limit.unwrap_or(DEFAULT_PAGE);
    if !(1..=MAX_PAGE).contains(&limit) {
        return Err(ApiError::validation(format!(
            "limit must be between 1 and {MAX_PAGE}"
        )));
    }
    let mut rows = sqlx::query_as::<_, EntryRow>(
        "SELECT seq, kind, server_id, period_start, period_end, millicore_seconds, \
         byte_seconds::float8 AS byte_seconds, created_at FROM ledger_entries \
         WHERE user_id = $1 AND seq < $2 ORDER BY seq DESC LIMIT $3",
    )
    .bind(user.id)
    .bind(q.before.unwrap_or(i64::MAX))
    .bind(limit + 1)
    .fetch_all(&state.pool)
    .await?;
    let has_more = rows.len() as i64 > limit;
    rows.truncate(limit as usize);
    let next_before = if has_more {
        rows.last().map(|r| r.seq)
    } else {
        None
    };
    let entries = rows
        .into_iter()
        .map(|r| {
            let c = compute(r.millicore_seconds as f64, r.byte_seconds);
            LedgerEntry {
                seq: r.seq,
                kind: if r.kind == "consumed" {
                    LedgerKind::Consumed
                } else {
                    LedgerKind::Contributed
                },
                server_id: r.server_id,
                period_start: r.period_start,
                period_end: r.period_end,
                vcpu_seconds: c.vcpu_seconds,
                gb_seconds: c.gb_seconds,
                created_at: r.created_at,
            }
        })
        .collect();
    Ok(Json(LedgerEntries {
        entries,
        next_before,
    }))
}

#[derive(Deserialize)]
pub struct SeriesQuery {
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
    limit: Option<i64>,
}

#[derive(sqlx::FromRow)]
struct SampleRow {
    period_start: DateTime<Utc>,
    period_end: DateTime<Utc>,
    cpu_total: i64,
    cpu_used: i64,
    memory_total: i64,
    memory_used: i64,
    kvm: bool,
    credited: bool,
}

/// Time series of one of the caller's servers, oldest first, for charts.
pub async fn server_usage(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(server_id): Path<Uuid>,
    Query(q): Query<SeriesQuery>,
) -> Result<Json<Vec<UsagePoint>>, ApiError> {
    let to = q.to.unwrap_or_else(Utc::now);
    let from = q
        .from
        .unwrap_or_else(|| to - Duration::seconds(DEFAULT_SERIES_SECS));
    let limit = q.limit.unwrap_or(MAX_SERIES_POINTS);
    if from >= to || !(1..=MAX_SERIES_POINTS).contains(&limit) {
        return Err(ApiError::validation(format!(
            "from must be before to and limit between 1 and {MAX_SERIES_POINTS}"
        )));
    }
    let owned: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM servers WHERE id = $1 AND user_id = $2)")
            .bind(server_id)
            .bind(user.id)
            .fetch_one(&state.pool)
            .await?;
    if !owned {
        return Err(ApiError::NotFound);
    }
    // Newest `limit` points inside the window, returned oldest first.
    let mut rows = sqlx::query_as::<_, SampleRow>(
        "SELECT period_start, period_end, cpu_total, cpu_used, memory_total, memory_used, kvm, \
         credited FROM usage_samples WHERE server_id = $1 AND period_end > $2 AND period_end <= $3 \
         ORDER BY period_end DESC LIMIT $4",
    )
    .bind(server_id)
    .bind(from)
    .bind(to)
    .bind(limit)
    .fetch_all(&state.pool)
    .await?;
    rows.reverse();
    let res = |t: i64, u: i64| Resource {
        total: t.max(0) as u64,
        used: u.max(0) as u64,
    };
    Ok(Json(
        rows.into_iter()
            .map(|r| UsagePoint {
                period_start: r.period_start,
                period_end: r.period_end,
                cpu: res(r.cpu_total, r.cpu_used),
                memory: res(r.memory_total, r.memory_used),
                kvm: r.kvm,
                credited: r.credited,
            })
            .collect(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_800_000_000 + secs, 0).unwrap()
    }

    fn sample(start: i64, end: i64) -> UsageSample {
        UsageSample {
            period_start: at(start),
            period_end: at(end),
            cpu: Resource {
                total: 4000,
                used: 1000,
            },
            memory: Resource {
                total: 8 << 30,
                used: 2 << 30,
            },
            kvm: true,
        }
    }

    #[test]
    fn idle_capacity_is_credited_in_both_units() {
        let c = credit_for(&sample(0, 60), true, at(61));
        // 3 idle cores for 60 s, 6 GiB idle for 60 s.
        assert_eq!(c.millicore_seconds, 3000 * 60);
        assert_eq!(c.byte_seconds, i128::from(6u64 << 30) * 60);
    }

    #[test]
    fn offline_gives_no_credit() {
        assert!(credit_for(&sample(0, 60), false, at(61)).is_zero());
    }

    #[test]
    fn stale_sample_gives_no_credit() {
        assert!(credit_for(&sample(0, 60), true, at(60 + OFFLINE_AFTER_SECS + 1)).is_zero());
        assert!(!credit_for(&sample(0, 60), true, at(60 + OFFLINE_AFTER_SECS)).is_zero());
    }

    #[test]
    fn no_kvm_gives_no_credit() {
        let mut s = sample(0, 60);
        s.kvm = false;
        assert!(credit_for(&s, true, at(61)).is_zero());
    }

    #[test]
    fn own_usage_is_not_credited() {
        let mut s = sample(0, 60);
        s.cpu.used = s.cpu.total;
        s.memory.used = s.memory.total;
        assert!(credit_for(&s, true, at(61)).is_zero());
        // Half used: only the idle half counts.
        s.cpu.used = 2000;
        assert_eq!(credit_for(&s, true, at(61)).millicore_seconds, 2000 * 60);
    }

    #[test]
    fn overlap_detects_duplicates_but_not_neighbours() {
        let a = (at(0), at(60));
        assert!(overlaps(a, (at(0), at(60))), "same sample twice");
        assert!(overlaps(a, (at(30), at(90))), "partial overlap");
        assert!(overlaps(a, (at(10), at(20))), "contained");
        assert!(!overlaps(a, (at(60), at(120))), "adjacent after");
        assert!(!overlaps((at(60), at(120)), a), "adjacent before");
    }

    #[test]
    fn validation_bounds() {
        let now = at(100);
        assert!(validate(&sample(0, 60), now).is_ok());
        assert!(validate(&sample(60, 60), now).is_err(), "empty period");
        assert!(validate(&sample(0, 301), at(400)).is_err(), "too long");
        assert!(validate(&sample(90, 150), now).is_err(), "future");
        let mut s = sample(0, 60);
        s.cpu.used = s.cpu.total + 1;
        assert!(validate(&s, now).is_err());
    }
}
