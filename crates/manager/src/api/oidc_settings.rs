//! Web-editable OIDC SSO configuration (Phase 1.5; design:
//! `platform/oidc-sso.md`).
//!
//! Precedence **database > `[auth.oidc]` file > defaults**. A `PUT` validates,
//! encrypts the client secret at rest (`buzenc:v1:`), saves the singleton row,
//! and swaps the live runtime in place — enabled/disabled/changed without a
//! restart. `DELETE` drops the override so the file seed applies again.
//! `POST …/test` probes the provider's discovery document without saving.
//!
//! Secret discipline: the plaintext client secret exists only inside the PUT
//! request; responses carry `client_secret_configured` booleans, never values;
//! audit records the actor and the issuer, never the secret.

use axum::{extract::State, Json};
use serde::Deserialize;
use shared::error::AppError;
use shared::protocol::response::ApiResponse;

use crate::audit::{self, Action, AuditEvent, Resource};
use crate::middleware::PlatformAdmin;
use crate::services::oidc::{self, EffectiveOidc, OidcRuntime};
use crate::state::AppState;

/// `GET /api/v1/auth/oidc/settings` — the effective configuration with the
/// secret masked, plus where each piece comes from and whether it validates.
pub async fn get_settings(
    _auth: PlatformAdmin,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let row = crate::store::oidc_settings::get(&state.pool).await?;
    let effective = resolve_for_state(&state, row.as_ref())?;
    Ok(Json(ApiResponse::success(
        settings_payload(&state, &effective, row.as_ref()).await,
    )))
}

#[derive(Deserialize)]
pub struct UpdateOidcSettingsRequest {
    pub enabled: bool,
    pub issuer: String,
    pub client_id: String,
    /// `None` or empty keeps the previously stored secret (row) — or, on a
    /// first save, the file/env secret. A non-empty value replaces it.
    #[serde(default)]
    pub client_secret: Option<String>,
    /// `None` or empty derives the callback from `server.public_base_url`.
    #[serde(default)]
    pub redirect_uri_override: Option<String>,
    #[serde(default)]
    pub scopes: Option<Vec<String>>,
    #[serde(default = "default_true")]
    pub auto_provision: bool,
}

fn default_true() -> bool {
    true
}

/// `PUT /api/v1/auth/oidc/settings` — validate, encrypt, save, hot-swap.
pub async fn update_settings(
    auth: PlatformAdmin,
    State(state): State<AppState>,
    Json(req): Json<UpdateOidcSettingsRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let row_before = crate::store::oidc_settings::get(&state.pool).await?;

    // Resolve the secret: keep the stored one unless the request carries a
    // replacement; fall back to the file/env secret on a first save.
    let client_secret = match req.client_secret.as_deref().map(str::trim) {
        Some(secret) if !secret.is_empty() => secret.to_string(),
        _ => match row_before.as_ref() {
            Some(row) if !row.client_secret_ciphertext.is_empty() => {
                crate::crypto::decrypt(&row.client_secret_ciphertext, &state.config.auth.jwt_secret)
                    .map_err(|e| {
                        AppError::Internal(format!(
                            "stored client secret unreadable — re-enter it: {e}"
                        ))
                    })?
            }
            _ => state
                .config
                .auth
                .oidc
                .resolved_client_secret()
                .map_err(AppError::ValidationFailed)?,
        },
    };

    let scopes = req
        .scopes
        .unwrap_or_else(|| state.config.auth.oidc.scopes.clone());
    let effective = EffectiveOidc {
        enabled: req.enabled,
        issuer: req.issuer.trim().trim_end_matches('/').to_string(),
        client_id: req.client_id.trim().to_string(),
        client_secret,
        redirect_uri: String::new(), // resolved below
        scopes,
        auto_provision: req.auto_provision,
        source: "database",
    };
    let redirect_uri = match req.redirect_uri_override.as_deref().map(str::trim) {
        Some(override_uri) if !override_uri.is_empty() => override_uri.to_string(),
        _ => EffectiveOidc::derived_redirect_uri(&state.config.server.public_base_url)
            .map_err(AppError::ValidationFailed)?,
    };
    let effective = EffectiveOidc {
        redirect_uri,
        ..effective
    };
    effective.validate().map_err(AppError::ValidationFailed)?;

    // Encrypt at rest and persist.
    let ciphertext = if effective.client_secret.is_empty() {
        String::new()
    } else {
        crate::crypto::encrypt(&effective.client_secret, &state.config.auth.jwt_secret)
            .map_err(|e| AppError::Internal(format!("secret encryption failed: {e}")))?
    };
    crate::store::oidc_settings::upsert(
        &state.pool,
        effective.enabled,
        &effective.issuer,
        &effective.client_id,
        &ciphertext,
        req.redirect_uri_override.as_deref().map(str::trim),
        &serde_json::to_string(&effective.scopes).unwrap_or_else(|_| "[]".into()),
        effective.auto_provision,
        &auth.username,
    )
    .await?;

    // Rebuild the runtime and swap it in: enabled/changed without a restart.
    let runtime = oidc::OidcRuntime::build_effective(&effective).await;
    state.set_oidc_runtime(runtime).await;

    audit::record(
        &state.pool,
        AuditEvent::new(Action::SettingUpdate, &auth)
            .on(Resource::Setting {
                key: "auth.oidc",
            })
            .after(Some(serde_json::json!({
                "enabled": effective.enabled,
                "issuer": effective.issuer,
                "client_id": effective.client_id,
                "auto_provision": effective.auto_provision,
                "secret_changed": req.client_secret.as_deref().map(|s| !s.trim().is_empty()).unwrap_or(false),
            }))),
    )
    .await;

    let row = crate::store::oidc_settings::get(&state.pool).await?;
    let effective = resolve_for_state(&state, row.as_ref())?;
    Ok(Json(ApiResponse::success(
        settings_payload(&state, &effective, row.as_ref()).await,
    )))
}

/// `DELETE /api/v1/auth/oidc/settings` — drop the override; the file seed
/// applies again. Refuses to disable an otherwise-valid SSO silently: the
/// response reports the resulting state.
pub async fn reset_settings(
    auth: PlatformAdmin,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let existed = crate::store::oidc_settings::delete(&state.pool).await?;

    let effective = EffectiveOidc::from(&state.config.auth.oidc);
    let runtime = OidcRuntime::build_effective(&effective).await;
    state.set_oidc_runtime(runtime).await;

    if existed {
        audit::record(
            &state.pool,
            AuditEvent::new(Action::SettingReset, &auth).on(Resource::Setting { key: "auth.oidc" }),
        )
        .await;
    }

    Ok(Json(ApiResponse::success(
        settings_payload(&state, &effective, None).await,
    )))
}

#[derive(Deserialize)]
pub struct TestOidcRequest {
    pub issuer: String,
}

/// `POST /api/v1/auth/oidc/settings/test` — probe the provider's discovery
/// document without saving anything (the UI's "Test connection").
pub async fn test_settings(
    _auth: PlatformAdmin,
    Json(req): Json<TestOidcRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    match oidc::probe_discovery(&req.issuer).await {
        Ok(probe) => Ok(Json(ApiResponse::success(serde_json::json!({
            "ok": true,
            "issuer_match": probe.issuer_match,
            "discovered_issuer": probe.discovered_issuer,
            "authorization_endpoint": probe.authorization_endpoint,
            "token_endpoint": probe.token_endpoint,
            "jwks_uri": probe.jwks_uri,
        })))),
        Err(error) => Ok(Json(ApiResponse::success(serde_json::json!({
            "ok": false,
            "error": error,
        })))),
    }
}

/// Resolve for the current state: the row, then the file seed; resolution
/// errors (e.g. undecryptable secret) surface as `validation_error` in the
/// payload instead of failing the GET — the UI must be able to show them.
fn resolve_for_state(
    state: &AppState,
    row: Option<&crate::store::oidc_settings::OidcSettingsRow>,
) -> Result<EffectiveOidc, AppError> {
    oidc::resolve_effective(
        &state.config.auth.oidc,
        row,
        &state.config.server.public_base_url,
        &state.config.auth.jwt_secret,
    )
    .map_err(|error| {
        tracing::warn!(error = %error, "oidc settings resolve failed");
        AppError::Internal(error)
    })
}

/// The masked settings payload: `client_secret_configured` instead of the
/// secret, plus the derivation inputs so the UI can explain the effective
/// callback URL.
async fn settings_payload(
    state: &AppState,
    effective: &EffectiveOidc,
    row: Option<&crate::store::oidc_settings::OidcSettingsRow>,
) -> serde_json::Value {
    let secret_configured = match row {
        Some(row) => !row.client_secret_ciphertext.is_empty(),
        None => state.config.auth.oidc.resolved_client_secret().is_ok(),
    };
    serde_json::json!({
        "source": effective.source,
        "enabled": effective.enabled,
        "issuer": effective.issuer,
        "client_id": effective.client_id,
        "client_secret_configured": secret_configured,
        "redirect_uri_effective": effective.redirect_uri,
        "redirect_uri_override": row.as_ref().and_then(|r| r.redirect_uri_override.clone()),
        "public_base_url": state.config.server.public_base_url,
        "derived_redirect_uri": EffectiveOidc::derived_redirect_uri(&state.config.server.public_base_url).ok(),
        "backchannel_logout_uri": EffectiveOidc::derived_backchannel_logout_uri(&state.config.server.public_base_url).ok(),
        "scopes": effective.scopes,
        "auto_provision": effective.auto_provision,
        "updated_by": row.as_ref().map(|r| r.updated_by.clone()).unwrap_or_default(),
        "active": state.oidc_runtime().await.is_some(),
        "validation": match effective.validate() {
            Ok(()) => serde_json::json!({ "ok": true }),
            Err(error) => serde_json::json!({ "ok": false, "error": error }),
        },
    })
}
