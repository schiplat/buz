//! sandboxd-facing configuration endpoints: a long-poll fetch of this
//! instance's desired document and a status report for the applied version.
//!
//! Delivery model (design record: sandbox-host-configuration): sandboxd
//! pulls over HTTPS with its per-instance token; a `Prefer: wait=N` request
//! holds until the desired version changes or the deadline passes, giving
//! push-like latency over an outbound-only transport. Periodic polling is
//! the client-side fallback when long-polls fail.

use axum::{
    extract::{Query, State},
    http::HeaderMap,
    response::IntoResponse,
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use shared::error::AppError;
use shared::protocol::response::ApiResponse;
use std::time::Duration;

use crate::middleware::SandboxdClaims;
use crate::services::sandbox_config::SandboxHostDocument;
use crate::services::{sandbox_config as sc, sandbox_config_events};
use crate::state::AppState;

const MAX_WAIT_SECONDS: u64 = 45;

#[derive(Deserialize)]
pub struct ConfigQuery {
    #[serde(default)]
    pub applied_version: Option<i64>,
}

#[derive(Deserialize)]
pub struct StatusReport {
    /// The version sandboxd currently has applied (0 = none yet).
    #[serde(default)]
    pub applied_version: i64,
    /// SHA-256 of the applied document, when known.
    #[serde(default)]
    pub applied_sha256: String,
    /// Last fetch/apply error; empty = healthy.
    #[serde(default)]
    pub last_error: String,
    /// Whether the request is a long-poll (for status telemetry only).
    #[serde(default)]
    pub consecutive_failures: i64,
}

/// One fetch outcome for the response envelope.
struct FetchOutcome {
    desired_version: i64,
    document_json: Option<String>,
    document_sha256: String,
}

/// Read the current desired version + document for one host.
async fn read_desired(
    pool: &sqlx::SqlitePool,
    host_id: &str,
    instance_id: &str,
) -> Result<FetchOutcome, AppError> {
    let row: Option<(i64, Option<String>)> = sqlx::query_as(
        "SELECT h.desired_version, c.document
         FROM sandbox_hosts h
         LEFT JOIN sandbox_host_configs c
           ON c.host_id = h.host_id AND c.instance_id = h.instance_id
              AND c.version = h.desired_version
         WHERE h.host_id = ? AND h.instance_id = ?",
    )
    .bind(host_id)
    .bind(instance_id)
    .fetch_optional(pool)
    .await?;

    let Some((version, document)) = row else {
        return Err(AppError::NotFound("sandbox host not enrolled".into()));
    };
    let document_sha256 = document
        .as_deref()
        .map(|json| sc::sha256_hex(json.as_bytes()))
        .unwrap_or_default();
    Ok(FetchOutcome {
        desired_version: version,
        document_json: document,
        document_sha256,
    })
}

/// Database restore guard: if the host reports an applied version the current
/// database has never seen, the DB went back in time. Bump the desired version
/// past it (re-storing the last known document when available) and alert — the
/// host must never be told to apply an OLDER version than it runs.
async fn reconcile_db_restore(
    pool: &sqlx::SqlitePool,
    host_id: &str,
    instance_id: &str,
    applied_version: i64,
) -> Result<(), AppError> {
    if applied_version <= 0 {
        return Ok(());
    }
    let desired: i64 = sqlx::query_scalar(
        "SELECT desired_version FROM sandbox_hosts WHERE host_id = ? AND instance_id = ?",
    )
    .bind(host_id)
    .bind(instance_id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| AppError::NotFound("sandbox host not enrolled".into()))?;
    if desired >= applied_version {
        return Ok(());
    }

    // Take the highest stored document (any version) as the restoration body;
    // an empty database stores an empty document and the host keeps its cache
    // version number via the bump alone.
    let stored: Option<String> = sqlx::query_scalar(
        "SELECT document FROM sandbox_host_configs
         WHERE host_id = ? AND instance_id = ?
         ORDER BY version DESC LIMIT 1",
    )
    .bind(host_id)
    .bind(instance_id)
    .fetch_optional(pool)
    .await?
    .flatten();
    let restored: SandboxHostDocument = stored
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default();
    let (document, _) =
        sc::validate(&restored).map_err(|problems| AppError::Internal(problems.join("; ")))?;

    let version = applied_version + 1;
    let json = serde_json::to_string(&sc::canonical_json(&document))
        .map_err(|e| AppError::Internal(e.to_string()))?;
    let sha = sc::sha256_hex(json.as_bytes());
    sqlx::query(
        "INSERT OR IGNORE INTO sandbox_host_configs
         (host_id, instance_id, version, document, document_sha256, created_by)
         VALUES (?, ?, ?, ?, ?, 'db-restore-reconcile')",
    )
    .bind(host_id)
    .bind(instance_id)
    .bind(version)
    .bind(&json)
    .bind(&sha)
    .execute(pool)
    .await?;
    sqlx::query(
        "UPDATE sandbox_hosts SET desired_version = ?, applied_version = ?, updated_at = datetime('now')
         WHERE host_id = ? AND instance_id = ?",
    )
    .bind(version)
    .bind(applied_version)
    .bind(host_id)
    .bind(instance_id)
    .execute(pool)
    .await?;
    tracing::warn!(
        host = %host_id,
        instance = %instance_id,
        restored_to = applied_version,
        new_version = version,
        "database restored behind a host's applied version — bumped desired version and re-served the last stored document"
    );
    crate::audit::record(
        pool,
        crate::audit::AuditEvent::system(crate::audit::Action::SandboxHostConfigUpdate)
            .on(crate::audit::Resource::sandbox_host(host_id, instance_id))
            .after(Some(json!({
                "reason": "db_restore_reconcile",
                "reported_applied": applied_version,
                "bumped_to": version,
            }))),
    )
    .await;
    Ok(())
}

/// GET /sandboxd/config?applied_version=N — long-poll the desired document.
///
/// Returns immediately when the desired version differs from the reported
/// applied version (or no document exists yet); otherwise holds on the config
/// broadcast until a change, the wait deadline, or a lost/lagged subscription.
pub async fn get_config(
    claims: SandboxdClaims,
    State(state): State<AppState>,
    Query(q): Query<ConfigQuery>,
    headers: HeaderMap,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let wait = headers
        .get("prefer")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| {
            v.split(',')
                .map(str::trim)
                .find_map(|s| s.strip_prefix("wait="))
        })
        .and_then(|v| v.parse::<u64>().ok())
        .map(|s| Duration::from_secs(s.min(MAX_WAIT_SECONDS)))
        .unwrap_or_default();
    let host_id = claims.host_id.clone();
    let instance_id = claims.instance_id.clone();

    reconcile_db_restore(
        &state.pool,
        &host_id,
        &instance_id,
        q.applied_version.unwrap_or(0),
    )
    .await?;

    let deadline = tokio::time::Instant::now() + wait;
    let outcome = loop {
        // Subscribe BEFORE reading so a change committed between the read and
        // the await is buffered and cannot be missed.
        let mut events = sandbox_config_events::subscribe();
        let current = read_desired(&state.pool, &host_id, &instance_id).await?;
        // A first fetch (no applied_version) always returns the document;
        // otherwise return as soon as the desired version differs.
        let changed = q
            .applied_version
            .is_none_or(|applied| applied != current.desired_version);
        if changed {
            break current;
        }

        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break current;
        }
        match tokio::time::timeout(remaining, events.recv()).await {
            Ok(Ok(())) | Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => {}
            Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) | Err(_) => break current,
        }
    };

    // Record the fetch for ops visibility (best-effort).
    let _ = sqlx::query(
        "UPDATE sandbox_hosts SET last_fetch_at = datetime('now'),
         last_fetch_error = NULL, consecutive_fetch_failures = 0,
         status = CASE WHEN status = 'enrolled' THEN 'syncing' ELSE status END,
         updated_at = datetime('now')
         WHERE host_id = ? AND instance_id = ?",
    )
    .bind(&host_id)
    .bind(&instance_id)
    .execute(&state.pool)
    .await;

    let changed = q
        .applied_version
        .is_none_or(|applied| applied != outcome.desired_version);
    let body = json!({
        "manager_instance_id": crate::api::version::instance_id(),
        "host_id": host_id,
        "sandboxd_instance_id": instance_id,
        "version": outcome.desired_version,
        "changed": changed,
        "sha256": outcome.document_sha256,
        "config": if changed {
            outcome
                .document_json
                .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
                .unwrap_or(Value::Null)
        } else {
            Value::Null
        },
        "retry_after_ms": 0,
    });
    Ok(Json(ApiResponse::success(body)))
}

/// POST /sandboxd/status — the host reports what it has applied and any
/// fetch/apply error. Updates the ops-visible state and reconciles a restored
/// database exactly like the fetch path does.
pub async fn report_status(
    claims: SandboxdClaims,
    State(state): State<AppState>,
    Json(report): Json<StatusReport>,
) -> Result<impl IntoResponse, AppError> {
    let host_id = &claims.host_id;
    let instance_id = &claims.instance_id;
    reconcile_db_restore(&state.pool, host_id, instance_id, report.applied_version).await?;

    let error = report.last_error.trim().to_string();
    sqlx::query(
        "UPDATE sandbox_hosts SET
           applied_version = ?,
           applied_sha256 = ?,
           applied_at = CASE WHEN ? > 0 THEN datetime('now') ELSE applied_at END,
           last_fetch_at = datetime('now'),
           last_fetch_error = ?,
           consecutive_fetch_failures = ?,
           status = CASE
             WHEN ? > 0 AND desired_version = ? THEN 'insync'
             WHEN ? > 0 THEN 'pending'
             ELSE status END,
           updated_at = datetime('now')
         WHERE host_id = ? AND instance_id = ?",
    )
    .bind(report.applied_version)
    .bind(&report.applied_sha256)
    .bind(report.applied_version)
    .bind(if error.is_empty() {
        None
    } else {
        Some(error.as_str())
    })
    .bind(report.consecutive_failures)
    .bind(report.applied_version)
    .bind(report.applied_version)
    .bind(report.applied_version)
    .bind(host_id)
    .bind(instance_id)
    .execute(&state.pool)
    .await?;
    Ok(Json(ApiResponse::success(Value::Null)))
}
