//! Web-editable OIDC SSO configuration (design: `platform/oidc-sso.md`).
//!
//! **The database is the only source.** A `PUT` validates, encrypts the
//! client secret at rest (`buzenc:v1:`), saves the singleton row, and swaps
//! the live runtime in place — enabled/disabled/changed without a restart.
//! `DELETE` clears the configuration (SSO off until saved again). The
//! `[auth.oidc]` section of manager.toml is a legacy seed: it is imported
//! into the row once at startup (`import_file_seed`) and afterwards ignored.
//! `POST …/test` probes the provider's discovery document without saving.
//!
//! Secret discipline: the plaintext client secret exists only inside the PUT
//! request (and the import); responses carry `client_secret_configured`
//! booleans, never values; audit records the actor and the issuer, never the
//! secret.

use axum::{extract::State, Json};
use serde::Deserialize;
use shared::error::AppError;
use shared::protocol::response::ApiResponse;

use crate::audit::{self, Action, AuditEvent, Resource};
use crate::middleware::PlatformAdmin;
use crate::services::oidc::{self, EffectiveOidc, OidcRuntime};
use crate::state::AppState;

/// One-time migration: import the legacy `[auth.oidc]` file seed into the
/// database row when the file section is configured and no row exists yet.
/// Called once at startup, before the runtime is built. The file section is
/// never consulted again afterwards — the database row is authoritative.
///
/// The import keeps `enabled` as-is: a deployment whose file seed had SSO on
/// keeps it on with identical values. `updated_by = "file-import"` marks the
/// row's origin. Failures are logged and skipped — the web form remains the
/// recovery path, and local login is never affected.
pub async fn import_file_seed(
    pool: &crate::store::exec::DbPool,
    config: &crate::config::OidcConfig,
    public_base_url: &str,
    jwt_secret: &str,
) {
    let file_configures_sso = config.issuer.trim().is_empty() || config.client_id.trim().is_empty();
    if file_configures_sso {
        return;
    }
    match crate::store::oidc_settings::get(pool).await {
        Ok(Some(_)) => return, // a saved row already exists — file section ignored
        Ok(None) => {}
        Err(error) => {
            tracing::warn!(error = %error, "oidc settings unreadable — file seed not imported");
            return;
        }
    }

    let client_secret = match config.resolved_client_secret() {
        Ok(secret) => secret,
        Err(error) => {
            tracing::warn!(
                error = %error,
                "[auth.oidc] has no usable client secret — file seed not imported"
            );
            return;
        }
    };
    let effective = EffectiveOidc {
        enabled: config.enabled,
        issuer: config.issuer.trim().trim_end_matches('/').to_string(),
        client_id: config.client_id.trim().to_string(),
        redirect_uri: String::new(), // derived during validation below
        scopes: config.scopes.clone(),
        auto_provision: config.auto_provision,
        source: "database",
        client_secret,
    };
    let effective = match (|| -> Result<EffectiveOidc, String> {
        let redirect_uri = match config.redirect_uri.trim() {
            "" => EffectiveOidc::derived_redirect_uri(public_base_url)?,
            uri => uri.to_string(),
        };
        let effective = EffectiveOidc {
            redirect_uri,
            ..effective
        };
        effective.validate()?;
        Ok(effective)
    })() {
        Ok(effective) => effective,
        Err(error) => {
            tracing::warn!(
                error = %error,
                "[auth.oidc] configuration invalid — file seed not imported"
            );
            return;
        }
    };

    let ciphertext = if effective.client_secret.is_empty() {
        String::new()
    } else {
        match crate::crypto::encrypt(&effective.client_secret, jwt_secret) {
            Ok(ciphertext) => ciphertext,
            Err(error) => {
                tracing::warn!(error = %error, "oidc import: secret encryption failed");
                return;
            }
        }
    };
    let result = crate::store::oidc_settings::upsert(
        pool,
        effective.enabled,
        &effective.issuer,
        &effective.client_id,
        &ciphertext,
        (!config.redirect_uri.trim().is_empty()).then_some(config.redirect_uri.trim()),
        &serde_json::to_string(&effective.scopes).unwrap_or_else(|_| "[]".into()),
        effective.auto_provision,
        "file-import",
    )
    .await;
    if let Err(error) = result {
        tracing::warn!(error = %error, "oidc import: saving the row failed");
        return;
    }
    tracing::info!(
        issuer = %effective.issuer,
        enabled = effective.enabled,
        "[auth.oidc] imported into the database — the section is now ignored; \
         remove it from manager.toml"
    );
}

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
    // replacement. The database is the only source — a first save requires
    // the secret.
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
            _ => {
                return Err(AppError::ValidationFailed(
                    "client secret is required".into(),
                ))
            }
        },
    };

    let scopes = req
        .scopes
        .unwrap_or_else(|| vec!["openid".into(), "profile".into(), "email".into()]);
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

/// `DELETE /api/v1/auth/oidc/settings` — clear the stored configuration:
/// SSO turns off until an administrator saves settings again. Local login
/// keeps working throughout (break-glass).
pub async fn reset_settings(
    auth: PlatformAdmin,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let existed = crate::store::oidc_settings::delete(&state.pool).await?;

    let effective = oidc::resolve_effective(None, &state.config.server.public_base_url, "")
        .expect("the empty configuration always resolves");
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
            "end_session_available": probe.end_session_available,
            "client_auth": probe.client_auth,
        })))),
        Err(error) => Ok(Json(ApiResponse::success(serde_json::json!({
            "ok": false,
            "error": error,
        })))),
    }
}

/// Resolve for the current state: the database row only. Resolution errors
/// (e.g. undecryptable secret) surface as `validation_error` in the payload
/// instead of failing the GET — the UI must be able to show them.
fn resolve_for_state(
    state: &AppState,
    row: Option<&crate::store::oidc_settings::OidcSettingsRow>,
) -> Result<EffectiveOidc, AppError> {
    oidc::resolve_effective(
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
        None => false,
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
        "post_logout_redirect_uri": EffectiveOidc::derived_post_logout_redirect_uri(&state.config.server.public_base_url).ok(),
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
