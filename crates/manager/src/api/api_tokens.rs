//! API token management (platform admins).
//!
//! Tiers:
//! - Super admin: create tokens for any admin, list/revoke every token.
//! - Regular platform admin: create/list/revoke only their own tokens.
//!
//! Tokens authenticate like a user JWT (`Authorization: Bearer <token>`).
//! Only the SHA-256 hash is stored; the plaintext is shown exactly once.

use axum::{
    extract::{Path, State},
    Json,
};
use shared::api_token;
use shared::error::AppError;
use shared::protocol::response::ApiResponse;
use shared::protocol::{CreateApiTokenRequest, CreateApiTokenResponse};
use sqlx::Row;
use uuid::Uuid;

use crate::audit::{self, Action, AuditEvent};
use crate::middleware::PlatformAdmin;
use crate::state::AppState;

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

    let id = Uuid::new_v4().to_string();
    let token = api_token::generate_token();
    let hash = api_token::hash_token(&token);
    let prefix = api_token::display_prefix(&token);

    sqlx::query(
        "INSERT INTO api_tokens (id, user_id, name, token_hash, token_prefix)
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&auth.id)
    .bind(name)
    .bind(&hash)
    .bind(&prefix)
    .execute(&state.pool)
    .await?;

    audit::record(
        &state.pool,
        AuditEvent::from_auth(&auth, Action::ApiTokenCreate)
            .resource("api_token", &id, name)
            .after(Some(serde_json::json!({
                "prefix": prefix,
                "created_by": auth.username,
            }))),
    )
    .await;

    tracing::info!(user = %auth.username, token_id = %id, "api token created");

    Ok(Json(ApiResponse::success(CreateApiTokenResponse {
        id,
        name: name.to_string(),
        token,
        prefix,
        created_at: chrono::Utc::now().to_rfc3339(),
    })))
}

/// GET /api-tokens — list tokens. Super admin sees all; regular admins only
/// their own.
pub async fn list(
    auth: PlatformAdmin,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let rows = if auth.is_super_admin {
        sqlx::query(
            "SELECT t.id, t.user_id, u.username, t.name, t.token_prefix,
                    t.created_at, t.last_used_at, t.revoked_at
             FROM api_tokens t
             LEFT JOIN users u ON u.id = t.user_id
             ORDER BY t.created_at DESC",
        )
        .fetch_all(&state.pool)
        .await?
    } else {
        sqlx::query(
            "SELECT t.id, t.user_id, u.username, t.name, t.token_prefix,
                    t.created_at, t.last_used_at, t.revoked_at
             FROM api_tokens t
             LEFT JOIN users u ON u.id = t.user_id
             WHERE t.user_id = ?
             ORDER BY t.created_at DESC",
        )
        .bind(&auth.id)
        .fetch_all(&state.pool)
        .await?
    };

    let items: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|r| {
            serde_json::json!({
                "id": r.get::<String, _>(0),
                "user_id": r.get::<String, _>(1),
                "username": r.get::<String, _>(2),
                "name": r.get::<String, _>(3),
                "prefix": r.get::<String, _>(4),
                "created_at": r.get::<String, _>(5),
                "last_used_at": r.get::<Option<String>, _>(6),
                "revoked_at": r.get::<Option<String>, _>(7),
                "revoked": r.get::<Option<String>, _>(7).is_some(),
            })
        })
        .collect();

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
    let row =
        sqlx::query("SELECT user_id, name FROM api_tokens WHERE id = ? AND revoked_at IS NULL")
            .bind(&id)
            .fetch_optional(&state.pool)
            .await?
            .ok_or_else(|| AppError::NotFound("api token not found".into()))?;

    let owner_id: String = row.get(0);
    let name: String = row.get(1);

    if !auth.is_super_admin && owner_id != auth.id {
        return Err(AppError::Forbidden(
            "only super admins can revoke another admin's api token".into(),
        ));
    }

    sqlx::query("UPDATE api_tokens SET revoked_at = datetime('now') WHERE id = ?")
        .bind(&id)
        .execute(&state.pool)
        .await?;

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
