//! Platform Environments — named scopes for Variable / Secret default overrides.
//!
//! Shared defaults live in `platform_vars` / `secrets`. An Environment may
//! override a subset of those values for a job when the operator selects it at
//! trigger time. GitHub sync per-repo overrides are a separate axis
//! (`github_sync`).

use axum::{
    extract::{Path, State},
    Json,
};
use serde::Deserialize;
use shared::config_auth::parse_oss_secret_value;
use shared::error::AppError;
use shared::protocol::response::ApiResponse;
use shared::rbac::TeamRole;
use shared::registry_auth::parse_registry_secret_value;
use shared::secret_kind::{is_aliyun_oss_auth_kind, is_registry_kind, normalize_secret_kind};
use sqlx::Row;
use uuid::Uuid;

use crate::audit::{self, Action, AuditEvent};
use crate::crypto;
use crate::middleware::{AuthUser, PlatformAdmin};
use crate::state::AppState;

/// Protection / approval-gate settings for an Environment (方案 B).
#[derive(Debug, Clone)]
pub struct EnvironmentProtection {
    pub enabled: bool,
    pub min_trigger_role: TeamRole,
    pub min_approve_role: TeamRole,
    pub require_approvals: i64,
    pub forbid_self_approve: bool,
    pub approval_timeout_seconds: i64,
}

impl EnvironmentProtection {
    pub fn defaults() -> Self {
        Self {
            enabled: false,
            min_trigger_role: TeamRole::Developer,
            min_approve_role: TeamRole::Maintainer,
            require_approvals: 1,
            forbid_self_approve: true,
            approval_timeout_seconds: 86400,
        }
    }

    /// Nested JSON frozen into `job.snapshot.protection`.
    pub fn to_snapshot_json(&self) -> serde_json::Value {
        serde_json::json!({
            "enabled": self.enabled,
            "min_trigger_role": self.min_trigger_role.as_str(),
            "min_approve_role": self.min_approve_role.as_str(),
            "require_approvals": self.require_approvals,
            "forbid_self_approve": self.forbid_self_approve,
            "approval_timeout_seconds": self.approval_timeout_seconds,
        })
    }

    /// Flat fields for Environment list/get/create/update API responses.
    pub fn to_api_fields(&self) -> serde_json::Map<String, serde_json::Value> {
        let mut m = serde_json::Map::new();
        m.insert("protection_enabled".into(), serde_json::json!(self.enabled));
        m.insert(
            "min_trigger_role".into(),
            serde_json::json!(self.min_trigger_role.as_str()),
        );
        m.insert(
            "min_approve_role".into(),
            serde_json::json!(self.min_approve_role.as_str()),
        );
        m.insert(
            "require_approvals".into(),
            serde_json::json!(self.require_approvals),
        );
        m.insert(
            "forbid_self_approve".into(),
            serde_json::json!(self.forbid_self_approve),
        );
        m.insert(
            "approval_timeout_seconds".into(),
            serde_json::json!(self.approval_timeout_seconds),
        );
        m
    }
}

/// Roles allowed for `min_trigger_role` / `min_approve_role` (viewer is never enough).
fn parse_protection_role(raw: &str, field: &str) -> Result<TeamRole, AppError> {
    match TeamRole::parse(raw.trim()) {
        Some(role @ (TeamRole::Owner | TeamRole::Maintainer | TeamRole::Developer)) => Ok(role),
        Some(TeamRole::Viewer) => Err(AppError::ValidationFailed(format!(
            "{field} cannot be 'viewer' — use owner, maintainer, or developer"
        ))),
        None => Err(AppError::ValidationFailed(format!(
            "{field} must be owner, maintainer, or developer"
        ))),
    }
}

fn merge_protection_from_request(
    base: &EnvironmentProtection,
    enabled: Option<bool>,
    min_trigger_role: Option<&str>,
    min_approve_role: Option<&str>,
    require_approvals: Option<i64>,
    forbid_self_approve: Option<bool>,
    approval_timeout_seconds: Option<i64>,
) -> Result<EnvironmentProtection, AppError> {
    let mut p = base.clone();
    if let Some(v) = enabled {
        p.enabled = v;
    }
    if let Some(s) = min_trigger_role {
        p.min_trigger_role = parse_protection_role(s, "min_trigger_role")?;
    }
    if let Some(s) = min_approve_role {
        p.min_approve_role = parse_protection_role(s, "min_approve_role")?;
    }
    if let Some(n) = require_approvals {
        if n < 1 {
            return Err(AppError::ValidationFailed(
                "require_approvals must be >= 1".into(),
            ));
        }
        p.require_approvals = n;
    }
    if let Some(v) = forbid_self_approve {
        p.forbid_self_approve = v;
    }
    if let Some(n) = approval_timeout_seconds {
        if n < 1 {
            return Err(AppError::ValidationFailed(
                "approval_timeout_seconds must be >= 1".into(),
            ));
        }
        p.approval_timeout_seconds = n;
    }
    Ok(p)
}

fn protection_from_db_row(
    enabled: i64,
    min_trigger: &str,
    min_approve: &str,
    require: i64,
    forbid: i64,
    timeout: i64,
) -> EnvironmentProtection {
    EnvironmentProtection {
        enabled: enabled != 0,
        min_trigger_role: TeamRole::parse(min_trigger).unwrap_or(TeamRole::Developer),
        min_approve_role: TeamRole::parse(min_approve).unwrap_or(TeamRole::Maintainer),
        require_approvals: require.max(1),
        forbid_self_approve: forbid != 0,
        approval_timeout_seconds: timeout.max(1),
    }
}

const ENV_PROTECTION_SELECT: &str = "protection_enabled, min_trigger_role, min_approve_role, \
     require_approvals, forbid_self_approve, approval_timeout_seconds";

fn validate_slug(slug: &str) -> Result<(), AppError> {
    if slug != slug.trim() {
        return Err(AppError::ValidationFailed(
            "slug must not contain surrounding whitespace".into(),
        ));
    }
    let s = slug;
    if s.is_empty() {
        return Err(AppError::ValidationFailed("slug is required".into()));
    }
    if s.len() > 64 {
        return Err(AppError::ValidationFailed(
            "slug must be ≤ 64 characters".into(),
        ));
    }
    if !s
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
    {
        return Err(AppError::ValidationFailed(
            "slug must be lowercase letters, digits, '-' or '_'".into(),
        ));
    }
    if s.starts_with('-') || s.ends_with('-') {
        return Err(AppError::ValidationFailed(
            "slug must not start or end with '-'".into(),
        ));
    }
    Ok(())
}

fn validate_secret_override_value(kind: &str, value: &str) -> Result<(), AppError> {
    if value.is_empty() {
        return Err(AppError::ValidationFailed(
            "secret override value must be non-empty (delete the override to inherit)".into(),
        ));
    }
    if is_registry_kind(kind) {
        parse_registry_secret_value(value, Some("registry.example.com/ns/img:tag")).map_err(
            |e| {
                AppError::ValidationFailed(format!(
                    "kind=registry requires JSON with username/password \
                     (optional registry, provider): {e}"
                ))
            },
        )?;
    } else if is_aliyun_oss_auth_kind(kind) {
        parse_oss_secret_value(value).map_err(|e| {
            AppError::ValidationFailed(format!(
                "kind=aliyun_oss_auth requires JSON with endpoint/bucket/access_key_id/access_key_secret \
                 (optional region, provider): {e}"
            ))
        })?;
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
pub struct CreateEnvironmentRequest {
    pub name: String,
    pub slug: String,
    #[serde(default)]
    pub description: String,
    pub protection_enabled: Option<bool>,
    pub min_trigger_role: Option<String>,
    pub min_approve_role: Option<String>,
    pub require_approvals: Option<i64>,
    pub forbid_self_approve: Option<bool>,
    pub approval_timeout_seconds: Option<i64>,
    /// Deployment concurrency across pipelines targeting this environment:
    /// off (default) | queue | cancel_in_progress.
    #[serde(default)]
    pub deployment_concurrency: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateEnvironmentRequest {
    pub name: Option<String>,
    pub slug: Option<String>,
    pub description: Option<String>,
    pub protection_enabled: Option<bool>,
    pub min_trigger_role: Option<String>,
    pub min_approve_role: Option<String>,
    pub require_approvals: Option<i64>,
    pub forbid_self_approve: Option<bool>,
    pub approval_timeout_seconds: Option<i64>,
    #[serde(default)]
    pub deployment_concurrency: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct HostAllowlistEntry {
    /// `name` (host name) or `label` (host label).
    pub kind: String,
    pub value: String,
}

/// Validate the deployment concurrency mode value.
pub(crate) fn validate_deployment_concurrency(raw: Option<&str>) -> Result<String, AppError> {
    match raw
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("off")
    {
        "off" => Ok("off".into()),
        "queue" => Ok("queue".into()),
        "cancel_in_progress" => Ok("cancel_in_progress".into()),
        other => Err(AppError::ValidationFailed(format!(
            "deployment_concurrency '{other}' is invalid — use off, queue or cancel_in_progress"
        ))),
    }
}

#[derive(Debug, Deserialize)]
pub struct UpsertOverrideRequest {
    pub var_id: String,
    pub value: String,
}

#[derive(Debug, Deserialize)]
pub struct UpsertSecretOverrideRequest {
    pub secret_id: String,
    pub value: String,
}

/// GET /environments — any authenticated user (Trigger dropdown).
pub async fn list(
    _auth: AuthUser,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<Vec<serde_json::Value>>>, AppError> {
    let rows = sqlx::query(
        "SELECT e.id, e.name, e.slug, e.description, e.updated_at,
                (SELECT COUNT(*) FROM environment_var_overrides o WHERE o.environment_id = e.id) AS var_override_count,
                (SELECT COUNT(*) FROM environment_secret_overrides o WHERE o.environment_id = e.id) AS secret_override_count,
                e.protection_enabled, e.min_trigger_role, e.min_approve_role,
                e.require_approvals, e.forbid_self_approve, e.approval_timeout_seconds,
                COALESCE(e.deployment_concurrency, 'off') AS deployment_concurrency,
                (SELECT COUNT(*) FROM environment_hosts h WHERE h.environment_id = e.id) AS host_allowlist_count
         FROM environments e
         ORDER BY e.name ASC",
    )
    .fetch_all(&state.pool)
    .await?;

    let out: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|r| {
            let var_n: i64 = r.get(5);
            let secret_n: i64 = r.get(6);
            let protection = protection_from_db_row(
                r.get(7),
                &r.get::<String, _>(8),
                &r.get::<String, _>(9),
                r.get(10),
                r.get(11),
                r.get(12),
            );
            let mut obj = serde_json::json!({
                "id": r.get::<String, _>(0),
                "name": r.get::<String, _>(1),
                "slug": r.get::<String, _>(2),
                "description": r.get::<String, _>(3),
                "updated_at": r.get::<Option<String>, _>(4),
                "var_override_count": var_n,
                "secret_override_count": secret_n,
                // Backward-compatible total for list badges.
                "override_count": var_n + secret_n,
                "deployment_concurrency": r.get::<String, _>(13),
                "host_allowlist_count": r.get::<i64, _>(14),
            });
            if let Some(map) = obj.as_object_mut() {
                map.extend(protection.to_api_fields());
            }
            obj
        })
        .collect();
    Ok(Json(ApiResponse::success(out)))
}

/// GET /environments/{id}
pub async fn get_one(
    _auth: AuthUser,
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let row = sqlx::query(&format!(
        "SELECT id, name, slug, description, updated_at, {ENV_PROTECTION_SELECT},
                COALESCE(deployment_concurrency, 'off')
         FROM environments WHERE id = ?"
    ))
    .bind(&id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound("environment not found".into()))?;

    let protection = protection_from_db_row(
        row.get(5),
        &row.get::<String, _>(6),
        &row.get::<String, _>(7),
        row.get(8),
        row.get(9),
        row.get(10),
    );
    let overrides = list_overrides_json(&state.pool, &id).await?;
    let secret_overrides = list_secret_overrides_json(&state.pool, &id).await?;
    let mut obj = serde_json::json!({
        "id": row.get::<String, _>(0),
        "name": row.get::<String, _>(1),
        "slug": row.get::<String, _>(2),
        "description": row.get::<String, _>(3),
        "updated_at": row.get::<Option<String>, _>(4),
        "overrides": overrides,
        "secret_overrides": secret_overrides,
        "deployment_concurrency": row.get::<String, _>(11),
        "host_allowlist": list_host_allowlist_json(&state.pool, &id).await?,
    });
    if let Some(map) = obj.as_object_mut() {
        map.extend(protection.to_api_fields());
    }
    Ok(Json(ApiResponse::success(obj)))
}

/// GET /environments/{id}/host-allowlist
pub async fn list_host_allowlist(
    _auth: AuthUser,
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<Vec<serde_json::Value>>>, AppError> {
    Ok(Json(ApiResponse::success(
        list_host_allowlist_json(&state.pool, &id).await?,
    )))
}

async fn list_host_allowlist_json(
    pool: &sqlx::SqlitePool,
    environment_id: &str,
) -> Result<Vec<serde_json::Value>, AppError> {
    let rows = sqlx::query(
        "SELECT kind, value, created_at FROM environment_hosts \
         WHERE environment_id = ? ORDER BY kind, value",
    )
    .bind(environment_id)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            serde_json::json!({
                "kind": r.get::<String, _>(0),
                "value": r.get::<String, _>(1),
                "created_at": r.get::<Option<String>, _>(2),
            })
        })
        .collect())
}

/// POST /environments/{id}/host-allowlist — add a name or label entry.
pub async fn add_host_allowlist(
    auth: PlatformAdmin,
    Path(id): Path<String>,
    State(state): State<AppState>,
    Json(req): Json<HostAllowlistEntry>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let kind = req.kind.trim().to_lowercase();
    let value = req.value.trim().to_string();
    if !matches!(kind.as_str(), "name" | "label") {
        return Err(AppError::ValidationFailed(
            "kind must be 'name' or 'label'".into(),
        ));
    }
    if value.is_empty() {
        return Err(AppError::ValidationFailed("value must not be empty".into()));
    }
    let exists: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM environments WHERE id = ?")
        .bind(&id)
        .fetch_one(&state.pool)
        .await?;
    if exists == 0 {
        return Err(AppError::NotFound("environment not found".into()));
    }
    sqlx::query(
        "INSERT INTO environment_hosts (environment_id, kind, value) VALUES (?, ?, ?) \
         ON CONFLICT (environment_id, kind, value) DO NOTHING",
    )
    .bind(&id)
    .bind(&kind)
    .bind(&value)
    .execute(&state.pool)
    .await?;
    audit::record(
        &state.pool,
        AuditEvent::from_auth(&auth, Action::EnvironmentHostAllowlistAdd)
            .resource("environment", &id, &kind)
            .after(Some(serde_json::json!({ "kind": kind, "value": value }))),
    )
    .await;
    Ok(Json(ApiResponse::success(serde_json::json!({
        "kind": kind,
        "value": value,
    }))))
}

/// DELETE /environments/{id}/host-allowlist/{kind}/{value} — remove an entry.
pub async fn delete_host_allowlist(
    auth: PlatformAdmin,
    Path((id, kind, value)): Path<(String, String, String)>,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let result = sqlx::query(
        "DELETE FROM environment_hosts WHERE environment_id = ? AND kind = ? AND value = ?",
    )
    .bind(&id)
    .bind(&kind)
    .bind(&value)
    .execute(&state.pool)
    .await?;
    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("allowlist entry not found".into()));
    }
    audit::record(
        &state.pool,
        AuditEvent::from_auth(&auth, Action::EnvironmentHostAllowlistRemove)
            .resource("environment", &id, &kind)
            .before(Some(serde_json::json!({ "kind": kind, "value": value }))),
    )
    .await;
    Ok(Json(ApiResponse::success(serde_json::json!({
        "kind": kind,
        "value": value,
    }))))
}

async fn list_overrides_json(
    pool: &sqlx::SqlitePool,
    environment_id: &str,
) -> Result<Vec<serde_json::Value>, AppError> {
    let rows = sqlx::query(
        "SELECT o.var_id, v.name, v.value AS default_value, o.value AS override_value, o.updated_at
         FROM environment_var_overrides o
         JOIN platform_vars v ON v.id = o.var_id
         WHERE o.environment_id = ?
         ORDER BY v.name ASC",
    )
    .bind(environment_id)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            serde_json::json!({
                "var_id": r.get::<String, _>(0),
                "var_name": r.get::<String, _>(1),
                "default_value": r.get::<String, _>(2),
                "value": r.get::<String, _>(3),
                "updated_at": r.get::<Option<String>, _>(4),
            })
        })
        .collect())
}

/// Secret overrides never expose plaintext (override or default) — `has_override` only.
async fn list_secret_overrides_json(
    pool: &sqlx::SqlitePool,
    environment_id: &str,
) -> Result<Vec<serde_json::Value>, AppError> {
    let rows = sqlx::query(
        "SELECT o.secret_id, s.name, s.kind, o.updated_at
         FROM environment_secret_overrides o
         JOIN secrets s ON s.id = o.secret_id
         WHERE o.environment_id = ?
         ORDER BY s.name ASC",
    )
    .bind(environment_id)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            serde_json::json!({
                "secret_id": r.get::<String, _>(0),
                "secret_name": r.get::<String, _>(1),
                "kind": normalize_secret_kind(Some(&r.get::<String, _>(2))),
                "has_override": true,
                "updated_at": r.get::<Option<String>, _>(3),
            })
        })
        .collect())
}

pub async fn create(
    auth: PlatformAdmin,
    State(state): State<AppState>,
    Json(req): Json<CreateEnvironmentRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let name = req.name.trim().to_string();
    if name.is_empty() {
        return Err(AppError::ValidationFailed("name is required".into()));
    }
    let slug = req.slug.to_lowercase();
    if req.slug != req.slug.trim() {
        return Err(AppError::ValidationFailed(
            "slug must not contain leading or trailing whitespace".into(),
        ));
    }
    validate_slug(&slug)?;

    let dup = sqlx::query("SELECT id FROM environments WHERE slug = ? OR name = ?")
        .bind(&slug)
        .bind(&name)
        .fetch_optional(&state.pool)
        .await?;
    if dup.is_some() {
        return Err(AppError::Conflict(
            "environment name or slug already exists".into(),
        ));
    }

    let protection = merge_protection_from_request(
        &EnvironmentProtection::defaults(),
        req.protection_enabled,
        req.min_trigger_role.as_deref(),
        req.min_approve_role.as_deref(),
        req.require_approvals,
        req.forbid_self_approve,
        req.approval_timeout_seconds,
    )?;

    let id = Uuid::new_v4().to_string();
    let description = req.description.trim().to_string();
    let deployment_concurrency =
        validate_deployment_concurrency(req.deployment_concurrency.as_deref())?;
    sqlx::query(
        "INSERT INTO environments (
            id, name, slug, description,
            protection_enabled, min_trigger_role, min_approve_role,
            require_approvals, forbid_self_approve, approval_timeout_seconds,
            deployment_concurrency
         ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&name)
    .bind(&slug)
    .bind(&description)
    .bind(if protection.enabled { 1 } else { 0 })
    .bind(protection.min_trigger_role.as_str())
    .bind(protection.min_approve_role.as_str())
    .bind(protection.require_approvals)
    .bind(if protection.forbid_self_approve { 1 } else { 0 })
    .bind(protection.approval_timeout_seconds)
    .bind(&deployment_concurrency)
    .execute(&state.pool)
    .await?;

    tracing::info!(environment = %slug, id = %id, "environments: created");
    audit::record(
        &state.pool,
        AuditEvent::from_auth(&auth, Action::EnvironmentCreate)
            .resource("environment", &id, &name)
            .after(Some(serde_json::json!({
                "slug": slug,
                "description": description,
                "protection": protection.to_snapshot_json(),
            }))),
    )
    .await;

    let mut obj = serde_json::json!({
        "id": id,
        "name": name,
        "slug": slug,
        "description": description,
        "override_count": 0,
        "var_override_count": 0,
        "secret_override_count": 0,
        "deployment_concurrency": deployment_concurrency,
    });
    if let Some(map) = obj.as_object_mut() {
        map.extend(protection.to_api_fields());
    }
    Ok(Json(ApiResponse::success(obj)))
}

pub async fn update(
    auth: PlatformAdmin,
    Path(id): Path<String>,
    State(state): State<AppState>,
    Json(req): Json<UpdateEnvironmentRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let existing = sqlx::query(&format!(
        "SELECT name, slug, description, {ENV_PROTECTION_SELECT} FROM environments WHERE id = ?"
    ))
    .bind(&id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound("environment not found".into()))?;
    let old_name: String = existing.get(0);
    let old_slug: String = existing.get(1);
    let old_desc: String = existing.get(2);
    let old_protection = protection_from_db_row(
        existing.get(3),
        &existing.get::<String, _>(4),
        &existing.get::<String, _>(5),
        existing.get(6),
        existing.get(7),
        existing.get(8),
    );

    let name = req
        .name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(&old_name)
        .to_string();
    let slug = match &req.slug {
        Some(s) => {
            if s != s.trim() {
                return Err(AppError::ValidationFailed(
                    "slug must not contain leading or trailing whitespace".into(),
                ));
            }
            let slug = s.to_lowercase();
            validate_slug(&slug)?;
            slug
        }
        None => old_slug.clone(),
    };
    let description = req
        .description
        .as_deref()
        .map(str::trim)
        .unwrap_or(&old_desc)
        .to_string();
    let protection = merge_protection_from_request(
        &old_protection,
        req.protection_enabled,
        req.min_trigger_role.as_deref(),
        req.min_approve_role.as_deref(),
        req.require_approvals,
        req.forbid_self_approve,
        req.approval_timeout_seconds,
    )?;
    // Omitted → keep the current mode (partial-update semantics like the
    // protection fields).
    let deployment_concurrency = if req.deployment_concurrency.is_some() {
        validate_deployment_concurrency(req.deployment_concurrency.as_deref())?
    } else {
        sqlx::query_scalar(
            "SELECT COALESCE(deployment_concurrency, 'off') FROM environments WHERE id = ?",
        )
        .bind(&id)
        .fetch_one(&state.pool)
        .await
        .unwrap_or_else(|_| "off".to_string())
    };

    if name != old_name || slug != old_slug {
        let dup =
            sqlx::query("SELECT id FROM environments WHERE (slug = ? OR name = ?) AND id != ?")
                .bind(&slug)
                .bind(&name)
                .bind(&id)
                .fetch_optional(&state.pool)
                .await?;
        if dup.is_some() {
            return Err(AppError::Conflict(
                "environment name or slug already exists".into(),
            ));
        }
    }

    sqlx::query(
        "UPDATE environments SET
            name = ?, slug = ?, description = ?,
            protection_enabled = ?, min_trigger_role = ?, min_approve_role = ?,
            require_approvals = ?, forbid_self_approve = ?, approval_timeout_seconds = ?,
            deployment_concurrency = ?, updated_at = datetime('now')
         WHERE id = ?",
    )
    .bind(&name)
    .bind(&slug)
    .bind(&description)
    .bind(if protection.enabled { 1 } else { 0 })
    .bind(protection.min_trigger_role.as_str())
    .bind(protection.min_approve_role.as_str())
    .bind(protection.require_approvals)
    .bind(if protection.forbid_self_approve { 1 } else { 0 })
    .bind(protection.approval_timeout_seconds)
    .bind(&deployment_concurrency)
    .bind(&id)
    .execute(&state.pool)
    .await?;

    tracing::info!(environment = %slug, id = %id, "environments: updated");
    audit::record(
        &state.pool,
        AuditEvent::from_auth(&auth, Action::EnvironmentUpdate)
            .resource("environment", &id, &name)
            .before(Some(serde_json::json!({
                "name": old_name,
                "slug": old_slug,
                "description": old_desc,
                "protection": old_protection.to_snapshot_json(),
            })))
            .after(Some(serde_json::json!({
                "name": name,
                "slug": slug,
                "description": description,
                "protection": protection.to_snapshot_json(),
            }))),
    )
    .await;

    let mut obj = serde_json::json!({
        "id": id,
        "name": name,
        "slug": slug,
        "description": description,
    });
    if let Some(map) = obj.as_object_mut() {
        map.extend(protection.to_api_fields());
    }
    Ok(Json(ApiResponse::success(obj)))
}

pub async fn delete(
    auth: PlatformAdmin,
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    let row = sqlx::query("SELECT name, slug FROM environments WHERE id = ?")
        .bind(&id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(|| AppError::NotFound("environment not found".into()))?;
    let name: String = row.get(0);
    let slug: String = row.get(1);

    sqlx::query("DELETE FROM environments WHERE id = ?")
        .bind(&id)
        .execute(&state.pool)
        .await?;

    tracing::info!(environment = %slug, id = %id, "environments: deleted");
    audit::record(
        &state.pool,
        AuditEvent::from_auth(&auth, Action::EnvironmentDelete)
            .resource("environment", &id, &name)
            .before(Some(serde_json::json!({ "slug": slug }))),
    )
    .await;

    Ok(Json(ApiResponse::success(())))
}

/// GET /environments/{id}/overrides
pub async fn list_overrides(
    _auth: AuthUser,
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<Vec<serde_json::Value>>>, AppError> {
    let exists = sqlx::query("SELECT id FROM environments WHERE id = ?")
        .bind(&id)
        .fetch_optional(&state.pool)
        .await?;
    if exists.is_none() {
        return Err(AppError::NotFound("environment not found".into()));
    }
    let overrides = list_overrides_json(&state.pool, &id).await?;
    Ok(Json(ApiResponse::success(overrides)))
}

/// PUT /environments/{id}/overrides — upsert one Variable override.
pub async fn upsert_override(
    auth: PlatformAdmin,
    Path(id): Path<String>,
    State(state): State<AppState>,
    Json(req): Json<UpsertOverrideRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let env = sqlx::query("SELECT name, slug FROM environments WHERE id = ?")
        .bind(&id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(|| AppError::NotFound("environment not found".into()))?;
    let env_name: String = env.get(0);
    let env_slug: String = env.get(1);

    let var = sqlx::query("SELECT name FROM platform_vars WHERE id = ?")
        .bind(&req.var_id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(|| AppError::NotFound("variable not found".into()))?;
    let var_name: String = var.get(0);

    sqlx::query(
        "INSERT INTO environment_var_overrides (environment_id, var_id, value, updated_at)
         VALUES (?, ?, ?, datetime('now'))
         ON CONFLICT(environment_id, var_id) DO UPDATE SET value = excluded.value, updated_at = datetime('now')",
    )
    .bind(&id)
    .bind(&req.var_id)
    .bind(&req.value)
    .execute(&state.pool)
    .await?;

    tracing::info!(
        environment = %env_slug,
        variable = %var_name,
        "environments: override upserted"
    );
    audit::record(
        &state.pool,
        AuditEvent::from_auth(&auth, Action::EnvironmentOverrideUpsert)
            .resource("environment", &id, &env_name)
            .after(Some(serde_json::json!({
                "var_id": req.var_id,
                "var_name": var_name,
            }))),
    )
    .await;

    Ok(Json(ApiResponse::success(serde_json::json!({
        "var_id": req.var_id,
        "var_name": var_name,
        "value": req.value,
    }))))
}

/// DELETE /environments/{id}/overrides/{var_id} — clear override (inherit default).
pub async fn delete_override(
    auth: PlatformAdmin,
    Path((id, var_id)): Path<(String, String)>,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    let env = sqlx::query("SELECT name, slug FROM environments WHERE id = ?")
        .bind(&id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(|| AppError::NotFound("environment not found".into()))?;
    let env_name: String = env.get(0);
    let env_slug: String = env.get(1);

    let r = sqlx::query(
        "DELETE FROM environment_var_overrides WHERE environment_id = ? AND var_id = ?",
    )
    .bind(&id)
    .bind(&var_id)
    .execute(&state.pool)
    .await?;
    if r.rows_affected() == 0 {
        return Err(AppError::NotFound("override not found".into()));
    }

    tracing::info!(
        environment = %env_slug,
        var_id = %var_id,
        "environments: override cleared"
    );
    audit::record(
        &state.pool,
        AuditEvent::from_auth(&auth, Action::EnvironmentOverrideDelete)
            .resource("environment", &id, &env_name)
            .before(Some(serde_json::json!({ "var_id": var_id }))),
    )
    .await;

    Ok(Json(ApiResponse::success(())))
}

/// GET /environments/{id}/secret-overrides — metadata only (never plaintext).
pub async fn list_secret_overrides(
    _auth: AuthUser,
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<Vec<serde_json::Value>>>, AppError> {
    let exists = sqlx::query("SELECT id FROM environments WHERE id = ?")
        .bind(&id)
        .fetch_optional(&state.pool)
        .await?;
    if exists.is_none() {
        return Err(AppError::NotFound("environment not found".into()));
    }
    let overrides = list_secret_overrides_json(&state.pool, &id).await?;
    Ok(Json(ApiResponse::success(overrides)))
}

/// PUT /environments/{id}/secret-overrides — upsert one Secret override (encrypted at rest).
pub async fn upsert_secret_override(
    auth: PlatformAdmin,
    Path(id): Path<String>,
    State(state): State<AppState>,
    Json(req): Json<UpsertSecretOverrideRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let env = sqlx::query("SELECT name, slug FROM environments WHERE id = ?")
        .bind(&id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(|| AppError::NotFound("environment not found".into()))?;
    let env_name: String = env.get(0);
    let env_slug: String = env.get(1);

    let secret = sqlx::query("SELECT name, kind FROM secrets WHERE id = ?")
        .bind(&req.secret_id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(|| AppError::NotFound("secret not found".into()))?;
    let secret_name: String = secret.get(0);
    let kind = normalize_secret_kind(Some(&secret.get::<String, _>(1)));
    validate_secret_override_value(kind, req.value.trim())?;

    let cipher = crypto::encrypt(req.value.trim(), &state.config.auth.jwt_secret)
        .map_err(|e| AppError::Internal(format!("encrypt secret override: {e}")))?;

    sqlx::query(
        "INSERT INTO environment_secret_overrides (environment_id, secret_id, value, updated_at)
         VALUES (?, ?, ?, datetime('now'))
         ON CONFLICT(environment_id, secret_id) DO UPDATE SET value = excluded.value, updated_at = datetime('now')",
    )
    .bind(&id)
    .bind(&req.secret_id)
    .bind(&cipher)
    .execute(&state.pool)
    .await?;

    tracing::info!(
        environment = %env_slug,
        secret = %secret_name,
        "environments: secret override upserted"
    );
    audit::record(
        &state.pool,
        AuditEvent::from_auth(&auth, Action::EnvironmentSecretOverrideUpsert)
            .resource("environment", &id, &env_name)
            .after(Some(serde_json::json!({
                "secret_id": req.secret_id,
                "secret_name": secret_name,
                "kind": kind,
            }))),
    )
    .await;

    Ok(Json(ApiResponse::success(serde_json::json!({
        "secret_id": req.secret_id,
        "secret_name": secret_name,
        "kind": kind,
        "has_override": true,
    }))))
}

/// DELETE /environments/{id}/secret-overrides/{secret_id} — clear → inherit default.
pub async fn delete_secret_override(
    auth: PlatformAdmin,
    Path((id, secret_id)): Path<(String, String)>,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    let env = sqlx::query("SELECT name, slug FROM environments WHERE id = ?")
        .bind(&id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(|| AppError::NotFound("environment not found".into()))?;
    let env_name: String = env.get(0);
    let env_slug: String = env.get(1);

    let r = sqlx::query(
        "DELETE FROM environment_secret_overrides WHERE environment_id = ? AND secret_id = ?",
    )
    .bind(&id)
    .bind(&secret_id)
    .execute(&state.pool)
    .await?;
    if r.rows_affected() == 0 {
        return Err(AppError::NotFound("secret override not found".into()));
    }

    tracing::info!(
        environment = %env_slug,
        secret_id = %secret_id,
        "environments: secret override cleared"
    );
    audit::record(
        &state.pool,
        AuditEvent::from_auth(&auth, Action::EnvironmentSecretOverrideDelete)
            .resource("environment", &id, &env_name)
            .before(Some(serde_json::json!({ "secret_id": secret_id }))),
    )
    .await;

    Ok(Json(ApiResponse::success(())))
}

/// Resolved environment for trigger: id, slug, and protection settings.
pub(crate) struct ResolvedEnvironment {
    pub id: String,
    pub slug: String,
    pub protection: EnvironmentProtection,
}

/// Resolve environment slug/id → id + slug + protection. Empty/None → None.
/// Validate the slugs a pipeline declares under `environments`: non-empty and
/// all present in the environments registry (same bar as team slugs).
pub(crate) async fn validate_environment_slugs(
    pool: &sqlx::SqlitePool,
    slugs: &[String],
) -> Result<(), AppError> {
    if slugs.is_empty() {
        return Err(AppError::ValidationFailed(
            "environments is required — declare which environments this pipeline targets".into(),
        ));
    }
    let mut missing: Vec<String> = Vec::new();
    for raw_slug in slugs {
        let slug = raw_slug.trim();
        if slug.is_empty() || raw_slug != slug {
            missing.push(raw_slug.to_string());
            continue;
        }
        if !slug
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_'))
            || slug.starts_with('-')
            || slug.ends_with('-')
            || slug.len() > 64
        {
            missing.push(raw_slug.to_string());
            continue;
        }
        let exists: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM environments WHERE slug = ?")
            .bind(slug)
            .fetch_one(pool)
            .await?;
        if exists == 0 {
            missing.push(slug.to_string());
        }
    }
    if !missing.is_empty() {
        return Err(AppError::ValidationFailed(format!(
            "unknown environment(s): {} — create them under Environments first",
            missing.join(", ")
        )));
    }
    Ok(())
}

pub(crate) async fn resolve_environment(
    pool: &sqlx::SqlitePool,
    slug_or_id: Option<&str>,
) -> Result<Option<ResolvedEnvironment>, AppError> {
    let Some(raw) = slug_or_id.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    let row = sqlx::query(&format!(
        "SELECT id, slug, {ENV_PROTECTION_SELECT}
         FROM environments WHERE slug = ? OR id = ?"
    ))
    .bind(raw)
    .bind(raw)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| AppError::NotFound(format!("environment '{raw}' not found")))?;
    let protection = protection_from_db_row(
        row.get(2),
        &row.get::<String, _>(3),
        &row.get::<String, _>(4),
        row.get(5),
        row.get(6),
        row.get(7),
    );
    Ok(Some(ResolvedEnvironment {
        id: row.get(0),
        slug: row.get(1),
        protection,
    }))
}

/// Resolve environment slug → (id, slug) for trigger. Empty/None → None.
#[allow(dead_code)] // Thin wrapper kept for callers that do not need protection.
pub(crate) async fn resolve_environment_id(
    pool: &sqlx::SqlitePool,
    slug_or_id: Option<&str>,
) -> Result<Option<(String, String)>, AppError> {
    Ok(resolve_environment(pool, slug_or_id)
        .await?
        .map(|e| (e.id, e.slug)))
}

/// Load decrypted Environment secret overrides (name → plaintext) for claim.
pub(crate) async fn load_secret_override_plaintexts(
    pool: &sqlx::SqlitePool,
    environment_id: &str,
    jwt_secret: &str,
) -> Result<std::collections::HashMap<String, String>, AppError> {
    let rows = sqlx::query(
        "SELECT s.name, o.value
         FROM environment_secret_overrides o
         JOIN secrets s ON s.id = o.secret_id
         WHERE o.environment_id = ?",
    )
    .bind(environment_id)
    .fetch_all(pool)
    .await?;
    let mut map = std::collections::HashMap::new();
    for r in rows {
        let name: String = r.get(0);
        let cipher: String = r.get(1);
        match crypto::decrypt(&cipher, jwt_secret) {
            Ok(plain) => {
                map.insert(name, plain);
            }
            Err(e) => {
                tracing::warn!(
                    environment_id = %environment_id,
                    secret = %name,
                    error = %e,
                    "environments: failed to decrypt secret override — skipped"
                );
            }
        }
    }
    Ok(map)
}
