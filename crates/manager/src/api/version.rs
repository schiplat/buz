use axum::{
    extract::{Query, State},
    http::StatusCode,
    Json,
};
use serde::Deserialize;
use shared::version::{PRODUCT_NAME, VERSION};
use std::sync::OnceLock;

use crate::state::AppState;
use crate::store::health::{db_probe, jobs_backlog};

/// Public plugin-marketplace linked from the Dashboard's YAML editor when
/// `marketplace_url` is not set in manager.toml.
pub const DEFAULT_MARKETPLACE_URL: &str = "https://plugins.buz.schiplat.com";

/// Wall-clock process start time, recorded once the manager boots.
static STARTED_AT: OnceLock<chrono::DateTime<chrono::Utc>> = OnceLock::new();

/// Manager control-plane identity, resolved once at startup by `main`
/// (`db::ensure_manager_instance_id`). A global rather than an `AppState`
/// field: it is immutable for the process lifetime and lets the response
/// header middleware read it without threading it through every extractor.
static MANAGER_INSTANCE_ID: OnceLock<String> = OnceLock::new();

/// Record the Manager instance identity — call from `main` right after it is
/// resolved. The first call wins; later calls are ignored.
pub fn set_instance_id(id: String) {
    let _ = MANAGER_INSTANCE_ID.set(id);
}

/// The Manager instance identity, or `None` before `main` sets it (tests).
pub fn instance_id() -> Option<&'static str> {
    MANAGER_INSTANCE_ID.get().map(String::as_str)
}

/// Record the process start time — call from `main` right after startup.
pub fn mark_started() {
    STARTED_AT.get_or_init(chrono::Utc::now);
}

fn started_at() -> Option<chrono::DateTime<chrono::Utc>> {
    STARTED_AT.get().copied()
}

/// Seconds since process start, or 0 before `mark_started` (tests). Exposed
/// for the `/metrics` scrape target.
pub fn uptime_seconds() -> u64 {
    started_at()
        .map(|s| (chrono::Utc::now() - s).num_seconds().max(0))
        .unwrap_or(0) as u64
}

#[derive(Debug, Deserialize, Default)]
pub struct HealthQuery {
    /// When true, a failing dependency (e.g. database) yields HTTP 503 instead
    /// of 200 — for readiness probes. Default is liveness semantics (always 200).
    #[serde(default)]
    pub readiness: Option<bool>,
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
    let latency = db_probe(&state.pool).await;
    let latency_ms = latency.as_ref().copied().unwrap_or(0.0);
    let db_ok = latency.is_ok();

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
    let uptime_secs = uptime_seconds();

    let status = if db_ok { "ok" } else { "degraded" };
    // DB override (admin UI) wins over the manager.toml default.
    let marketplace_override = crate::services::platform_settings::effective(
        &state.db(),
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
    // manifest path stays private (this endpoint is unauthenticated).
    // "Configured" = the manifest path resolves to an EXISTING file — the
    // default now points beside the plugin directory, so a fresh deployment
    // (nothing cloned yet) stays honest about having no review ledger.
    let manifest_path = crate::services::platform_settings::effective(
        &state.db(),
        crate::services::platform_settings::KEY_PLUGIN_REVIEWED_MANIFEST,
        &state.config.plugin_index.reviewed_manifest,
    )
    .await;
    let manifest_configured =
        !manifest_path.trim().is_empty() && std::path::Path::new(manifest_path.trim()).exists();
    let plugin_review = serde_json::json!({
        "manifest_configured": manifest_configured,
        "require_reviewed": crate::services::platform_settings::effective(
            &state.db(),
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
        "manager_instance_id": instance_id(),
        "marketplace_url": marketplace_url,
        "uptime_seconds": uptime_secs,
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

/// Public control-plane identity probe (no auth). Returns the persistent
/// `manager_instance_id` so Runners, sandboxd hosts, and operators can confirm
/// which control plane they are talking to. IDs are public identifiers.
#[utoipa::path(
    get,
    path = "/meta",
    tag = "system",
    responses((status = 200, description = "Control-plane identity"))
)]
pub async fn meta() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "name": PRODUCT_NAME,
        "version": VERSION,
        "component": "manager",
        "manager_instance_id": instance_id().unwrap_or("unknown"),
    }))
}
