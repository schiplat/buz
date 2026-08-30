use axum::{
    extract::{Query, State},
    http::StatusCode,
    Json,
};
use serde::Deserialize;
use shared::version::{PRODUCT_NAME, VERSION};
use sqlx::Row;
use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::Instant;

use crate::state::AppState;

/// Wall-clock process start time, recorded once the manager boots.
static STARTED_AT: OnceLock<chrono::DateTime<chrono::Utc>> = OnceLock::new();

/// Record the process start time — call from `main` right after startup.
pub fn mark_started() {
    STARTED_AT.get_or_init(chrono::Utc::now);
}

fn started_at() -> Option<chrono::DateTime<chrono::Utc>> {
    STARTED_AT.get().copied()
}

#[derive(Debug, Deserialize, Default)]
pub struct HealthQuery {
    /// When true, a failing dependency (e.g. database) yields HTTP 503 instead
    /// of 200 — for readiness probes. Default is liveness semantics (always 200).
    #[serde(default)]
    pub readiness: Option<bool>,
}

/// Snapshot of non-terminal job counts — an early warning for scheduling
/// backpressure or a stuck queue.
async fn jobs_backlog(pool: &sqlx::SqlitePool) -> serde_json::Value {
    let rows = sqlx::query(
        "SELECT status, COUNT(*) AS cnt FROM jobs
         WHERE status IN ('pending', 'running', 'queued', 'waiting_approval')
         GROUP BY status",
    )
    .fetch_all(pool)
    .await;

    let counts: HashMap<String, i64> = match rows {
        Ok(rows) => rows
            .into_iter()
            .map(|r| (r.get::<String, _>(0), r.get::<i64, _>(1)))
            .collect(),
        Err(e) => {
            tracing::warn!(error = %e, "health: failed to sample job backlog");
            HashMap::new()
        }
    };
    serde_json::json!({
        "pending": counts.get("pending").copied().unwrap_or(0),
        "running": counts.get("running").copied().unwrap_or(0),
        "queued": counts.get("queued").copied().unwrap_or(0),
        "waiting_approval": counts.get("waiting_approval").copied().unwrap_or(0),
    })
}

/// Public health + version probe (no auth). Liveness semantics by default
/// (HTTP 200 while the process responds); pass `?readiness=true` for a
/// readiness probe that returns 503 when a dependency is unavailable.
pub async fn health(
    State(state): State<AppState>,
    Query(q): Query<HealthQuery>,
) -> (StatusCode, Json<serde_json::Value>) {
    // ── Database probe (SELECT 1 + latency) ──
    let probe_start = Instant::now();
    let db_probe = sqlx::query_scalar::<_, i64>("SELECT 1")
        .fetch_one(&state.pool)
        .await;
    let latency_ms = probe_start.elapsed().as_secs_f64() * 1000.0;
    let db_ok = db_probe.is_ok();

    let (pool_max, pool_idle, pool_in_use) = if db_ok {
        let max = state.pool.size();
        let idle = state.pool.num_idle();
        (max, idle, max.saturating_sub(idle as u32))
    } else {
        (state.pool.size(), 0, 0)
    };

    // ── Job backlog (best-effort) ──
    let jobs = jobs_backlog(&state.pool).await;

    // ── WebSocket broadcast sinks ──
    let ws_job_sinks = state.log_broadcast.job_sink_count();
    let ws_pipeline_sinks = state.log_broadcast.pipeline_sink_count();

    let started = started_at();
    let uptime_seconds = started
        .map(|s| (chrono::Utc::now() - s).num_seconds().max(0))
        .unwrap_or(0);

    let status = if db_ok { "ok" } else { "degraded" };
    let body = Json(serde_json::json!({
        "status": status,
        "name": PRODUCT_NAME,
        "version": VERSION,
        "component": "manager",
        "uptime_seconds": uptime_seconds,
        "started_at": started.map(|s| s.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
        "database": {
            "status": if db_ok { "ok" } else { "error" },
            "latency_ms": latency_ms,
            "pool": {
                "max": pool_max,
                "idle": pool_idle,
                "in_use": pool_in_use,
            },
        },
        "jobs": jobs,
        "ws": {
            "job_sinks": ws_job_sinks,
            "pipeline_sinks": ws_pipeline_sinks,
        },
    }));

    let code = if q.readiness == Some(true) && !db_ok {
        StatusCode::SERVICE_UNAVAILABLE
    } else {
        StatusCode::OK
    };
    (code, body)
}

#[cfg(test)]
#[path = "tests/version_tests.rs"]
mod tests;
