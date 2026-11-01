//! Pipeline Git webhook hook configuration — multiple hooks per pipeline.
//! Hooks are immutable after create: delete and recreate to change settings.

use axum::{
    extract::{Path, State},
    Json,
};
use rand::RngCore;
use shared::error::AppError;
use shared::git_webhook::{normalize_provider, PROVIDER_GITEE, PROVIDER_GITHUB};
use shared::protocol::response::ApiResponse;
use shared::rbac::Perm;
use sqlx::Row;
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

async fn pipeline_team(pool: &sqlx::SqlitePool, pipeline_id: &str) -> Result<String, AppError> {
    // Shared resolver (same semantics, incl. the empty-team_id guard).
    rbac::pipeline_team_id(pool, pipeline_id).await
}

#[allow(clippy::too_many_arguments)] // handler context: state, path, body, headers, auth
fn public_hook_view(
    id: &str,
    label: &str,
    provider: &str,
    hook_key: &str,
    branch_filter: &str,
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
    let team_id = pipeline_team(&state.pool, &pipeline_id).await?;
    auth.check_team(&team_id, Perm::TeamView)?;

    let rows = sqlx::query(
        "SELECT id, label, provider, hook_key, branch_filter, enabled, environment
         FROM pipeline_git_hooks WHERE pipeline_id = ?
         ORDER BY created_at ASC",
    )
    .bind(&pipeline_id)
    .fetch_all(&state.pool)
    .await?;

    let items: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|r| {
            let id: String = r.get(0);
            let label: String = r.get(1);
            let provider: String = r.get(2);
            let hook_key: String = r.get(3);
            let branch_filter: String = r.get(4);
            let enabled: i64 = r.get(5);
            let environment: String = r.get(6);
            public_hook_view(
                &id,
                &label,
                &provider,
                &hook_key,
                &branch_filter,
                &environment,
                enabled != 0,
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
    let team_id = pipeline_team(&state.pool, &pipeline_id).await?;
    auth.check_team(&team_id, Perm::PipelineWrite)?;

    let provider = normalize_provider(&req.provider).ok_or_else(|| {
        AppError::ValidationFailed(format!(
            "provider must be '{PROVIDER_GITHUB}' or '{PROVIDER_GITEE}'"
        ))
    })?;
    let label = req.label.unwrap_or_default().trim().to_string();
    let branch_filter = req.branch_filter.unwrap_or_default();
    let environment = req.environment.trim().to_string();
    if environment.is_empty() {
        return Err(AppError::ValidationFailed(
            "git hook environment is required".into(),
        ));
    }
    let declared_raw: String =
        sqlx::query_scalar("SELECT environments FROM pipelines WHERE id = ?")
            .bind(&pipeline_id)
            .fetch_one(&state.pool)
            .await?;
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

    sqlx::query(
        "INSERT INTO pipeline_git_hooks
         (id, pipeline_id, label, provider, hook_key, secret_ciphertext, branch_filter, environment, enabled)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&pipeline_id)
    .bind(&label)
    .bind(provider)
    .bind(&hook_key)
    .bind(&cipher)
    .bind(&branch_filter)
    .bind(&environment)
    .bind(if enabled { 1 } else { 0 })
    .execute(&state.pool)
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
        &environment,
        enabled,
        Some(secret.as_str()),
    ))))
}

/// DELETE /pipelines/:id/git-hooks/:hook_id
pub async fn delete_hook(
    auth: AuthUser,
    Path((pipeline_id, hook_id)): Path<(String, String)>,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let team_id = pipeline_team(&state.pool, &pipeline_id).await?;
    auth.check_team(&team_id, Perm::PipelineWrite)?;
    let before_row = sqlx::query(
        "SELECT label, provider, hook_key, branch_filter, environment, enabled FROM pipeline_git_hooks
         WHERE id = ? AND pipeline_id = ?",
    )
    .bind(&hook_id)
    .bind(&pipeline_id)
    .fetch_optional(&state.pool)
    .await?;
    let before = before_row.as_ref().map(|r| {
        serde_json::json!({
            "id": hook_id,
            "pipeline_id": pipeline_id,
            "label": r.get::<String, _>(0),
            "provider": r.get::<String, _>(1),
            "hook_key": r.get::<String, _>(2),
            "branch_filter": r.get::<String, _>(3),
            "environment": r.get::<String, _>(4),
            "enabled": r.get::<i64, _>(5) != 0,
        })
    });
    let name = before_row
        .as_ref()
        .map(|r| {
            let label: String = r.get(0);
            let provider: String = r.get(1);
            let key: String = r.get(2);
            if label.is_empty() {
                format!("{provider}/{key}")
            } else {
                label
            }
        })
        .unwrap_or_else(|| hook_id.clone());

    let res = sqlx::query("DELETE FROM pipeline_git_hooks WHERE id = ? AND pipeline_id = ?")
        .bind(&hook_id)
        .bind(&pipeline_id)
        .execute(&state.pool)
        .await?;
    if res.rows_affected() == 0 {
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
