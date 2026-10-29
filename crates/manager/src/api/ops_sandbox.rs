//! Ops API: sandbox host enrollment, Manager-managed configuration, and
//! resource-profile grants (design record: sandbox-host-configuration).
//!
//! Gated by [`crate::middleware::OpsAdmin`] (platform admin or platform-ops).
//! Sandboxd does NOT use this namespace — it authenticates with its
//! per-instance token against the dedicated `/sandboxd/*` endpoints (PR #3).
//!
//! Versioning: every accepted config change bumps the host's desired version
//! to `max(desired, applied) + 1` and stores an immutable history row, so a
//! restored/rolled-back database can never collide with a version a host
//! already applied. When the approval setting is on, a `PUT` creates a
//! pending change instead; a *different* ops user must approve it.

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderName},
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use shared::error::AppError;
use shared::protocol::response::ApiResponse;
use uuid::Uuid;

use crate::audit::{self, Action, AuditEvent, Resource};
use crate::middleware::OpsAdmin;
use crate::services::platform_settings;
use crate::services::sandbox_config::{self, SandboxHostDocument, DEFAULT_PROFILE};
use crate::state::AppState;

const TOKEN_PREFIX: &str = "buzsd_";
const IF_MATCH: HeaderName = HeaderName::from_static("if-match");

// ── request/response types ──────────────────────────────────────────

#[derive(Deserialize)]
pub struct EnrollRequest {
    pub host_id: String,
    pub instance_id: String,
    /// Host capacity in slots; 0 = unset (the ops UI can backfill later).
    #[serde(default)]
    pub max_resource_slots: u32,
}

#[derive(Deserialize)]
pub struct PutConfigRequest {
    /// The full desired document (without version — the Manager owns it).
    #[serde(flatten)]
    pub document: SandboxHostDocument,
}

#[derive(Deserialize)]
pub struct RollbackRequest {
    pub version: i64,
}

#[derive(Deserialize)]
pub struct ChangeDecisionRequest {
    #[serde(default)]
    pub comment: Option<String>,
}

#[derive(Deserialize)]
pub struct GrantRequest {
    pub team_id: String,
    pub profile: String,
}

#[derive(Deserialize)]
pub struct ChangesQuery {
    #[serde(default)]
    pub status: Option<String>,
}

// ── helpers ─────────────────────────────────────────────────────────

fn validate_pair(host_id: &str, instance_id: &str) -> Result<(), AppError> {
    for (what, raw) in [("host_id", host_id), ("instance_id", instance_id)] {
        shared::instance::validate(raw)
            .map_err(|e| AppError::ValidationFailed(format!("invalid {what}: {e}")))?;
    }
    Ok(())
}

fn generate_token() -> String {
    shared::api_token::generate_with_prefix(TOKEN_PREFIX)
}

async fn load_host(
    pool: &sqlx::SqlitePool,
    host_id: &str,
    instance_id: &str,
) -> Result<sqlx::sqlite::SqliteRow, AppError> {
    sqlx::query(
        "SELECT host_id, instance_id, status, max_resource_slots, desired_version,
                applied_version, applied_sha256, applied_at, last_fetch_at,
                last_fetch_error, consecutive_fetch_failures, created_at, updated_at
         FROM sandbox_hosts WHERE host_id = ? AND instance_id = ?",
    )
    .bind(host_id)
    .bind(instance_id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| AppError::NotFound("sandbox host not enrolled".into()))
}

fn host_json(r: &sqlx::sqlite::SqliteRow) -> Value {
    use sqlx::Row;
    json!({
        "host_id": r.get::<String, _>(0),
        "instance_id": r.get::<String, _>(1),
        "status": r.get::<String, _>(2),
        "max_resource_slots": r.get::<i64, _>(3),
        "desired_version": r.get::<i64, _>(4),
        "applied_version": r.get::<i64, _>(5),
        "applied_sha256": r.get::<String, _>(6),
        "applied_at": r.get::<Option<String>, _>(7),
        "last_fetch_at": r.get::<Option<String>, _>(8),
        "last_fetch_error": r.get::<Option<String>, _>(9),
        "consecutive_fetch_failures": r.get::<i64, _>(10),
        "created_at": r.get::<Option<String>, _>(11),
        "updated_at": r.get::<Option<String>, _>(12),
    })
}

async fn current_document(
    pool: &sqlx::SqlitePool,
    host_id: &str,
    instance_id: &str,
) -> Result<Option<SandboxHostDocument>, AppError> {
    let raw: Option<String> = sqlx::query_scalar(
        "SELECT document FROM sandbox_host_configs
         WHERE host_id = ? AND instance_id = ?
         ORDER BY version DESC LIMIT 1",
    )
    .bind(host_id)
    .bind(instance_id)
    .fetch_optional(pool)
    .await?;
    Ok(raw.and_then(|raw| serde_json::from_str(&raw).ok()))
}

/// Apply a validated document as the next desired version and record the
/// history row. Caller has already validated and audited.
async fn apply_document(
    pool: &sqlx::SqlitePool,
    host_id: &str,
    instance_id: &str,
    document: &SandboxHostDocument,
    created_by: &str,
) -> Result<i64, AppError> {
    let (desired_version, applied_version): (i64, i64) = sqlx::query_as(
        "SELECT desired_version, applied_version FROM sandbox_hosts
         WHERE host_id = ? AND instance_id = ?",
    )
    .bind(host_id)
    .bind(instance_id)
    .fetch_one(pool)
    .await?;
    let version = desired_version.max(applied_version) + 1;
    let json = serde_json::to_string(&sandbox_config::canonical_json(document))
        .map_err(|e| AppError::Internal(e.to_string()))?;
    let sha = sandbox_config::sha256_hex(json.as_bytes());

    sqlx::query(
        "INSERT INTO sandbox_host_configs (host_id, instance_id, version, document, document_sha256, created_by)
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(host_id)
    .bind(instance_id)
    .bind(version)
    .bind(&json)
    .bind(&sha)
    .bind(created_by)
    .execute(pool)
    .await?;
    sqlx::query(
        "UPDATE sandbox_hosts SET desired_version = ?, updated_at = datetime('now')
         WHERE host_id = ? AND instance_id = ?",
    )
    .bind(version)
    .bind(host_id)
    .bind(instance_id)
    .execute(pool)
    .await?;
    Ok(version)
}

async fn require_approval_enabled(pool: &sqlx::SqlitePool) -> Result<bool, AppError> {
    let raw = platform_settings::effective(
        pool,
        platform_settings::KEY_SANDBOX_CONFIG_REQUIRE_APPROVAL,
        "false",
    )
    .await;
    Ok(raw == "true")
}

// ── handlers ────────────────────────────────────────────────────────

/// GET /ops/sandbox-hosts — every enrolled host with desired/applied state.
pub async fn list_hosts(
    _auth: OpsAdmin,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let rows = sqlx::query(
        "SELECT host_id, instance_id, status, max_resource_slots, desired_version,
                applied_version, applied_sha256, applied_at, last_fetch_at,
                last_fetch_error, consecutive_fetch_failures, created_at, updated_at
         FROM sandbox_hosts ORDER BY host_id, instance_id",
    )
    .fetch_all(&state.pool)
    .await?;
    let hosts: Vec<Value> = rows.iter().map(host_json).collect();
    Ok(Json(ApiResponse::success(json!({ "hosts": hosts }))))
}

/// POST /ops/sandbox-hosts — enroll a (host_id, instance_id) pair and return
/// the one-time bearer token for its sandboxd. Re-enrolling rotates the token.
pub async fn enroll_host(
    auth: OpsAdmin,
    State(state): State<AppState>,
    Json(req): Json<EnrollRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let host_id = req.host_id.trim().to_string();
    let instance_id = req.instance_id.trim().to_string();
    validate_pair(&host_id, &instance_id)?;
    let token = generate_token();
    let token_hash = shared::api_token::hash_token(&token);

    sqlx::query(
        "INSERT INTO sandbox_hosts (host_id, instance_id, status, max_resource_slots, token_hash)
         VALUES (?, ?, 'enrolled', ?, ?)
         ON CONFLICT(host_id, instance_id) DO UPDATE SET
           token_hash = excluded.token_hash,
           max_resource_slots = CASE WHEN excluded.max_resource_slots > 0
                                     THEN excluded.max_resource_slots
                                     ELSE sandbox_hosts.max_resource_slots END,
           updated_at = datetime('now')",
    )
    .bind(&host_id)
    .bind(&instance_id)
    .bind(req.max_resource_slots as i64)
    .bind(&token_hash)
    .execute(&state.pool)
    .await?;

    audit::record(
        &state.pool,
        AuditEvent::from_auth(&auth, Action::SandboxHostEnroll)
            .on(Resource::sandbox_host(&host_id, &instance_id))
            .after(Some(
                json!({ "max_resource_slots": req.max_resource_slots }),
            )),
    )
    .await;

    tracing::info!(host = %host_id, instance = %instance_id, "sandbox host enrolled");
    Ok(Json(ApiResponse::success(json!({
        "host_id": host_id,
        "instance_id": instance_id,
        // Shown exactly once; only the SHA-256 is stored.
        "token": token,
    }))))
}

/// GET /ops/sandbox-hosts/{host_id}/{instance_id} — detail incl. desired doc.
pub async fn get_host(
    _auth: OpsAdmin,
    Path((host_id, instance_id)): Path<(String, String)>,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    validate_pair(&host_id, &instance_id)?;
    let row = load_host(&state.pool, &host_id, &instance_id).await?;
    let document = current_document(&state.pool, &host_id, &instance_id).await?;
    let mut body = host_json(&row);
    body["document"] = serde_json::to_value(&document).unwrap_or(Value::Null);
    // Runners reporting this host (PR #5 wires the heartbeat side).
    let runners: Vec<String> =
        sqlx::query_scalar("SELECT name FROM runners WHERE host_id = ? ORDER BY name")
            .bind(&host_id)
            .fetch_all(&state.pool)
            .await?
            .into_iter()
            .collect();
    body["attached_runners"] = json!(runners);
    Ok(Json(ApiResponse::success(body)))
}

/// PUT /ops/sandbox-hosts/{host_id}/{instance_id}/config — replace the whole
/// desired document. Honors `If-Match: <version>`; creates a pending change
/// when the approval setting is on.
pub async fn put_config(
    auth: OpsAdmin,
    Path((host_id, instance_id)): Path<(String, String)>,
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<PutConfigRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    validate_pair(&host_id, &instance_id)?;
    load_host(&state.pool, &host_id, &instance_id).await?;

    let (document, warnings) = sandbox_config::validate(&req.document)
        .map_err(|problems| AppError::ValidationFailed(problems.join("; ")))?;

    if let Some(if_match) = headers.get(&IF_MATCH).and_then(|v| v.to_str().ok()) {
        let row = load_host(&state.pool, &host_id, &instance_id).await?;
        use sqlx::Row;
        let desired: i64 = row.get(4);
        let expected: i64 = if_match
            .trim()
            .parse()
            .map_err(|_| AppError::ValidationFailed("If-Match must be a version number".into()))?;
        if expected != desired {
            return Err(AppError::Conflict(format!(
                "If-Match version {expected} does not match desired version {desired}"
            )));
        }
    }

    if require_approval_enabled(&state.pool).await? {
        let before = current_document(&state.pool, &host_id, &instance_id).await?;
        let change_id = Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO sandbox_config_changes
             (id, host_id, instance_id, status, requested_by, before_document, after_document)
             VALUES (?, ?, ?, 'pending', ?, ?, ?)",
        )
        .bind(&change_id)
        .bind(&host_id)
        .bind(&instance_id)
        .bind(&auth.username)
        .bind(serde_json::to_string(&before).unwrap_or_default())
        .bind(serde_json::to_string(&document).map_err(|e| AppError::Internal(e.to_string()))?)
        .execute(&state.pool)
        .await?;
        audit::record(
            &state.pool,
            AuditEvent::from_auth(&auth, Action::SandboxHostConfigUpdate)
                .on(Resource::sandbox_host(&host_id, &instance_id))
                .after(Some(json!({ "pending_change": change_id }))),
        )
        .await;
        return Ok(Json(ApiResponse::success(json!({
            "pending": true,
            "change_id": change_id,
            "warnings": warnings,
        }))));
    }

    let version = apply_document(
        &state.pool,
        &host_id,
        &instance_id,
        &document,
        &auth.username,
    )
    .await?;
    audit::record(
        &state.pool,
        AuditEvent::from_auth(&auth, Action::SandboxHostConfigUpdate)
            .on(Resource::sandbox_host(&host_id, &instance_id))
            .after(Some(json!({
                "version": version,
                "document": sandbox_config::canonical_json(&document),
            }))),
    )
    .await;
    Ok(Json(ApiResponse::success(json!({
        "pending": false,
        "version": version,
        "warnings": warnings,
    }))))
}

/// POST /ops/sandbox-hosts/{host_id}/{instance_id}/validate — dry-run.
pub async fn validate_config(
    _auth: OpsAdmin,
    Path((host_id, instance_id)): Path<(String, String)>,
    Json(req): Json<PutConfigRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    validate_pair(&host_id, &instance_id)?;
    match sandbox_config::validate(&req.document) {
        Ok((_, warnings)) => Ok(Json(ApiResponse::success(json!({
            "valid": true,
            "warnings": warnings,
        })))),
        Err(problems) => Ok(Json(ApiResponse::success(json!({
            "valid": false,
            "problems": problems,
        })))),
    }
}

/// GET /ops/sandbox-hosts/{host_id}/{instance_id}/versions — history.
pub async fn list_versions(
    _auth: OpsAdmin,
    Path((host_id, instance_id)): Path<(String, String)>,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    validate_pair(&host_id, &instance_id)?;
    let rows: Vec<(i64, String, String, String, String)> = sqlx::query_as(
        "SELECT version, document_sha256, created_by, created_at, document
         FROM sandbox_host_configs
         WHERE host_id = ? AND instance_id = ?
         ORDER BY version DESC",
    )
    .bind(&host_id)
    .bind(&instance_id)
    .fetch_all(&state.pool)
    .await?;
    let versions: Vec<Value> = rows
        .into_iter()
        .map(|(version, sha, by, at, document)| {
            let parsed: Value = serde_json::from_str(&document).unwrap_or(Value::Null);
            json!({
                "version": version,
                "sha256": sha,
                "created_by": by,
                "created_at": at,
                "document": parsed,
            })
        })
        .collect();
    Ok(Json(ApiResponse::success(json!({ "versions": versions }))))
}
/// POST /ops/sandbox-hosts/{host_id}/{instance_id}/rollback — write an older
/// document back as a NEW version (never an in-place edit).
pub async fn rollback(
    auth: OpsAdmin,
    Path((host_id, instance_id)): Path<(String, String)>,
    State(state): State<AppState>,
    Json(req): Json<RollbackRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    validate_pair(&host_id, &instance_id)?;
    let raw: String = sqlx::query_scalar(
        "SELECT document FROM sandbox_host_configs
         WHERE host_id = ? AND instance_id = ? AND version = ?",
    )
    .bind(&host_id)
    .bind(&instance_id)
    .bind(req.version)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound("no such config version".into()))?;
    let document: SandboxHostDocument = serde_json::from_str(&raw)
        .map_err(|e| AppError::Internal(format!("stored document unreadable: {e}")))?;
    let (document, warnings) = sandbox_config::validate(&document)
        .map_err(|problems| AppError::ValidationFailed(problems.join("; ")))?;
    let version = apply_document(
        &state.pool,
        &host_id,
        &instance_id,
        &document,
        &auth.username,
    )
    .await?;
    audit::record(
        &state.pool,
        AuditEvent::from_auth(&auth, Action::SandboxHostRollback)
            .on(Resource::sandbox_host(&host_id, &instance_id))
            .after(Some(json!({
                "restored_from": req.version,
                "new_version": version,
            }))),
    )
    .await;
    Ok(Json(ApiResponse::success(json!({
        "restored_from": req.version,
        "version": version,
        "warnings": warnings,
    }))))
}

/// POST /ops/sandbox-hosts/{host_id}/{instance_id}/token/rotate.
pub async fn rotate_token(
    auth: OpsAdmin,
    Path((host_id, instance_id)): Path<(String, String)>,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    validate_pair(&host_id, &instance_id)?;
    load_host(&state.pool, &host_id, &instance_id).await?;
    let token = generate_token();
    sqlx::query(
        "UPDATE sandbox_hosts SET token_hash = ?, updated_at = datetime('now')
         WHERE host_id = ? AND instance_id = ?",
    )
    .bind(shared::api_token::hash_token(&token))
    .bind(&host_id)
    .bind(&instance_id)
    .execute(&state.pool)
    .await?;
    audit::record(
        &state.pool,
        AuditEvent::from_auth(&auth, Action::SandboxHostTokenRotate)
            .on(Resource::sandbox_host(&host_id, &instance_id)),
    )
    .await;
    Ok(Json(ApiResponse::success(json!({ "token": token }))))
}

/// GET /ops/sandbox-changes?status=pending — approval queue.
pub async fn list_changes(
    _auth: OpsAdmin,
    State(state): State<AppState>,
    Query(q): Query<ChangesQuery>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let status = q.status.unwrap_or_else(|| "pending".into());
    let rows: Vec<(String, String, String, String, String, String, Option<String>, Option<String>)> =
        sqlx::query_as(
            "SELECT id, host_id, instance_id, status, requested_by, after_document, created_at, decided_at
             FROM sandbox_config_changes WHERE status = ? ORDER BY created_at DESC",
        )
        .bind(&status)
        .fetch_all(&state.pool)
        .await?;
    let changes: Vec<Value> = rows
        .into_iter()
        .map(
            |(id, host, instance, status, by, after, created, decided)| {
                let parsed: Value = serde_json::from_str(&after).unwrap_or(Value::Null);
                json!({
                    "id": id,
                    "host_id": host,
                    "instance_id": instance,
                    "status": status,
                    "requested_by": by,
                    "after_document": parsed,
                    "created_at": created,
                    "decided_at": decided,
                })
            },
        )
        .collect();
    Ok(Json(ApiResponse::success(json!({ "changes": changes }))))
}

/// POST /ops/sandbox-changes/{id}/approve — the approver must differ from the
/// requester; approval applies the change as a new desired version.
pub async fn approve_change(
    auth: OpsAdmin,
    Path(change_id): Path<String>,
    State(state): State<AppState>,
    Json(req): Json<ChangeDecisionRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let row: Option<(String, String, String, String, String)> = sqlx::query_as(
        "SELECT status, requested_by, host_id, instance_id, after_document
         FROM sandbox_config_changes WHERE id = ?",
    )
    .bind(&change_id)
    .fetch_optional(&state.pool)
    .await?;
    let Some((status, requested_by, host_id, instance_id, after_document)) = row else {
        return Err(AppError::NotFound("change not found".into()));
    };
    if status != "pending" {
        return Err(AppError::Conflict(format!("change is already {status}")));
    }
    if requested_by == auth.username {
        return Err(AppError::Conflict(
            "the requesting user cannot approve their own change".into(),
        ));
    }
    let document: SandboxHostDocument = serde_json::from_str(&after_document)
        .map_err(|e| AppError::Internal(format!("pending document unreadable: {e}")))?;
    let (document, _) = sandbox_config::validate(&document)
        .map_err(|problems| AppError::Internal(problems.join("; ")))?;
    let version = apply_document(
        &state.pool,
        &host_id,
        &instance_id,
        &document,
        &auth.username,
    )
    .await?;
    sqlx::query(
        "UPDATE sandbox_config_changes SET status = 'approved', approved_by = ?, decided_at = datetime('now')
         WHERE id = ?",
    )
    .bind(&auth.username)
    .bind(&change_id)
    .execute(&state.pool)
    .await?;
    audit::record(
        &state.pool,
        AuditEvent::from_auth(&auth, Action::SandboxHostConfigApproved)
            .on(Resource::sandbox_host(&host_id, &instance_id))
            .after(Some(json!({
                "change_id": change_id,
                "requested_by": requested_by,
                "version": version,
                "comment": req.comment,
            }))),
    )
    .await;
    Ok(Json(ApiResponse::success(json!({ "version": version }))))
}

/// POST /ops/sandbox-changes/{id}/reject.
pub async fn reject_change(
    auth: OpsAdmin,
    Path(change_id): Path<String>,
    State(state): State<AppState>,
    Json(req): Json<ChangeDecisionRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let row: Option<(String, String, String, String)> = sqlx::query_as(
        "SELECT status, requested_by, host_id, instance_id
         FROM sandbox_config_changes WHERE id = ?",
    )
    .bind(&change_id)
    .fetch_optional(&state.pool)
    .await?;
    let Some((status, requested_by, host_id, instance_id)) = row else {
        return Err(AppError::NotFound("change not found".into()));
    };
    if status != "pending" {
        return Err(AppError::Conflict(format!("change is already {status}")));
    }
    sqlx::query(
        "UPDATE sandbox_config_changes SET status = 'rejected', approved_by = ?, decided_at = datetime('now')
         WHERE id = ?",
    )
    .bind(&auth.username)
    .bind(&change_id)
    .execute(&state.pool)
    .await?;
    audit::record(
        &state.pool,
        AuditEvent::from_auth(&auth, Action::SandboxHostConfigRejected)
            .on(Resource::sandbox_host(&host_id, &instance_id))
            .after(Some(json!({
                "change_id": change_id,
                "requested_by": requested_by,
                "comment": req.comment,
            }))),
    )
    .await;
    Ok(Json(ApiResponse::success(Value::Null)))
}

// ── resource-profile grants ─────────────────────────────────────────

/// GET /ops/resource-grants — the full team allowlist (`default` is implicit).
pub async fn list_grants(
    _auth: OpsAdmin,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let rows: Vec<(String, String, String, Option<String>)> = sqlx::query_as(
        "SELECT g.team_id, t.name, g.profile, g.created_at
         FROM team_resource_grants g JOIN teams t ON t.id = g.team_id
         ORDER BY t.name, g.profile",
    )
    .fetch_all(&state.pool)
    .await?;
    let grants: Vec<Value> = rows
        .into_iter()
        .map(|(team_id, team_name, profile, created_at)| {
            json!({
                "team_id": team_id,
                "team_name": team_name,
                "profile": profile,
                "created_at": created_at,
            })
        })
        .collect();
    Ok(Json(ApiResponse::success(json!({ "grants": grants }))))
}

/// PUT /ops/resource-grants — allow a team to request one non-default profile.
pub async fn upsert_grant(
    auth: OpsAdmin,
    State(state): State<AppState>,
    Json(req): Json<GrantRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let team_id = req.team_id.trim().to_string();
    let profile = req.profile.trim().to_string();
    if profile == DEFAULT_PROFILE {
        return Err(AppError::ValidationFailed(
            "'default' is implicitly available to every team; only non-default profiles are granted".into(),
        ));
    }
    if !profile
        .chars()
        .enumerate()
        .all(|(i, c)| c.is_ascii_digit() || c.is_ascii_lowercase() || (c == '-' && i > 0))
        || profile.is_empty()
        || profile.len() > 32
    {
        return Err(AppError::ValidationFailed(format!(
            "profile name '{profile}' is invalid (expected [a-z0-9][a-z0-9-]{{0,31}})"
        )));
    }
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM teams WHERE id = ?)")
        .bind(&team_id)
        .fetch_one(&state.pool)
        .await?;
    if !exists {
        return Err(AppError::ValidationFailed(format!(
            "team '{team_id}' does not exist"
        )));
    }
    sqlx::query(
        "INSERT INTO team_resource_grants (team_id, profile, granted_by)
         VALUES (?, ?, ?)
         ON CONFLICT(team_id, profile) DO NOTHING",
    )
    .bind(&team_id)
    .bind(&profile)
    .bind(&auth.username)
    .execute(&state.pool)
    .await?;
    audit::record(
        &state.pool,
        AuditEvent::from_auth(&auth, Action::ResourceGrantUpdate)
            .on(Resource::resource_grant(&team_id, &profile))
            .after(Some(
                json!({ "team_id": team_id, "profile": profile, "granted": true }),
            )),
    )
    .await;
    Ok(Json(ApiResponse::success(json!({
        "team_id": team_id,
        "profile": profile,
        "granted": true,
    }))))
}

/// DELETE /ops/resource-grants/{team_id}/{profile} — revoke.
pub async fn delete_grant(
    auth: OpsAdmin,
    Path((team_id, profile)): Path<(String, String)>,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    let result = sqlx::query("DELETE FROM team_resource_grants WHERE team_id = ? AND profile = ?")
        .bind(&team_id)
        .bind(&profile)
        .execute(&state.pool)
        .await?;
    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("grant not found".into()));
    }
    audit::record(
        &state.pool,
        AuditEvent::from_auth(&auth, Action::ResourceGrantUpdate)
            .on(Resource::resource_grant(&team_id, &profile))
            .after(Some(
                json!({ "team_id": team_id, "profile": profile, "granted": false }),
            )),
    )
    .await;
    Ok(Json(ApiResponse::success(())))
}
