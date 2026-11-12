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
    state: &AppState,
    host_id: &str,
    instance_id: &str,
) -> Result<FetchOutcome, AppError> {
    let row = crate::store::sandboxd_config::desired_row(&state.db(), host_id, instance_id).await?;

    let Some((version, document)) = row else {
        return Err(AppError::NotFound("sandbox host not enrolled".into()));
    };
    // Nothing published yet: there is no document to deliver, and defaults
    // alone cannot form one (a document must define the 'default' profile).
    if document.is_none() {
        return Ok(FetchOutcome {
            desired_version: version,
            document_json: None,
            document_sha256: String::new(),
        });
    }
    // Tiers the host leaves undefined inherit the platform defaults; what is
    // delivered here is the effective document, and the sha256 covers exactly
    // those bytes.
    let tier_defaults = crate::services::sandbox_defaults::load(&state.pool).await?;
    let profile_defaults =
        crate::services::sandbox_defaults::load_default_profiles(&state.pool).await?;
    let network_default =
        crate::services::sandbox_defaults::load_default_network(&state.pool).await?;
    let host_document: SandboxHostDocument = document
        .as_deref()
        .and_then(|raw| serde_json::from_str(raw).ok())
        .unwrap_or_default();
    let effective = crate::services::sandbox_defaults::effective_document(
        &host_document,
        &tier_defaults,
        &profile_defaults,
        &network_default,
    )
    .to_string();
    let document_sha256 = sc::sha256_hex(effective.as_bytes());
    Ok(FetchOutcome {
        desired_version: version,
        document_json: Some(effective),
        document_sha256,
    })
}

/// Database restore guard: if the host reports an applied version the current
/// database has never seen, the DB went back in time. Bump the desired version
/// past it (re-storing the last known document when available) and alert — the
/// host must never be told to apply an OLDER version than it runs.
async fn reconcile_db_restore(
    state: &AppState,
    host_id: &str,
    instance_id: &str,
    applied_version: i64,
) -> Result<(), AppError> {
    if applied_version <= 0 {
        return Ok(());
    }
    let desired = crate::store::sandboxd_config::desired_version(&state.db(), host_id, instance_id)
        .await?
        .ok_or_else(|| AppError::NotFound("sandbox host not enrolled".into()))?;
    if desired >= applied_version {
        return Ok(());
    }

    // Take the highest stored document (any version) as the restoration body;
    // an empty database stores an empty document and the host keeps its cache
    // version number via the bump alone.
    let stored =
        crate::store::sandboxd_config::latest_config_document(&state.db(), host_id, instance_id)
            .await?;
    let restored: SandboxHostDocument = stored
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default();
    // An empty document (nothing stored at all) is passed through as-is: the
    // host keeps its cache version number via the bump alone. Anything else
    // must still be a valid document.
    let document = if restored.profiles.is_empty() && restored.tiers.is_empty() {
        restored
    } else {
        sc::validate(&restored)
            .map_err(|problems| AppError::Internal(problems.join("; ")))?
            .0
    };

    let version = applied_version + 1;
    let json = serde_json::to_string(&sc::canonical_json(&document))
        .map_err(|e| AppError::Internal(e.to_string()))?;
    let sha = sc::sha256_hex(json.as_bytes());
    crate::store::sandboxd_config::insert_restore_config(
        &state.db(),
        host_id,
        instance_id,
        version,
        &json,
        &sha,
    )
    .await?;
    crate::store::sandboxd_config::bump_versions_for_restore(
        &state.db(),
        host_id,
        instance_id,
        version,
        applied_version,
    )
    .await?;
    tracing::warn!(
        host = %host_id,
        instance = %instance_id,
        restored_to = applied_version,
        new_version = version,
        "database restored behind a host's applied version — bumped desired version and re-served the last stored document"
    );
    crate::audit::record(
        &state.pool,
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
        &state,
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
        let current = read_desired(&state, &host_id, &instance_id).await?;
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
    let _ =
        crate::store::sandboxd_config::stamp_fetch_ok(&state.db(), &host_id, &instance_id).await;

    let changed = q
        .applied_version
        .is_none_or(|applied| applied != outcome.desired_version);
    tracing::debug!(
        host = %host_id,
        instance = %instance_id,
        applied = ?q.applied_version,
        desired = outcome.desired_version,
        changed,
        "sandboxd config fetch served"
    );
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
/// database exactly like the fetch path does. `applied_at` is the push date
/// of the currently applied version: it only advances when the reported
/// version differs from the stored one, so heartbeat reports never overwrite
/// when that version landed.
pub async fn report_status(
    claims: SandboxdClaims,
    State(state): State<AppState>,
    Json(report): Json<StatusReport>,
) -> Result<impl IntoResponse, AppError> {
    let host_id = &claims.host_id;
    let instance_id = &claims.instance_id;
    reconcile_db_restore(&state, host_id, instance_id, report.applied_version).await?;

    // Snapshot before the update so the log can tell a fresh apply (version
    // moved) from a routine heartbeat (same version again).
    let before =
        crate::store::sandboxd_config::status_snapshot(&state.db(), host_id, instance_id).await?;

    let error = report.last_error.trim().to_string();
    crate::store::sandboxd_config::apply_status_report(
        &state.db(),
        host_id,
        instance_id,
        report.applied_version,
        &report.applied_sha256,
        if error.is_empty() {
            None
        } else {
            Some(error.as_str())
        },
        report.consecutive_failures,
    )
    .await?;

    // Mirror the status CASE above for the log line (no second read).
    let (previous_applied, desired) = before.unwrap_or((0, 0));
    let status = if report.applied_version <= 0 {
        "unchanged"
    } else if desired == report.applied_version {
        "insync"
    } else {
        "pending"
    };
    if !error.is_empty() {
        tracing::warn!(
            host = %host_id,
            instance = %instance_id,
            applied = report.applied_version,
            desired,
            consecutive_failures = report.consecutive_failures,
            error = %error,
            "sandboxd reported a config apply error"
        );
    } else if previous_applied != report.applied_version {
        tracing::info!(
            host = %host_id,
            instance = %instance_id,
            applied = report.applied_version,
            desired,
            status,
            "sandboxd applied a new config version"
        );
    } else {
        tracing::debug!(
            host = %host_id,
            instance = %instance_id,
            applied = report.applied_version,
            desired,
            status,
            "sandboxd status report"
        );
    }
    Ok(Json(ApiResponse::success(Value::Null)))
}
