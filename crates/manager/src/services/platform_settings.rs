//! Platform-level runtime settings (admin-editable, DB-backed).
//!
//! Precedence: **DB override > manager.toml default** — a missing row means
//! "use the default". Values are plain strings; interpretation and validation
//! live in [`setting_meta`]. Editing happens through the admin settings API;
//! resolver-affecting keys additionally push a runtime override into
//! `step_resolver` so changes take effect without a restart.

use sqlx::SqlitePool;
use std::collections::HashMap;

pub const KEY_MARKETPLACE_URL: &str = "marketplace_url";
pub const KEY_STEP_REVIEW_REQUIRED: &str = "step_review_required";
pub const KEY_STEP_INDEX_LOCAL_DIR: &str = "step_index_local_dir";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingKind {
    Url,
    Bool,
    Path,
}

pub fn setting_kind(key: &str) -> Option<SettingKind> {
    match key {
        KEY_MARKETPLACE_URL => Some(SettingKind::Url),
        KEY_STEP_REVIEW_REQUIRED => Some(SettingKind::Bool),
        KEY_STEP_INDEX_LOCAL_DIR => Some(SettingKind::Path),
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
            .map_err(|_| "step_review_required must be 'true' or 'false'".into()),
        SettingKind::Path => {
            if value.trim().is_empty() {
                Err("path must not be empty".into())
            } else {
                Ok(())
            }
        }
    }
}

/// All DB overrides for the known keys. Unknown rows are ignored (forward
/// compatibility with older Managers).
pub async fn fetch_overrides(pool: &SqlitePool) -> HashMap<String, String> {
    let rows: Vec<(String, String)> =
        sqlx::query_as("SELECT key, value FROM platform_settings WHERE key IN (?, ?, ?)")
            .bind(KEY_MARKETPLACE_URL)
            .bind(KEY_STEP_REVIEW_REQUIRED)
            .bind(KEY_STEP_INDEX_LOCAL_DIR)
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
        assert!(validate_value(KEY_STEP_REVIEW_REQUIRED, "true").is_ok());
        assert!(validate_value(KEY_STEP_REVIEW_REQUIRED, "yes").is_err());
        assert!(validate_value(KEY_STEP_REVIEW_REQUIRED, "").is_err());
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
        assert!(validate_value(KEY_STEP_INDEX_LOCAL_DIR, "/srv/steps").is_ok());
        assert!(validate_value(KEY_STEP_INDEX_LOCAL_DIR, "  ").is_err());
    }

    #[test]
    fn unknown_key_is_rejected() {
        assert!(validate_value("nope", "x").is_err());
        assert!(setting_kind("nope").is_none());
    }
}
