//! Aggregate Prometheus metrics — `GET /metrics`, no auth.
//!
//! Counts only: no team, pipeline, runner, or job identifiers ever appear,
//! matching the `/health` posture for unauthenticated endpoints. Operators
//! exposing the manager port beyond trusted networks should front this with
//! reverse-proxy auth like any other ops surface.
//!
//! Deliberately not in the OpenAPI document — this is an ops-plane scrape
//! target, not an API surface (same treatment as the sandboxd endpoints).

use axum::{extract::State, http::header::CONTENT_TYPE, response::IntoResponse};
use sqlx::Row;
use std::collections::HashMap;
use std::fmt::Write as _;

use crate::state::AppState;

/// Every job status the schema uses — zero-filled so dashboards never lose a
/// series when a status has no rows yet.
const JOB_STATUSES: &[&str] = &[
    "pending",
    "queued",
    "waiting_approval",
    "running",
    "success",
    "failed",
    "cancelled",
    "timeout",
];

const CONTENT_TYPE_PROM: &str = "text/plain; version=0.0.4; charset=utf-8";

/// Best-effort grouped count: query errors yield an empty map (zeros) with a
/// warning, so a transient DB hiccup degrades the scrape instead of failing it.
async fn job_status_counts(pool: &sqlx::SqlitePool) -> HashMap<String, i64> {
    let rows = sqlx::query("SELECT status, COUNT(*) AS cnt FROM jobs GROUP BY status")
        .fetch_all(pool)
        .await;
    match rows {
        Ok(rows) => rows
            .into_iter()
            .map(|r| (r.get::<String, _>(0), r.get::<i64, _>(1)))
            .collect(),
        Err(e) => {
            tracing::warn!(error = %e, "metrics: failed to sample job counts");
            HashMap::new()
        }
    }
}

/// Best-effort scalar count; `None` when the query fails (omit the line).
async fn scalar(pool: &sqlx::SqlitePool, sql: &str) -> Option<i64> {
    match sqlx::query_scalar(sql).fetch_one(pool).await {
        Ok(v) => Some(v),
        Err(e) => {
            tracing::warn!(error = %e, sql, "metrics: count query failed");
            None
        }
    }
}

pub async fn metrics(State(state): State<AppState>) -> impl IntoResponse {
    let mut out = String::new();

    // ── Jobs by status ──
    let counts = job_status_counts(&state.pool).await;
    let _ = writeln!(
        out,
        "# HELP buz_jobs Jobs by status (aggregate, no identifiers)."
    );
    let _ = writeln!(out, "# TYPE buz_jobs gauge");
    for status in JOB_STATUSES {
        let _ = writeln!(
            out,
            "buz_jobs{{status=\"{status}\"}} {}",
            counts.get(*status).copied().unwrap_or(0)
        );
    }

    // ── Runners ──
    let _ = writeln!(out, "# HELP buz_runners Registered runners (aggregate).");
    let _ = writeln!(out, "# TYPE buz_runners gauge");
    if let Some(total) = scalar(&state.pool, "SELECT COUNT(*) FROM runners").await {
        let _ = writeln!(out, "buz_runners{{state=\"total\"}} {total}");
    }
    if let Some(frozen) = scalar(
        &state.pool,
        "SELECT COUNT(*) FROM runners WHERE is_frozen != 0",
    )
    .await
    {
        let _ = writeln!(out, "buz_runners{{state=\"frozen\"}} {frozen}");
    }

    // ── Pipelines ──
    let _ = writeln!(out, "# HELP buz_pipelines Pipelines (aggregate).");
    let _ = writeln!(out, "# TYPE buz_pipelines gauge");
    if let Some(total) = scalar(&state.pool, "SELECT COUNT(*) FROM pipelines").await {
        let _ = writeln!(out, "buz_pipelines{{state=\"total\"}} {total}");
    }
    if let Some(active) = scalar(
        &state.pool,
        "SELECT COUNT(*) FROM pipelines WHERE is_active = 1",
    )
    .await
    {
        let _ = writeln!(out, "buz_pipelines{{state=\"active\"}} {active}");
    }

    // ── Process identity / uptime ──
    let _ = writeln!(out, "# HELP buz_build_info Build metadata, always 1.");
    let _ = writeln!(out, "# TYPE buz_build_info gauge");
    let _ = writeln!(
        out,
        "buz_build_info{{version=\"{}\"}} 1",
        shared::version::VERSION
    );
    let _ = writeln!(
        out,
        "# HELP buz_process_uptime_seconds Seconds since manager start."
    );
    let _ = writeln!(out, "# TYPE buz_process_uptime_seconds gauge");
    let _ = writeln!(
        out,
        "buz_process_uptime_seconds {}",
        crate::api::version::uptime_seconds()
    );

    ([(CONTENT_TYPE, CONTENT_TYPE_PROM)], out)
}
