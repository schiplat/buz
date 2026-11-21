//! Web-editable session policy: user-JWT lifetime + client-side idle timeout.
//!
//! Two different enforcement models, deliberately kept apart:
//!
//! - `session_ttl_hours` is enforced **server-side** at token mint. Changing it
//!   only affects tokens issued afterwards; already-issued JWTs keep their
//!   `exp` (there is no server-side session table — see `platform/oidc-sso.md`).
//! - `session_idle_minutes` is enforced by the **SPA only**: after that much
//!   inactivity the browser clears its token and returns to the login page.
//!   It is a convenience, not a revocation: a stolen token stays valid until
//!   its `exp` (or an IdP-initiated account-wide revocation).
//!
//! Both live in `platform_settings` (DB override > manager.toml default) so the
//! existing validation/audit/override semantics apply. `GET` is available to
//! every signed-in user (the SPA needs the idle value); `PUT` is platform-
//! administrator only.

use axum::{extract::State, Json};
use serde::Deserialize;
use shared::error::AppError;
use shared::protocol::response::ApiResponse;

use crate::audit::{self, Action, AuditEvent, Resource};
use crate::middleware::{AuthUser, PlatformAdmin};
use crate::services::platform_settings::{self, KEY_SESSION_IDLE_MINUTES, KEY_SESSION_TTL_HOURS};
use crate::state::AppState;

#[derive(Deserialize)]
pub struct UpdateSessionPolicyRequest {
    /// `None` (or `null`) clears the override and falls back to
    /// `[auth] jwt_ttl_hours` in manager.toml.
    #[serde(default)]
    pub ttl_hours: Option<i64>,
    /// `None` (or `null`) clears the override; `0` means "disabled".
    #[serde(default)]
    pub idle_minutes: Option<i64>,
}

/// `GET /api/v1/auth/session-policy` — effective policy for the SPA.
pub async fn get_policy(
    _auth: AuthUser,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    Ok(Json(ApiResponse::success(policy_payload(&state).await)))
}

/// `PUT /api/v1/auth/session-policy` — platform administrator only. Both
/// fields are authoritative: a value saves an override, `null` resets that key
/// to the manager.toml default.
pub async fn update_policy(
    auth: PlatformAdmin,
    State(state): State<AppState>,
    Json(req): Json<UpdateSessionPolicyRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    save_key(&state, &auth, KEY_SESSION_TTL_HOURS, req.ttl_hours).await?;
    save_key(&state, &auth, KEY_SESSION_IDLE_MINUTES, req.idle_minutes).await?;
    Ok(Json(ApiResponse::success(policy_payload(&state).await)))
}

async fn policy_payload(state: &AppState) -> serde_json::Value {
    let overrides = platform_settings::fetch_overrides(&state.pool).await;
    let ttl_default = state.config.auth.jwt_ttl_hours;
    serde_json::json!({
        "ttl_hours": platform_settings::session_ttl_hours(&state.pool, ttl_default).await,
        "ttl_default_hours": ttl_default,
        "ttl_overridden": overrides.contains_key(KEY_SESSION_TTL_HOURS),
        "idle_minutes": platform_settings::session_idle_minutes(&state.pool).await,
        "idle_default_minutes": 0,
        "idle_overridden": overrides.contains_key(KEY_SESSION_IDLE_MINUTES),
    })
}

/// Upsert one key (validated), or clear it when `value` is `None`. Records an
/// audit event only when the stored state actually changes.
async fn save_key(
    state: &AppState,
    auth: &PlatformAdmin,
    key: &'static str,
    value: Option<i64>,
) -> Result<(), AppError> {
    let previous = platform_settings::fetch_overrides(&state.pool)
        .await
        .get(key)
        .cloned();
    match value {
        Some(value) => {
            platform_settings::upsert(&state.pool, key, &value.to_string(), &auth.username)
                .await
                .map_err(|error| AppError::ValidationFailed(error.to_string()))?;
            audit::record(
                &state.pool,
                AuditEvent::new(Action::SettingUpdate, auth)
                    .on(Resource::setting(key))
                    .changes(
                        Some(serde_json::json!({ "from": previous })),
                        Some(serde_json::json!({ "to": value.to_string() })),
                    ),
            )
            .await;
            Ok(())
        }
        None if previous.is_some() => {
            platform_settings::delete(&state.pool, key)
                .await
                .map_err(|error| AppError::Internal(error.to_string()))?;
            audit::record(
                &state.pool,
                AuditEvent::new(Action::SettingReset, auth).on(Resource::setting(key)),
            )
            .await;
            Ok(())
        }
        None => Ok(()),
    }
}
