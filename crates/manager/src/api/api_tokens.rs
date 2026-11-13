//! API token management (platform admins).
//!
//! Tiers:
//! - Super admin: list/revoke every token.
//! - Regular platform admin: create/list/revoke only their own tokens.
//!
//! Tokens authenticate like a user JWT (`Authorization: Bearer <token>`) but
//! are operations-plane credentials: every scope is denied the identity plane
//! (users, api-tokens, password change) and the governance plane (settings,
//! plugin sources/pool). New tokens pick a band — `read` (safe methods only)
//! or `ops` (read/write); the creator-equivalent `full` band is closed to new
//! tokens and only exists on pre-scope rows until revoked. Only the SHA-256
//! hash is stored; the plaintext is shown exactly once.

use axum::{
    extract::{Path, State},
    Json,
};
use shared::api_token::{self, ApiTokenScope};
use shared::error::AppError;
use shared::protocol::response::ApiResponse;
use shared::protocol::{CreateApiTokenRequest, CreateApiTokenResponse};
use uuid::Uuid;

use crate::audit::{self, Action, AuditEvent};
use crate::middleware::PlatformAdmin;
use crate::state::AppState;
use crate::store::api_tokens as token_store;

/// POST /api-tokens — create a token bound to the calling admin. Super admins
/// can then list/revoke every token platform-wide.
pub async fn create(
    auth: PlatformAdmin,
    State(state): State<AppState>,
    Json(req): Json<CreateApiTokenRequest>,
) -> Result<Json<ApiResponse<CreateApiTokenResponse>>, AppError> {
    let name = req.name.trim();
    if name.is_empty() || name.len() > 64 {
        return Err(AppError::ValidationFailed(
            "api token name is required (max 64 chars)".into(),
        ));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
    {
        return Err(AppError::ValidationFailed(
            "api token name may only contain letters, digits, - _ .".into(),
        ));
    }

    // Scope band: default `read` (least privilege), `ops` for read/write
    // automation. `full` is the legacy band of pre-scope tokens — closed to
    // new tokens.
    let scope = match req.scope.as_deref().unwrap_or("read") {
        "read" => ApiTokenScope::Read,
        "ops" => ApiTokenScope::Ops,
        "full" => {
            return Err(AppError::ValidationFailed(
                "scope 'full' is legacy-only and closed to new tokens — use 'read' or 'ops'".into(),
            ));
        }
        other => {
            return Err(AppError::ValidationFailed(format!(
                "unknown api token scope {other:?} — use 'read' or 'ops'"
            )));
        }
    };

    let id = Uuid::new_v4().to_string();
    let token = api_token::generate_token();
    let hash = api_token::hash_token(&token);
    let prefix = api_token::display_prefix(&token);

    token_store::insert(
        &state.pool,
        &id,
        &auth.id,
        name,
        &hash,
        &prefix,
        scope.as_str(),
    )
    .await?;

    audit::record(
        &state.pool,
        AuditEvent::from_auth(&auth, Action::ApiTokenCreate)
            .resource("api_token", &id, name)
            .after(Some(serde_json::json!({
                "prefix": prefix,
                "scope": scope.as_str(),
                "created_by": auth.username,
            }))),
    )
    .await;

    tracing::info!(user = %auth.username, token_id = %id, scope = %scope.as_str(), "api token created");

    Ok(Json(ApiResponse::success(CreateApiTokenResponse {
        id,
        name: name.to_string(),
        token,
        prefix,
        created_at: chrono::Utc::now().to_rfc3339(),
        scope: scope.as_str().to_string(),
    })))
}

/// GET /api-tokens — list tokens. Super admin sees all; regular admins only
/// their own.
pub async fn list(
    auth: PlatformAdmin,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let items = token_store::list(
        &state.pool,
        (!auth.is_super_admin).then_some(auth.id.as_str()),
    )
    .await?;

    Ok(Json(ApiResponse::success(
        serde_json::json!({ "items": items }),
    )))
}

/// DELETE /api-tokens/{id} — revoke a token (own, or any when super admin).
pub async fn revoke(
    auth: PlatformAdmin,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    let (owner_id, name) = token_store::find_active(&state.pool, &id)
        .await?
        .ok_or_else(|| AppError::NotFound("api token not found".into()))?;

    if !auth.is_super_admin && owner_id != auth.id {
        return Err(AppError::Forbidden(
            "only super admins can revoke another admin's api token".into(),
        ));
    }

    token_store::revoke(&state.pool, &id).await?;

    audit::record(
        &state.pool,
        AuditEvent::from_auth(&auth, Action::ApiTokenRevoke)
            .resource("api_token", &id, &name)
            .meta(Some(serde_json::json!({ "revoked_by": auth.username }))),
    )
    .await;

    tracing::info!(user = %auth.username, token_id = %id, "api token revoked");

    Ok(Json(ApiResponse::success(())))
}
