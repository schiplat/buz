//! Dashboard-managed OIDC settings (single row; Phase 1.5 web configuration).
//!
//! A saved row makes the **database authoritative** (DB > `[auth.oidc]`
//! file > defaults); an absent row means "the file seed applies". The client
//! secret lives encrypted at rest (`buzenc:v1:` via `crypto::encrypt`, key
//! derived from `jwt_secret`) and is re-encrypted by `rotate-jwt-key` —
//! the row's plaintext never exists outside the request that set it.

use crate::store::exec::{self, DbPool, SqlValue};

use shared::error::AppError;

/// The dashboard-saved SSO configuration (`id = 1` singleton).
#[derive(Debug, Clone)]
pub struct OidcSettingsRow {
    pub enabled: bool,
    pub issuer: String,
    pub client_id: String,
    pub client_secret_ciphertext: String,
    pub redirect_uri_override: Option<String>,
    pub scopes_json: String,
    pub auto_provision: bool,
    /// Token-endpoint client authentication override: `auto` (discovery
    /// negotiation), `client_secret_basic`, or `client_secret_post`.
    pub token_auth: String,
    /// id_token claim mirrored into `external_identities.groups_json`.
    /// Display/audit only — it never grants permissions.
    pub groups_claim: String,
    pub updated_by: String,
}

/// Everything an upsert writes. A struct keeps the call site readable as the
/// configuration grows (the positional form used to be nine arguments).
pub struct OidcSettingsUpsert<'a> {
    pub enabled: bool,
    pub issuer: &'a str,
    pub client_id: &'a str,
    pub client_secret_ciphertext: &'a str,
    pub redirect_uri_override: Option<&'a str>,
    pub scopes_json: &'a str,
    pub auto_provision: bool,
    pub token_auth: &'a str,
    pub groups_claim: &'a str,
    pub updated_by: &'a str,
}

/// The singleton row, when an administrator has saved SSO settings.
pub async fn get(pool: &DbPool) -> Result<Option<OidcSettingsRow>, AppError> {
    Ok(exec::fetch_optional(
        pool,
        "SELECT enabled, issuer, client_id, client_secret_ciphertext, redirect_uri_override,
                scopes_json, auto_provision, token_auth, groups_claim, updated_by
         FROM oidc_settings WHERE id = 1",
        &[],
    )
    .await?
    .map(|r| OidcSettingsRow {
        enabled: r.i64_of("enabled") != 0,
        issuer: r.text_of("issuer"),
        client_id: r.text_of("client_id"),
        client_secret_ciphertext: r.text_of("client_secret_ciphertext"),
        redirect_uri_override: r.opt_text_of("redirect_uri_override"),
        scopes_json: r.text_of("scopes_json"),
        auto_provision: r.i64_of("auto_provision") != 0,
        token_auth: r.text_of("token_auth"),
        groups_claim: r.text_of("groups_claim"),
        updated_by: r.text_of("updated_by"),
    }))
}

/// Save (or overwrite) the singleton row. `client_secret_ciphertext` carries
/// the already-encrypted secret (`crypto::encrypt` at the API layer); callers
/// resolve "keep the stored secret" before calling this (the SQL overwrites
/// every column).
pub async fn upsert(pool: &DbPool, settings: &OidcSettingsUpsert<'_>) -> Result<(), AppError> {
    exec::execute(
        pool,
        "INSERT INTO oidc_settings
            (id, enabled, issuer, client_id, client_secret_ciphertext,
             redirect_uri_override, scopes_json, auto_provision, token_auth,
             groups_claim, updated_by)
         VALUES (1, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT (id) DO UPDATE SET
            enabled = excluded.enabled,
            issuer = excluded.issuer,
            client_id = excluded.client_id,
            client_secret_ciphertext = excluded.client_secret_ciphertext,
            redirect_uri_override = excluded.redirect_uri_override,
            scopes_json = excluded.scopes_json,
            auto_provision = excluded.auto_provision,
            token_auth = excluded.token_auth,
            groups_claim = excluded.groups_claim,
            updated_by = excluded.updated_by,
            updated_at = datetime('now')",
        &[
            SqlValue::Int(i64::from(settings.enabled)),
            settings.issuer.into(),
            settings.client_id.into(),
            settings.client_secret_ciphertext.into(),
            settings.redirect_uri_override.map(|s| s.to_string()).into(),
            settings.scopes_json.into(),
            SqlValue::Int(i64::from(settings.auto_provision)),
            settings.token_auth.into(),
            settings.groups_claim.into(),
            settings.updated_by.into(),
        ],
    )
    .await?;
    Ok(())
}

/// Drop the override row: the file `[auth.oidc]` seed applies again.
pub async fn delete(pool: &DbPool) -> Result<bool, AppError> {
    let affected = exec::execute(pool, "DELETE FROM oidc_settings WHERE id = 1", &[]).await?;
    Ok(affected != 0)
}
