//! Platform-level runtime settings (admin-editable, DB-backed).
//!
//! Precedence: **DB override > manager.toml default** — a missing row means
//! "use the default". Values are plain strings; interpretation and validation
//! live in [`setting_meta`]. Editing happens through the admin settings API;
//! resolver-affecting keys additionally push a runtime override into
//! `plugin_resolver` so changes take effect without a restart.

use sqlx::SqlitePool;
use std::collections::HashMap;

pub const KEY_MARKETPLACE_URL: &str = "marketplace_url";
pub const KEY_PLUGIN_REVIEW_REQUIRED: &str = "plugin_review_required";
pub const KEY_PLUGIN_INDEX_LOCAL_DIR: &str = "plugin_index_local_dir";
pub const KEY_PLUGIN_REVIEWED_MANIFEST: &str = "plugin_reviewed_manifest";
pub const KEY_PLUGIN_SYNC_TIMEOUT: &str = "plugin_sync_timeout";
/// Trust anchor for source indexes: when on, every sync verifies
/// `index.json.sig` against `plugin_index_public_key` and refuses sources
/// that are unsigned or tampered with.
pub const KEY_PLUGIN_REQUIRE_SIGNED_INDEX: &str = "plugin_require_signed_index";
pub const KEY_PLUGIN_INDEX_PUBLIC_KEY: &str = "plugin_index_public_key";

/// Default source-sync timeout (seconds) — the single source of truth: the
/// manager.toml serde default, the settings API default, and the HTTP client
/// construction all read this constant (DB override via the settings UI).
pub const DEFAULT_STEP_SYNC_TIMEOUT_SECS: u64 = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingKind {
    Url,
    Bool,
    Path,
    Number,
    /// Base64 Ed25519 public key (empty = no trust anchor configured).
    PublicKey,
}

pub fn setting_kind(key: &str) -> Option<SettingKind> {
    match key {
        KEY_MARKETPLACE_URL => Some(SettingKind::Url),
        KEY_PLUGIN_REVIEW_REQUIRED => Some(SettingKind::Bool),
        KEY_PLUGIN_INDEX_LOCAL_DIR => Some(SettingKind::Path),
        KEY_PLUGIN_REVIEWED_MANIFEST => Some(SettingKind::Path),
        KEY_PLUGIN_SYNC_TIMEOUT => Some(SettingKind::Number),
        KEY_PLUGIN_REQUIRE_SIGNED_INDEX => Some(SettingKind::Bool),
        KEY_PLUGIN_INDEX_PUBLIC_KEY => Some(SettingKind::PublicKey),
        _ => None,
    }
}

/// Validate a candidate value. Empty string = "reset to default" for
/// [`SettingKind::Url`] (the built-in marketplace applies); paths and booleans
/// must be non-empty/valid.
pub fn validate_value(key: &str, value: &str) -> Result<(), String> {
    match setting_kind(key).ok_or_else(|| format!("unknown setting '{key}'"))? {
        SettingKind::Url => {
            if value.is_empty() {
                return Ok(());
            }
            let parsed = url::Url::parse(value)
                .map_err(|_| format!("marketplace_url must be an absolute http(s) URL"))?;
            if parsed.scheme() != "http" && parsed.scheme() != "https" {
                return Err("marketplace_url must be an http(s) URL".into());
            }
            Ok(())
        }
        SettingKind::Bool => value
            .parse::<bool>()
            .map(|_| ())
            .map_err(|_| "plugin_review_required must be 'true' or 'false'".into()),
        SettingKind::Path => {
            // plugin_reviewed_manifest may be empty (= review gate disabled);
            // other paths must exist as a meaningful setting.
            if key == KEY_PLUGIN_REVIEWED_MANIFEST || !value.trim().is_empty() {
                Ok(())
            } else {
                Err("path must not be empty".into())
            }
        }
        SettingKind::PublicKey => {
            if value.trim().is_empty() {
                return Ok(()); // no trust anchor configured
            }
            use base64::Engine as _;
            let decoded = base64::engine::general_purpose::STANDARD
                .decode(value.trim())
                .map_err(|_| {
                    "plugin_index_public_key must be base64 (the `public_key` field from                      `buz-plugin keygen`)"
                        .to_string()
                })?;
            if decoded.len() != 32 {
                return Err(format!(
                    "plugin_index_public_key must decode to 32 bytes (got {})",
                    decoded.len()
                ));
            }
            Ok(())
        }
        SettingKind::Number => {
            let secs: u64 = value.trim().parse().map_err(|_| {
                String::from("plugin_sync_timeout must be a whole number of seconds")
            })?;
            // 0 disables the timeout entirely (reqwest semantics) — legal in
            // a config file for special cases, but flagged loudly in the UI
            // because a hung source then blocks pipeline saves indefinitely.
            if secs == 0 || (5..=300).contains(&secs) {
                Ok(())
            } else {
                Err("plugin_sync_timeout must be 0 (disable) or between 5 and 300 seconds".into())
            }
        }
    }
}

/// All DB overrides for the known keys. Unknown rows are ignored (forward
/// compatibility with older Managers).
pub async fn fetch_overrides(pool: &SqlitePool) -> HashMap<String, String> {
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT key, value FROM platform_settings WHERE key IN (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(KEY_MARKETPLACE_URL)
    .bind(KEY_PLUGIN_REVIEW_REQUIRED)
    .bind(KEY_PLUGIN_INDEX_LOCAL_DIR)
    .bind(KEY_PLUGIN_REVIEWED_MANIFEST)
    .bind(KEY_PLUGIN_SYNC_TIMEOUT)
    .bind(KEY_PLUGIN_REQUIRE_SIGNED_INDEX)
    .bind(KEY_PLUGIN_INDEX_PUBLIC_KEY)
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    rows.into_iter().collect()
}

pub async fn upsert(
    pool: &SqlitePool,
    key: &str,
    value: &str,
    updated_by: &str,
) -> anyhow::Result<()> {
    validate_value(key, value).map_err(|e| anyhow::anyhow!("{e}"))?;
    sqlx::query(
        "INSERT INTO platform_settings (key, value, updated_by, updated_at)
         VALUES (?, ?, ?, datetime('now'))
         ON CONFLICT(key) DO UPDATE SET value = excluded.value,
           updated_by = excluded.updated_by, updated_at = excluded.updated_at",
    )
    .bind(key)
    .bind(value)
    .bind(updated_by)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn delete(pool: &SqlitePool, key: &str) -> anyhow::Result<()> {
    sqlx::query("DELETE FROM platform_settings WHERE key = ?")
        .bind(key)
        .execute(pool)
        .await?;
    Ok(())
}

/// Effective value for one key: DB override, else `default`.
pub async fn effective(pool: &SqlitePool, key: &str, default: &str) -> String {
    let overrides = fetch_overrides(pool).await;
    overrides
        .get(key)
        .cloned()
        .unwrap_or_else(|| default.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_bool_rejects_junk() {
        assert!(validate_value(KEY_PLUGIN_REVIEW_REQUIRED, "true").is_ok());
        assert!(validate_value(KEY_PLUGIN_REVIEW_REQUIRED, "yes").is_err());
        assert!(validate_value(KEY_PLUGIN_REVIEW_REQUIRED, "").is_err());
    }

    #[test]
    fn validate_sync_timeout_zero_is_explicit_disable() {
        // 0 = timeout disabled (reqwest semantics) — legal but dangerous,
        // the UI flags it loudly.
        assert!(validate_value(KEY_PLUGIN_SYNC_TIMEOUT, "0").is_ok());
        assert!(validate_value(KEY_PLUGIN_SYNC_TIMEOUT, "60").is_ok());
        assert!(validate_value(KEY_PLUGIN_SYNC_TIMEOUT, "300").is_ok());
        assert!(validate_value(KEY_PLUGIN_SYNC_TIMEOUT, "301").is_err());
        assert!(validate_value(KEY_PLUGIN_SYNC_TIMEOUT, "4").is_err());
    }

    #[test]
    fn validate_url_allows_empty_reset_and_rejects_non_http() {
        assert!(validate_value(KEY_MARKETPLACE_URL, "").is_ok());
        assert!(validate_value(KEY_MARKETPLACE_URL, "https://plugins.example.com").is_ok());
        assert!(validate_value(KEY_MARKETPLACE_URL, "ftp://x").is_err());
        assert!(validate_value(KEY_MARKETPLACE_URL, "not a url").is_err());
    }

    #[test]
    fn validate_path_rejects_empty() {
        assert!(validate_value(KEY_PLUGIN_INDEX_LOCAL_DIR, "/srv/steps").is_ok());
        assert!(validate_value(KEY_PLUGIN_INDEX_LOCAL_DIR, "  ").is_err());
    }

    #[test]
    fn unknown_key_is_rejected() {
        assert!(validate_value("nope", "x").is_err());
        assert!(setting_kind("nope").is_none());
    }
}
