use axum::{
    extract::FromRequestParts,
    http::{request::Parts, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use shared::auth::{Claims, Role, TokenManager};

use crate::rbac::load_auth_user;
use crate::state::AppState;

pub use crate::rbac::AuthUser;

/// Any authenticated human user (platform admin or team member).
pub type AuthUserExtractor = AuthUser;

impl FromRequestParts<AppState> for AuthUser {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let token = extract_bearer(parts)?;
        let tm = TokenManager::new(&state.config.auth.jwt_secret);
        let claims = tm
            .verify(token)
            .map_err(|_| auth_err("invalid or expired token"))?;

        if !matches!(claims.role, Role::Admin | Role::User) {
            return Err(forbidden_err("user role required"));
        }

        load_auth_user(&state.pool, &claims.sub)
            .await
            .map_err(|e| match e {
                shared::error::AppError::AuthFailed(msg) => auth_err(&msg),
                other => {
                    tracing::error!("load_auth_user failed: {other}");
                    auth_err("invalid or expired token")
                }
            })
    }
}

/// Platform administrator only (replaces former AdminClaims for infra routes).
#[derive(Debug, Clone)]
pub struct PlatformAdmin(pub AuthUser);

impl std::ops::Deref for PlatformAdmin {
    type Target = AuthUser;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl FromRequestParts<AppState> for PlatformAdmin {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let user = AuthUser::from_request_parts(parts, state).await?;
        if !user.is_platform_admin {
            return Err(forbidden_err("platform admin required"));
        }
        Ok(PlatformAdmin(user))
    }
}

/// Backward-compatible alias used by older handlers — means platform admin.
#[derive(Debug, Clone)]
pub struct AdminClaims {
    pub username: String,
    pub user: AuthUser,
}

impl FromRequestParts<AppState> for AdminClaims {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let PlatformAdmin(user) = PlatformAdmin::from_request_parts(parts, state).await?;
        Ok(AdminClaims {
            username: user.username.clone(),
            user,
        })
    }
}

/// Extracts Runner JWT claims from the Authorization header.
#[derive(Debug, Clone)]
pub struct RunnerClaims {
    pub runner_id: String,
    pub claims: Claims,
}

impl FromRequestParts<AppState> for RunnerClaims {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let token = extract_bearer(parts)?;
        let tm = TokenManager::new(&state.config.auth.jwt_secret);
        let claims = tm
            .verify(token)
            .map_err(|_| auth_err("invalid or expired token"))?;

        if claims.role != Role::Runner {
            return Err(forbidden_err("runner role required"));
        }

        Ok(RunnerClaims {
            runner_id: claims.sub.clone(),
            claims,
        })
    }
}

fn extract_bearer(parts: &Parts) -> Result<&str, Response> {
    let header = parts
        .headers
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| auth_err("missing Authorization header"))?;

    header
        .strip_prefix("Bearer ")
        .ok_or_else(|| auth_err("Authorization header must start with 'Bearer '"))
}

fn auth_err(message: &str) -> Response {
    let body = serde_json::json!({
        "code": 1002,
        "message": message,
        "data": serde_json::Value::Null,
    });
    (StatusCode::UNAUTHORIZED, Json(body)).into_response()
}

fn forbidden_err(message: &str) -> Response {
    let body = serde_json::json!({
        "code": 1003,
        "message": message,
        "data": serde_json::Value::Null,
    });
    (StatusCode::FORBIDDEN, Json(body)).into_response()
}
