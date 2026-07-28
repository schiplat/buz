use axum::{Json, extract::State};
use shared::protocol::{ChangePasswordRequest, LoginRequest, LoginResponse};
use shared::protocol::response::ApiResponse;
use shared::auth::{TokenManager, Role};
use shared::error::AppError;
use sqlx::Row;

use crate::password::{hash_password, verify_password};
use crate::state::AppState;
use crate::middleware::AuthUser;

pub async fn login(
    State(state): State<AppState>,
    Json(req): Json<LoginRequest>,
) -> Result<Json<ApiResponse<LoginResponse>>, AppError> {
    let row = sqlx::query(
        "SELECT id, username, password_hash, is_platform_admin, is_active
         FROM users WHERE username = ?",
    )
    .bind(&req.username)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::AuthFailed("invalid username or password".into()))?;

    let is_active: i64 = row.get(4);
    if is_active == 0 {
        return Err(AppError::AuthFailed("user is disabled".into()));
    }

    let password_hash: String = row.get(2);
    if !verify_password(&req.password, &password_hash) {
        return Err(AppError::AuthFailed("invalid username or password".into()));
    }

    let user_id: String = row.get(0);
    let username: String = row.get(1);
    let is_platform_admin: i64 = row.get(3);
    let role = if is_platform_admin != 0 {
        Role::Admin
    } else {
        Role::User
    };

    let token_mgr = TokenManager::new(&state.config.auth.jwt_secret);
    let token = token_mgr
        .create_token(&user_id, role, state.config.auth.jwt_ttl_hours)
        .map_err(|e| AppError::Internal(format!("token generation failed: {e}")))?;

    Ok(Json(ApiResponse::success(LoginResponse {
        token,
        user_id: Some(user_id),
        username: Some(username),
        is_platform_admin: Some(is_platform_admin != 0),
    })))
}

pub async fn verify(auth: AuthUser) -> Result<Json<ApiResponse<()>>, AppError> {
    let _ = auth;
    Ok(Json(ApiResponse::success(())))
}

pub async fn me(auth: AuthUser) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let teams: Vec<serde_json::Value> = auth
        .memberships
        .iter()
        .map(|m| {
            serde_json::json!({
                "id": m.team_id,
                "slug": m.team_slug,
                "name": m.team_name,
                "role": m.role.as_str(),
            })
        })
        .collect();

    Ok(Json(ApiResponse::success(serde_json::json!({
        "id": auth.id,
        "username": auth.username,
        "is_platform_admin": auth.is_platform_admin,
        "teams": teams,
    }))))
}

/// Validate change-password fields before hitting the database.
pub(crate) fn validate_change_password(req: &ChangePasswordRequest) -> Result<(), AppError> {
    if req.current_password.is_empty() {
        return Err(AppError::ValidationFailed(
            "current_password is required".into(),
        ));
    }
    if req.new_password.is_empty() {
        return Err(AppError::ValidationFailed("new_password is required".into()));
    }
    if req.new_password.len() < 6 {
        return Err(AppError::ValidationFailed(
            "new_password must be at least 6 characters".into(),
        ));
    }
    if req.current_password == req.new_password {
        return Err(AppError::ValidationFailed(
            "new_password must differ from current_password".into(),
        ));
    }
    Ok(())
}

pub async fn change_password(
    auth: AuthUser,
    State(state): State<AppState>,
    Json(req): Json<ChangePasswordRequest>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    validate_change_password(&req)?;

    let row = sqlx::query("SELECT password_hash FROM users WHERE id = ?")
        .bind(&auth.id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(|| AppError::NotFound("user not found".into()))?;

    let password_hash: String = row.get(0);
    if !verify_password(&req.current_password, &password_hash) {
        return Err(AppError::ValidationFailed(
            "current password is incorrect".into(),
        ));
    }

    let new_hash = hash_password(&req.new_password).map_err(AppError::Internal)?;
    sqlx::query(
        "UPDATE users SET password_hash = ?, updated_at = datetime('now') WHERE id = ?",
    )
    .bind(&new_hash)
    .bind(&auth.id)
    .execute(&state.pool)
    .await?;

    Ok(Json(ApiResponse::success(())))
}

#[cfg(test)]
#[path = "tests/auth_change_password_tests.rs"]
mod tests;
