use axum::{extract::State, Json};
use shared::auth::{Role, TokenManager};
use shared::error::AppError;
use shared::protocol::response::ApiResponse;
use shared::protocol::{
    ChangePasswordRequest, LoginRequest, LoginResponse, SetupAdminRequest, SetupStatusResponse,
};
use sqlx::Row;
use uuid::Uuid;

use crate::audit::{self, Action, AuditEvent, Resource};
use crate::middleware::AuthUser;
use crate::password::{hash_password, validate_password_complexity, verify_password};
use crate::state::AppState;

pub async fn setup_status(
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<SetupStatusResponse>>, AppError> {
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users")
        .fetch_one(&state.pool)
        .await?;
    Ok(Json(ApiResponse::success(SetupStatusResponse {
        needs_setup: count == 0,
    })))
}

/// Validate first-admin setup fields.
pub(crate) fn validate_setup(req: &SetupAdminRequest) -> Result<(), AppError> {
    let username = req.username.trim();
    if username.is_empty() {
        return Err(AppError::ValidationFailed("username is required".into()));
    }
    if username.len() < 2 || username.len() > 64 {
        return Err(AppError::ValidationFailed(
            "username must be 2–64 characters".into(),
        ));
    }
    if !username
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
    {
        return Err(AppError::ValidationFailed(
            "username may only contain letters, digits, _ - .".into(),
        ));
    }
    if let Err(msg) = validate_password_complexity(&req.password) {
        return Err(AppError::ValidationFailed(msg));
    }
    Ok(())
}

/// Create the first platform admin when no users exist. Returns a login JWT.
pub async fn setup(
    State(state): State<AppState>,
    Json(req): Json<SetupAdminRequest>,
) -> Result<Json<ApiResponse<LoginResponse>>, AppError> {
    validate_setup(&req)?;
    let username = req.username.trim().to_string();

    let mut tx = state.pool.begin().await?;

    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users")
        .fetch_one(&mut *tx)
        .await?;
    if count > 0 {
        return Err(AppError::Conflict(
            "setup already completed — use login".into(),
        ));
    }

    let user_id = Uuid::new_v4().to_string();
    let hash = hash_password(&req.password).map_err(AppError::Internal)?;
    sqlx::query(
        "INSERT INTO users (id, username, password_hash, display_name, is_platform_admin, is_super_admin, is_active)
         VALUES (?, ?, ?, ?, 1, 1, 1)",
    )
    .bind(&user_id)
    .bind(&username)
    .bind(&hash)
    .bind(&username)
    .execute(&mut *tx)
    .await?;

    // Attach to default team (created by bootstrap)
    let team_id: Option<String> = sqlx::query_scalar("SELECT id FROM teams WHERE slug = 'default'")
        .fetch_optional(&mut *tx)
        .await?;
    if let Some(tid) = team_id {
        sqlx::query(
            "INSERT OR IGNORE INTO team_members (team_id, user_id, role) VALUES (?, ?, 'owner')",
        )
        .bind(&tid)
        .bind(&user_id)
        .execute(&mut *tx)
        .await?;
    }

    // No actor exists yet: the first admin is a system-provisioned account.
    audit::record_in(
        &mut *tx,
        AuditEvent::system(Action::AuthSetup)
            .on(Resource::user(&user_id, &username))
            .after(Some(serde_json::json!({
                "username": username,
                "is_super_admin": true,
            }))),
    )
    .await?;
    tx.commit().await?;

    tracing::info!("First platform admin created via web setup: {username}");

    let token_mgr = TokenManager::new(&state.config.auth.jwt_secret);
    let token = token_mgr
        .create_token(&user_id, Role::Admin, state.config.auth.jwt_ttl_hours)
        .map_err(|e| AppError::Internal(format!("token generation failed: {e}")))?;

    Ok(Json(ApiResponse::success(LoginResponse {
        token,
        user_id: Some(user_id),
        username: Some(username),
        is_platform_admin: Some(true),
    })))
}

pub async fn login(
    State(state): State<AppState>,
    Json(req): Json<LoginRequest>,
) -> Result<Json<ApiResponse<LoginResponse>>, AppError> {
    let user_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users")
        .fetch_one(&state.pool)
        .await?;
    if user_count == 0 {
        return Err(AppError::AuthFailed(
            "instance not initialized — open /setup to create the first admin".into(),
        ));
    }

    let username = req.username.trim();
    if username.is_empty() || req.password.is_empty() {
        return Err(AppError::AuthFailed("invalid username or password".into()));
    }

    let row = sqlx::query(
        "SELECT id, username, password_hash, is_platform_admin, is_active
         FROM users WHERE username = ?",
    )
    .bind(username)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::AuthFailed("invalid username or password".into()))?;

    let is_active: i64 = row.get(4);
    if is_active == 0 {
        return Err(AppError::AuthFailed("user is disabled".into()));
    }

    let password_hash: String = row.get(2);
    if !verify_password(&req.password, &password_hash) {
        tracing::warn!(
            username = %username,
            hash_prefix = %password_hash.chars().take(20).collect::<String>(),
            "login rejected: password verify failed"
        );
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

    audit::record(
        &state.pool,
        AuditEvent::actor(&user_id, &username, Action::AuthLogin)
            .on(Resource::user(&user_id, &username))
            .after(Some(serde_json::json!({
                "username": username.clone(),
                "is_platform_admin": is_platform_admin != 0,
            }))),
    )
    .await;

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
        "is_super_admin": auth.is_super_admin,
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
        return Err(AppError::ValidationFailed(
            "new_password is required".into(),
        ));
    }
    if let Err(msg) = validate_password_complexity(&req.new_password) {
        return Err(AppError::ValidationFailed(msg));
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
    sqlx::query("UPDATE users SET password_hash = ?, updated_at = datetime('now') WHERE id = ?")
        .bind(&new_hash)
        .bind(&auth.id)
        .execute(&state.pool)
        .await?;
    // The hash never enters the record; the event alone is the signal.
    audit::record(
        &state.pool,
        AuditEvent::new(Action::AuthChangePassword, &auth)
            .on(Resource::user(&auth.id, &auth.username)),
    )
    .await;

    Ok(Json(ApiResponse::success(())))
}

#[cfg(test)]
#[path = "tests/auth_change_password_tests.rs"]
mod tests;
