use axum::{
    extract::FromRequestParts,
    http::{request::Parts, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use shared::auth::{Claims, Role, TokenManager};

use crate::api::AppState;

/// Extracts Admin JWT claims from the Authorization header.
/// Fails with 401 if the token is missing, invalid, expired, or not Admin.
#[derive(Debug, Clone)]
pub struct AdminClaims {
    pub username: String,
    pub claims: Claims,
}

impl FromRequestParts<AppState> for AdminClaims {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        let token = extract_bearer(parts)?;
        let tm = TokenManager::new(&state.config.auth.jwt_secret);
        let claims = tm.verify(token).map_err(|_| auth_err("invalid or expired token"))?;

        if claims.role != Role::Admin {
            return Err(forbidden_err("admin role required"));
        }

        Ok(AdminClaims {
            username: claims.sub.clone(),
            claims,
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

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        let token = extract_bearer(parts)?;
        let tm = TokenManager::new(&state.config.auth.jwt_secret);
        let claims = tm.verify(token).map_err(|_| auth_err("invalid or expired token"))?;

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
