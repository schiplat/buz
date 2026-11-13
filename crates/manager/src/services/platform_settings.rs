//! Platform-level runtime settings (admin-editable, DB-backed).
//!
//! Precedence: **DB override > manager.toml default** — a missing row means
//! "use the default". Values are plain strings; interpretation and validation
//! live in [`setting_meta`]. Editing happens through the admin settings API;
//! resolver-affecting keys additionally push a runtime override into
//! `plugin_resolver` so changes take effect without a restart.

use std::collections::HashMap;

use crate::store::exec::DbPool;

pub use crate::store::platform_settings::{
    KEY_GRANTS_REQUIRE_APPROVAL, KEY_JOB_LOG_RETENTION_DAYS, KEY_MARKETPLACE_URL,
    KEY_PLUGIN_INDEX_LOCAL_DIR, KEY_PLUGIN_INDEX_PUBLIC_KEY, KEY_PLUGIN_REQUIRE_SIGNED_INDEX,
    KEY_PLUGIN_REVIEWED_MANIFEST, KEY_PLUGIN_REVIEW_REQUIRED, KEY_PLUGIN_SYNC_TIMEOUT,
    KEY_SANDBOX_CONFIG_POLL_SECONDS, KEY_SANDBOX_CONFIG_REQUIRE_APPROVAL,
    KEY_SANDBOX_CONFIG_RETENTION_DAYS, KEY_SANDBOX_CONFIG_STALE_SECONDS,
    KEY_SCHEDULER_STARVATION_SECONDS,
};

pub const DEFAULT_SANDBOX_CONFIG_RETENTION_DAYS: u64 = 30;
pub const DEFAULT_SANDBOX_CONFIG_POLL_SECONDS: u64 = 30;
pub const DEFAULT_SANDBOX_CONFIG_STALE_SECONDS: u64 = 300;
/// 10 minutes: long enough that bursty small-job streams still backfill,
/// short enough that a genuinely starved large job gets the next capacity.
pub const DEFAULT_SCHEDULER_STARVATION_SECONDS: u64 = 600;
pub const DEFAULT_JOB_LOG_RETENTION_DAYS: u64 = 30;

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
        KEY_SANDBOX_CONFIG_REQUIRE_APPROVAL => Some(SettingKind::Bool),
        KEY_SANDBOX_CONFIG_RETENTION_DAYS => Some(SettingKind::Number),
        KEY_SANDBOX_CONFIG_POLL_SECONDS => Some(SettingKind::Number),
        KEY_SANDBOX_CONFIG_STALE_SECONDS => Some(SettingKind::Number),
        KEY_SCHEDULER_STARVATION_SECONDS => Some(SettingKind::Number),
        KEY_JOB_LOG_RETENTION_DAYS => Some(SettingKind::Number),
        KEY_GRANTS_REQUIRE_APPROVAL => Some(SettingKind::Bool),
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
                .map_err(|_| "marketplace_url must be an absolute http(s) URL".to_string())?;
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
            // Per-key ranges: each Number setting carries its own domain.
            let parsed: u64 = value
                .trim()
                .parse()
                .map_err(|_| format!("{key} must be a whole number"))?;
            match key {
                KEY_PLUGIN_SYNC_TIMEOUT => {
                    // 0 disables the timeout entirely (reqwest semantics) — legal in
                    // a config file for special cases, but flagged loudly in the UI
                    // because a hung source then blocks pipeline saves indefinitely.
                    if parsed == 0 || (5..=300).contains(&parsed) {
                        Ok(())
                    } else {
                        Err(
                            "plugin_sync_timeout must be 0 (disable) or between 5 and 300 seconds"
                                .into(),
                        )
                    }
                }
                KEY_SANDBOX_CONFIG_RETENTION_DAYS => {
                    if (1..=3650).contains(&parsed) {
                        Ok(())
                    } else {
                        Err(
                            "sandbox_config_history_retention_days must be between 1 and 3650"
                                .into(),
                        )
                    }
                }
                KEY_SANDBOX_CONFIG_POLL_SECONDS => {
                    if (5..=600).contains(&parsed) {
                        Ok(())
                    } else {
                        Err("sandbox_config_poll_seconds must be between 5 and 600".into())
                    }
                }
                KEY_SANDBOX_CONFIG_STALE_SECONDS => {
                    if (60..=86400).contains(&parsed) {
                        Ok(())
                    } else {
                        Err(
                            "sandbox_config_dispatch_stale_after_seconds must be between 60 and 86400"
                                .into(),
                        )
                    }
                }
                KEY_JOB_LOG_RETENTION_DAYS => {
                    // 0 = keep forever (retention disabled); otherwise 1..=10 years.
                    if parsed == 0 || (1..=3650).contains(&parsed) {
                        Ok(())
                    } else {
                        Err(
                            "job_log_retention_days must be 0 (keep forever) or between 1 and 3650"
                                .into(),
                        )
                    }
                }
                KEY_SCHEDULER_STARVATION_SECONDS => {
                    if parsed == 0 || (60..=86400).contains(&parsed) {
                        Ok(())
                    } else {
                        Err(
                            "scheduler_starvation_seconds must be 0 (disabled) or between 60 and 86400"
                                .into(),
                        )
                    }
                }
                _ => Err(format!("unknown numeric setting '{key}'")),
            }
        }
    }
}

/// All DB overrides for the known keys. Unknown rows are ignored (forward
/// compatibility with older Managers).
pub async fn fetch_overrides(pool: &DbPool) -> HashMap<String, String> {
    crate::store::platform_settings::overrides(pool)
        .await
        .unwrap_or_default()
        .into_iter()
        .collect()
}

pub async fn upsert(pool: &DbPool, key: &str, value: &str, updated_by: &str) -> anyhow::Result<()> {
    validate_value(key, value).map_err(|e| anyhow::anyhow!("{e}"))?;
    crate::store::platform_settings::upsert(pool, key, value, updated_by)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))
}

pub async fn delete(pool: &DbPool, key: &str) -> anyhow::Result<()> {
    crate::store::platform_settings::delete(pool, key)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))
}

/// Effective value for one key: DB override, else `default`.
pub async fn effective(pool: &DbPool, key: &str, default: &str) -> String {
    let overrides = fetch_overrides(pool).await;
    overrides
        .get(key)
        .cloned()
        .unwrap_or_else(|| default.to_string())
}
