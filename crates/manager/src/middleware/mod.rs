use axum::{
    extract::FromRequestParts,
    http::{request::Parts, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use shared::auth::{Role, TokenManager};

use crate::state::AppState;

pub use crate::rbac::AuthUser;

impl FromRequestParts<AppState> for AuthUser {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let token = extract_bearer(parts)?;
        let user =
            crate::rbac::authenticate_user_token(&state.pool, &state.config.auth.jwt_secret, token)
                .await
                .map_err(|e| match e {
                    shared::error::AppError::AuthFailed(msg) => auth_err(&msg),
                    other => {
                        tracing::error!("authenticate_user_token failed: {other}");
                        auth_err("invalid or expired token")
                    }
                })?;

        // API-token authority bands (identity/governance planes + read
        // method restriction). JWT sessions pass through.
        user.assert_api_token_allowed(parts.method.as_str(), parts.uri.path())
            .map_err(|e| match e {
                shared::error::AppError::Forbidden(msg) => forbidden_err(&msg),
                other => {
                    tracing::error!("assert_api_token_allowed failed: {other}");
                    forbidden_err("api token not allowed here")
                }
            })?;

        Ok(user)
    }
}

/// Platform administrator only.
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

/// Platform administrator **or** platform-ops user. Gates the ops API
/// namespace (sandbox host administration); grants nothing else — every other
/// namespace keeps its own extractor.
#[derive(Debug, Clone)]
pub struct OpsAdmin(pub AuthUser);

impl std::ops::Deref for OpsAdmin {
    type Target = AuthUser;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl FromRequestParts<AppState> for OpsAdmin {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let user = AuthUser::from_request_parts(parts, state).await?;
        if !user.is_ops {
            return Err(forbidden_err("ops or platform admin required"));
        }
        Ok(OpsAdmin(user))
    }
}

/// Authenticated runner identity extracted from a Runner JWT.
#[derive(Debug, Clone)]
pub struct RunnerClaims {
    pub runner_id: String,
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
            runner_id: claims.sub,
        })
    }
}

/// Authenticated sandboxd instance, resolved from its opaque `buzsd_…` bearer
/// token (SHA-256 looked up in `sandbox_hosts.token_hash`). Scoped to exactly
/// one `(host_id, instance_id)`: the token can fetch that host's configuration
/// and report its status — nothing else. It is never a user and never a Runner.
#[derive(Debug, Clone)]
pub struct SandboxdClaims {
    pub host_id: String,
    pub instance_id: String,
}

impl FromRequestParts<AppState> for SandboxdClaims {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let token = extract_bearer(parts)?;
        if !token.starts_with("buzsd_") {
            return Err(forbidden_err("sandboxd token required"));
        }
        let token_hash = shared::api_token::hash_token(token);
        let row: Option<(String, String)> = sqlx::query_as(
            "SELECT host_id, instance_id FROM sandbox_hosts WHERE token_hash = ? AND token_hash != ''",
        )
        .bind(&token_hash)
        .fetch_optional(&state.pool)
        .await
        .map_err(|e| {
            tracing::error!("sandboxd token lookup failed: {e}");
            auth_err("invalid or revoked token")
        })?;
        let Some((host_id, instance_id)) = row else {
            return Err(auth_err("invalid or revoked token"));
        };
        Ok(SandboxdClaims {
            host_id,
            instance_id,
        })
    }
}

#[allow(clippy::result_large_err)] // Axum extractors commonly use Response as Err
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
