//! Platform Environments — named scopes for Variable / Secret default overrides.
//!
//! Shared defaults live in `platform_vars` / `secrets`. An Environment may
//! override a subset of those values for a job when the operator selects it at
//! trigger time. GitHub sync per-repo overrides are a separate axis
//! (`github_sync`).
//!
//! SQL lives in `store::environments`; these handlers do authorization, gate
//! evaluation, audit fan-out and response shaping.

use axum::{
    extract::{Path, State},
    Json,
};
use serde::Deserialize;
use shared::error::AppError;
use shared::protocol::response::ApiResponse;
use uuid::Uuid;

use crate::audit::{self, Action, AuditEvent};
use crate::crypto;
use crate::middleware::{AuthUser, PlatformAdmin};
use crate::state::AppState;

// Re-exported so pipeline / job paths keep one import site for the trigger gates.
pub(crate) use crate::store::environments::{
    load_secret_override_plaintexts, resolve_environment, validate_environment_slugs,
    EnvironmentProtection,
};

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
    Ok(Json(ApiResponse::success(
        crate::store::environments::list_environments(&state.pool).await?,
    )))
}

/// GET /environments/{id}
pub async fn get_one(
    auth: AuthUser,
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    // Variable overrides carry plaintext values (defaults + overrides) —
    // platform-admin only; everyone else gets the environment shell.
    let payload =
        crate::store::environments::environment_detail(&state.pool, &id, auth.is_platform_admin)
            .await?;
    Ok(Json(ApiResponse::success(payload)))
}

/// GET /environments/{id}/host-allowlist
pub async fn list_host_allowlist(
    _auth: AuthUser,
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<Vec<serde_json::Value>>>, AppError> {
    Ok(Json(ApiResponse::success(
        crate::store::environments::list_host_allowlist_json(&state.pool, &id).await?,
    )))
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
    if !crate::store::environments::environment_exists(&state.pool, &id).await? {
        return Err(AppError::NotFound("environment not found".into()));
    }
    crate::store::environments::insert_host_allowlist(&state.pool, &id, &kind, &value).await?;
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
    crate::store::environments::delete_host_allowlist(&state.pool, &id, &kind, &value).await?;
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

/// POST /environments — create a named scope for Variable / Secret defaults.
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
    crate::store::environments::validate_slug(&slug)?;

    if crate::store::environments::name_or_slug_taken(&state.pool, &slug, &name, None).await? {
        return Err(AppError::Conflict(
            "environment name or slug already exists".into(),
        ));
    }

    let protection = crate::store::environments::merge_protection_from_request(
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
    let deployment_concurrency = crate::store::environments::validate_deployment_concurrency(
        req.deployment_concurrency.as_deref(),
    )?;
    crate::store::environments::insert_environment(
        &state.pool,
        &id,
        &name,
        &slug,
        &description,
        &protection,
        &deployment_concurrency,
    )
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

/// PUT /environments/{id} — partial update; omitted fields keep their values.
pub async fn update(
    auth: PlatformAdmin,
    Path(id): Path<String>,
    State(state): State<AppState>,
    Json(req): Json<UpdateEnvironmentRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let existing = crate::store::environments::settings_row(&state.pool, &id)
        .await?
        .ok_or_else(|| AppError::NotFound("environment not found".into()))?;

    let name = req
        .name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(&existing.name)
        .to_string();
    let slug = match &req.slug {
        Some(s) => {
            if s != s.trim() {
                return Err(AppError::ValidationFailed(
                    "slug must not contain leading or trailing whitespace".into(),
                ));
            }
            let slug = s.to_lowercase();
            crate::store::environments::validate_slug(&slug)?;
            slug
        }
        None => existing.slug.clone(),
    };
    let description = req
        .description
        .as_deref()
        .map(str::trim)
        .unwrap_or(&existing.description)
        .to_string();
    let protection = crate::store::environments::merge_protection_from_request(
        &existing.protection,
        req.protection_enabled,
        req.min_trigger_role.as_deref(),
        req.min_approve_role.as_deref(),
        req.require_approvals,
        req.forbid_self_approve,
        req.approval_timeout_seconds,
    )?;
    // Omitted keeps the current mode (partial-update semantics like protection).
    let deployment_concurrency = if req.deployment_concurrency.is_some() {
        crate::store::environments::validate_deployment_concurrency(
            req.deployment_concurrency.as_deref(),
        )?
    } else {
        existing.deployment_concurrency.clone()
    };

    if (name != existing.name || slug != existing.slug)
        && crate::store::environments::name_or_slug_taken(&state.pool, &slug, &name, Some(&id))
            .await?
    {
        return Err(AppError::Conflict(
            "environment name or slug already exists".into(),
        ));
    }

    crate::store::environments::update_environment(
        &state.pool,
        &id,
        &name,
        &slug,
        &description,
        &protection,
        &deployment_concurrency,
    )
    .await?;

    tracing::info!(environment = %slug, id = %id, "environments: updated");
    audit::record(
        &state.pool,
        AuditEvent::from_auth(&auth, Action::EnvironmentUpdate)
            .resource("environment", &id, &name)
            .before(Some(serde_json::json!({
                "name": existing.name,
                "slug": existing.slug,
                "description": existing.description,
                "protection": existing.protection.to_snapshot_json(),
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

/// DELETE /environments/{id}
pub async fn delete(
    auth: PlatformAdmin,
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    let (name, slug) = crate::store::environments::name_and_slug(&state.pool, &id)
        .await?
        .ok_or_else(|| AppError::NotFound("environment not found".into()))?;

    crate::store::environments::delete_environment(&state.pool, &id).await?;

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
    _auth: PlatformAdmin,
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<Vec<serde_json::Value>>>, AppError> {
    if !crate::store::environments::environment_exists(&state.pool, &id).await? {
        return Err(AppError::NotFound("environment not found".into()));
    }
    Ok(Json(ApiResponse::success(
        crate::store::environments::list_overrides_json(&state.pool, &id).await?,
    )))
}

/// PUT /environments/{id}/overrides — upsert one Variable override.
pub async fn upsert_override(
    auth: PlatformAdmin,
    Path(id): Path<String>,
    State(state): State<AppState>,
    Json(req): Json<UpsertOverrideRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let (env_name, env_slug) = crate::store::environments::name_and_slug(&state.pool, &id)
        .await?
        .ok_or_else(|| AppError::NotFound("environment not found".into()))?;

    let var_name = crate::store::environments::var_name(&state.pool, &req.var_id)
        .await?
        .ok_or_else(|| AppError::NotFound("variable not found".into()))?;

    crate::store::environments::upsert_override(&state.pool, &id, &req.var_id, &req.value).await?;

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
    let (env_name, env_slug) = crate::store::environments::name_and_slug(&state.pool, &id)
        .await?
        .ok_or_else(|| AppError::NotFound("environment not found".into()))?;

    crate::store::environments::delete_override(&state.pool, &id, &var_id).await?;

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
    if !crate::store::environments::environment_exists(&state.pool, &id).await? {
        return Err(AppError::NotFound("environment not found".into()));
    }
    Ok(Json(ApiResponse::success(
        crate::store::environments::list_secret_overrides_json(&state.pool, &id).await?,
    )))
}

/// PUT /environments/{id}/secret-overrides — upsert one Secret override (encrypted at rest).
pub async fn upsert_secret_override(
    auth: PlatformAdmin,
    Path(id): Path<String>,
    State(state): State<AppState>,
    Json(req): Json<UpsertSecretOverrideRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let (env_name, env_slug) = crate::store::environments::name_and_slug(&state.pool, &id)
        .await?
        .ok_or_else(|| AppError::NotFound("environment not found".into()))?;

    let (secret_name, kind_raw) =
        crate::store::environments::secret_name_and_kind(&state.pool, &req.secret_id)
            .await?
            .ok_or_else(|| AppError::NotFound("secret not found".into()))?;
    let kind = shared::secret_kind::normalize_secret_kind(Some(&kind_raw));
    crate::store::environments::validate_secret_override_value(req.value.trim())?;

    let cipher = crypto::encrypt(req.value.trim(), &state.config.auth.jwt_secret)
        .map_err(|e| AppError::Internal(format!("encrypt secret override: {e}")))?;

    crate::store::environments::upsert_secret_override(&state.pool, &id, &req.secret_id, &cipher)
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
    let (env_name, env_slug) = crate::store::environments::name_and_slug(&state.pool, &id)
        .await?
        .ok_or_else(|| AppError::NotFound("environment not found".into()))?;

    crate::store::environments::delete_secret_override(&state.pool, &id, &secret_id).await?;

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
