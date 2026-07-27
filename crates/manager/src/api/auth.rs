use axum::{Json, extract::State};
use shared::protocol::{LoginRequest, LoginResponse};
use shared::protocol::response::ApiResponse;
use shared::auth::{TokenManager, Role};
use shared::error::AppError;

use crate::api::AppState;

pub async fn login(
    State(state): State<AppState>,
    Json(req): Json<LoginRequest>,
) -> Result<Json<ApiResponse<LoginResponse>>, AppError> {
    // MVP: hardcoded admin check - replace with proper password hashing in Phase 2
    if req.username != state.config.auth.admin_user
        || req.password != state.config.auth.admin_password
    {
        return Err(AppError::AuthFailed("invalid username or password".into()));
    }

    let token_mgr = TokenManager::new(&state.config.auth.jwt_secret);
    let token = token_mgr
        .create_token(&req.username, Role::Admin, state.config.auth.jwt_ttl_hours)
        .map_err(|e| AppError::Internal(format!("token generation failed: {e}")))?;

    Ok(Json(ApiResponse::success(LoginResponse { token })))
}
