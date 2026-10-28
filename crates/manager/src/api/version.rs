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

/// Public plugin-marketplace linked from the Dashboard's YAML editor when
/// `marketplace_url` is not set in manager.toml.
pub const DEFAULT_MARKETPLACE_URL: &str = "https://plugins.buz.schiplat.com";

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

/// Clock-skew guard state: a Manager clock behind real time signs config URLs
/// that are already expired when a Runner downloads them.
///
/// `/health` is unauthenticated, so this deliberately exposes only the verdict
/// and the measured offset: the probe source URL (`https://{bucket}.{endpoint}/`)
/// and raw probe errors would disclose internal OSS endpoints and hostnames to
/// anonymous callers. Operators get the full detail from the Manager logs.
fn clock_health() -> serde_json::Value {
    let guard = crate::services::clock::global();
    let warn_threshold = guard.config().warn_threshold_seconds;
    let probe_failed = guard.last_error().is_some();

    match guard.latest() {
        Some(sample) => serde_json::json!({
            "status": if sample.is_skewed(warn_threshold) { "skewed" } else { "ok" },
            "skew_seconds": sample.skew_secs,
            "behind_seconds": sample.local_behind_secs(),
            "checked_at": sample.checked_at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            "probe_failed": probe_failed,
        }),
        None => serde_json::json!({
            "status": "unknown",
            "skew_seconds": serde_json::Value::Null,
            "behind_seconds": serde_json::Value::Null,
            "checked_at": serde_json::Value::Null,
            "probe_failed": probe_failed,
        }),
    }
}

/// Public health + version probe (no auth). Liveness semantics by default
/// (HTTP 200 while the process responds); pass `?readiness=true` for a
/// readiness probe that returns 503 when a dependency is unavailable.
///
/// The first endpoint annotated for the OpenAPI document served at
/// `/api/v1/openapi.json` — annotate further handlers with `#[utoipa::path]`
/// and register them in `api/openapi.rs`; the spec grows incrementally.
#[utoipa::path(
    get,
    path = "/health",
    tag = "system",
    responses(
        (status = 200, description = "Liveness probe with version and health details"),
        (status = 503, description = "Readiness probe failed (pass ?readiness=true)")
    )
)]
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
    // DB override (admin UI) wins over the manager.toml default.
    let marketplace_override = crate::services::platform_settings::effective(
        &state.pool,
        crate::services::platform_settings::KEY_MARKETPLACE_URL,
        &state.config.marketplace_url,
    )
    .await;
    let marketplace_url = if marketplace_override.is_empty() {
        DEFAULT_MARKETPLACE_URL
    } else {
        marketplace_override.as_str()
    };
    // Review-gate posture for the dashboard banner. Booleans only — the
    // manifest path stays private (same policy as the clock section: this
    // endpoint is unauthenticated).
    // "Configured" = the manifest path resolves to an EXISTING file — the
    // default now points beside the plugin directory, so a fresh deployment
    // (nothing cloned yet) stays honest about having no review ledger.
    let manifest_path = crate::services::platform_settings::effective(
        &state.pool,
        crate::services::platform_settings::KEY_PLUGIN_REVIEWED_MANIFEST,
        &state.config.plugin_index.reviewed_manifest,
    )
    .await;
    let manifest_configured =
        !manifest_path.trim().is_empty() && std::path::Path::new(manifest_path.trim()).exists();
    let plugin_review = serde_json::json!({
        "manifest_configured": manifest_configured,
        "require_reviewed": crate::services::platform_settings::effective(
            &state.pool,
            crate::services::platform_settings::KEY_PLUGIN_REVIEW_REQUIRED,
            &state.config.plugin_index.require_reviewed.to_string(),
        )
        .await
        == "true",
    });
    let body = Json(serde_json::json!({
        "status": status,
        "name": PRODUCT_NAME,
        "version": VERSION,
        "component": "manager",
        "marketplace_url": marketplace_url,
        "uptime_seconds": uptime_seconds,
        "started_at": started.map(|s| s.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
        "plugin_review": plugin_review,
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
        "clock": clock_health(),
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
