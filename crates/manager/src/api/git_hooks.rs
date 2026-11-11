//! Pipeline Git webhook hook configuration — multiple hooks per pipeline.
//! Settings (label / branch / event filter / environment / enabled) are
//! updatable via PUT; provider, hook_key, and the secret are immutable —
//! changing those requires delete + recreate (new URL + secret).

use axum::{
    extract::{Path, State},
    Json,
};
use rand::RngCore;
use shared::error::AppError;
use shared::git_webhook::{normalize_provider, PROVIDER_GITEE, PROVIDER_GITHUB};
use shared::protocol::response::ApiResponse;
use shared::rbac::Perm;
use uuid::Uuid;

use crate::rbac;

use crate::audit::{self, Action, AuditEvent};
use crate::crypto;
use crate::rbac::AuthUser;
use crate::state::AppState;

fn random_hex(nbytes: usize) -> String {
    let mut buf = vec![0u8; nbytes];
    rand::thread_rng().fill_bytes(&mut buf);
    hex::encode(buf)
}

#[allow(clippy::too_many_arguments)] // handler context: state, path, body, headers, auth
fn public_hook_view(
    id: &str,
    label: &str,
    provider: &str,
    hook_key: &str,
    branch_filter: &str,
    event_filter: &str,
    environment: &str,
    enabled: bool,
    plaintext_secret: Option<&str>,
) -> serde_json::Value {
    let mut v = serde_json::json!({
        "id": id,
        "label": label,
        "provider": provider,
        "hook_key": hook_key,
        "url_path": format!("/api/v1/hooks/git/{provider}/{hook_key}"),
        "branch_filter": branch_filter,
        "event_filter": event_filter,
        "environment": environment,
        "enabled": enabled,
        "has_secret": true,
    });
    if let Some(s) = plaintext_secret {
        v["secret"] = serde_json::json!(s);
        v["secret_shown_once"] = serde_json::json!(true);
    }
    v
}

/// GET /pipelines/:id/git-hooks
pub async fn list_hooks(
    auth: AuthUser,
    Path(pipeline_id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let team_id = rbac::pipeline_team_id(&state.pool, &pipeline_id).await?;
    auth.check_team(&team_id, Perm::TeamView)?;

    let rows = crate::store::git_hooks::list_for_pipeline(&state.pool, &pipeline_id).await?;

    let items: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|h| {
            public_hook_view(
                &h.id,
                &h.label,
                &h.provider,
                &h.hook_key,
                &h.branch_filter,
                &h.event_filter,
                &h.environment,
                h.enabled,
                None,
            )
        })
        .collect();

    Ok(Json(ApiResponse::success(
        serde_json::json!({ "items": items }),
    )))
}

#[derive(Debug, serde::Deserialize)]
pub struct CreateGitHookRequest {
    /// `github` | `gitee`
    pub provider: String,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub branch_filter: Option<String>,
    /// Comma-separated normalized events this hook accepts: `push`
    /// (default), `pull_request`, or both.
    #[serde(default)]
    pub event_filter: Option<String>,
    pub environment: String,
    #[serde(default)]
    pub enabled: Option<bool>,
}

/// POST /pipelines/:id/git-hooks — create; secret returned once. No update API.
pub async fn create_hook(
    auth: AuthUser,
    Path(pipeline_id): Path<String>,
    State(state): State<AppState>,
    Json(req): Json<CreateGitHookRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let team_id = rbac::pipeline_team_id(&state.pool, &pipeline_id).await?;
    auth.check_team(&team_id, Perm::PipelineWrite)?;

    let provider = normalize_provider(&req.provider).ok_or_else(|| {
        AppError::ValidationFailed(format!(
            "provider must be '{PROVIDER_GITHUB}' or '{PROVIDER_GITEE}'"
        ))
    })?;
    let label = req.label.unwrap_or_default().trim().to_string();
    let branch_filter = req.branch_filter.unwrap_or_default();
    let event_filter = super::git_webhook::normalize_event_filter(req.event_filter.as_deref())?;
    let environment = req.environment.trim().to_string();
    if environment.is_empty() {
        return Err(AppError::ValidationFailed(
            "git hook environment is required".into(),
        ));
    }
    let declared_raw: String =
        crate::store::pipelines::declared_environments(&state.pool, &pipeline_id).await?;
    let declared: Vec<String> = serde_json::from_str(&declared_raw).unwrap_or_default();
    if !declared.iter().any(|slug| slug == &environment) {
        return Err(AppError::ValidationFailed(format!(
            "git hook environment '{environment}' is not declared by this pipeline"
        )));
    }
    let enabled = req.enabled.unwrap_or(true);

    let id = Uuid::new_v4().to_string();
    let hook_key = random_hex(12);
    let secret = random_hex(24);
    let cipher =
        crypto::encrypt(&secret, &state.config.auth.jwt_secret).map_err(AppError::Internal)?;

    crate::store::git_hooks::insert(
        &state.pool,
        &crate::store::git_hooks::GitHookInsert {
            id: id.clone(),
            pipeline_id: pipeline_id.clone(),
            label: label.clone(),
            provider: provider.to_string(),
            hook_key: hook_key.clone(),
            secret_ciphertext: cipher,
            branch_filter: branch_filter.clone(),
            environment: environment.clone(),
            enabled,
            event_filter: event_filter.clone(),
        },
    )
    .await?;

    let name = if label.is_empty() {
        format!("{provider}/{hook_key}")
    } else {
        label.clone()
    };
    audit::record(
        &state.pool,
        AuditEvent::from_auth(&auth, Action::GitHookCreate)
            .resource("git_hook", &id, &name)
            .team(Some(team_id.clone()))
            .after(Some(serde_json::json!({
                "id": &id,
                "pipeline_id": &pipeline_id,
                "label": &label,
                "provider": provider,
                "hook_key": &hook_key,
                "branch_filter": &branch_filter,
                "event_filter": &event_filter,
                "environment": &environment,
                "enabled": enabled,
            }))),
    )
    .await;

    Ok(Json(ApiResponse::success(public_hook_view(
        &id,
        &label,
        provider,
        &hook_key,
        &branch_filter,
        &event_filter,
        &environment,
        enabled,
        Some(secret.as_str()),
    ))))
}

/// DELETE /pipelines/:id/git-hooks/:hook_id
/// PUT /pipelines/:id/git-hooks/:hook_id — update mutable settings.
///
/// `label` / `branch_filter` / `event_filter` / `environment` / `enabled` are
/// updatable. Provider, hook key, and the signing secret are immutable — the
/// hook URL and secret only change by delete + recreate.
#[derive(Debug, serde::Deserialize)]
pub struct UpdateGitHookRequest {
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub branch_filter: Option<String>,
    /// `push` (default) | `pull_request` | both, comma-separated.
    #[serde(default)]
    pub event_filter: Option<String>,
    pub environment: String,
    #[serde(default)]
    pub enabled: Option<bool>,
}

pub async fn update_hook(
    auth: AuthUser,
    Path((pipeline_id, hook_id)): Path<(String, String)>,
    State(state): State<AppState>,
    Json(req): Json<UpdateGitHookRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let team_id = rbac::pipeline_team_id(&state.pool, &pipeline_id).await?;
    auth.check_team(&team_id, Perm::PipelineWrite)?;

    let label = req.label.unwrap_or_default().trim().to_string();
    let branch_filter = req.branch_filter.unwrap_or_default();
    let event_filter = super::git_webhook::normalize_event_filter(req.event_filter.as_deref())?;
    let environment = req.environment.trim().to_string();
    if environment.is_empty() {
        return Err(AppError::ValidationFailed(
            "git hook environment is required".into(),
        ));
    }
    let enabled = req.enabled.unwrap_or(true);

    let existing = crate::store::git_hooks::find_for_update(&state.pool, &hook_id, &pipeline_id)
        .await?
        .ok_or_else(|| AppError::NotFound("git hook not found".into()))?;
    let provider = existing.provider.clone();
    let hook_key = existing.hook_key.clone();
    let before = serde_json::json!({
        "label": existing.label,
        "branch_filter": existing.branch_filter,
        "event_filter": existing.event_filter,
        "environment": existing.environment,
        "enabled": existing.enabled,
    });

    crate::store::git_hooks::update_hook(
        &state.pool,
        &pipeline_id,
        &hook_id,
        &label,
        &branch_filter,
        &event_filter,
        &environment,
        enabled,
    )
    .await?;

    let name = if label.is_empty() {
        hook_id.clone()
    } else {
        label.clone()
    };
    audit::record(
        &state.pool,
        AuditEvent::from_auth(&auth, Action::GitHookUpdate)
            .resource("git_hook", &hook_id, &name)
            .team(Some(team_id.clone()))
            .before(Some(before))
            .after(Some(serde_json::json!({
                "label": &label,
                "branch_filter": &branch_filter,
                "event_filter": &event_filter,
                "environment": &environment,
                "enabled": enabled,
            }))),
    )
    .await;

    Ok(Json(ApiResponse::success(public_hook_view(
        &hook_id,
        &label,
        &provider,
        &hook_key,
        &branch_filter,
        &event_filter,
        &environment,
        enabled,
        None,
    ))))
}

pub async fn delete_hook(
    auth: AuthUser,
    Path((pipeline_id, hook_id)): Path<(String, String)>,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let team_id = rbac::pipeline_team_id(&state.pool, &pipeline_id).await?;
    auth.check_team(&team_id, Perm::PipelineWrite)?;
    let before_row =
        crate::store::git_hooks::delete_facts(&state.pool, &hook_id, &pipeline_id).await?;
    let before = before_row.as_ref().map(|f| {
        serde_json::json!({
            "id": hook_id,
            "pipeline_id": pipeline_id,
            "label": f.label,
            "provider": f.provider,
            "hook_key": f.hook_key,
            "branch_filter": f.branch_filter,
            "environment": f.environment,
            "enabled": f.enabled,
        })
    });
    let name = before_row
        .map(|f| {
            if f.label.is_empty() {
                format!("{}/{}", f.provider, f.hook_key)
            } else {
                f.label
            }
        })
        .unwrap_or_else(|| hook_id.clone());

    let deleted = crate::store::git_hooks::delete_hook(&state.pool, &hook_id, &pipeline_id).await?;
    if !deleted {
        return Err(AppError::NotFound("git hook not found".into()));
    }
    audit::record(
        &state.pool,
        AuditEvent::from_auth(&auth, Action::GitHookDelete)
            .resource("git_hook", &hook_id, &name)
            .team(Some(team_id))
            .before(before),
    )
    .await;
    Ok(Json(ApiResponse::success(
        serde_json::json!({ "deleted": true }),
    )))
}
