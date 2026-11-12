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
use std::fmt::Write as _;

use crate::state::AppState;
use crate::store::exec::DbPool;
use crate::store::metrics::{entity_counts, job_status_counts};

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

pub async fn metrics(State(state): State<AppState>) -> impl IntoResponse {
    let mut out = String::new();

    // ── Jobs by status ──
    let counts = job_status_counts(&DbPool::from_sqlite(&state.pool)).await;
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

    // ── Runners & pipelines ──
    let counts = entity_counts(&DbPool::from_sqlite(&state.pool)).await;
    let _ = writeln!(out, "# HELP buz_runners Registered runners (aggregate).");
    let _ = writeln!(out, "# TYPE buz_runners gauge");
    if let Some(total) = counts.runners_total {
        let _ = writeln!(out, "buz_runners{{state=\"total\"}} {total}");
    }
    if let Some(frozen) = counts.runners_frozen {
        let _ = writeln!(out, "buz_runners{{state=\"frozen\"}} {frozen}");
    }
    let _ = writeln!(out, "# HELP buz_pipelines Pipelines (aggregate).");
    let _ = writeln!(out, "# TYPE buz_pipelines gauge");
    if let Some(total) = counts.pipelines_total {
        let _ = writeln!(out, "buz_pipelines{{state=\"total\"}} {total}");
    }
    if let Some(active) = counts.pipelines_active {
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
