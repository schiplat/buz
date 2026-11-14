//! `platform_settings` rows: the admin-editable runtime overrides.
//!
//! The key vocabulary and value validation live in
//! `services::platform_settings`; this module owns the table access only.

use crate::store::exec::{self, DbPool, SqlValue};

use shared::error::AppError;

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

/// Sandbox host configuration (design record: sandbox-host-configuration).
/// Two-person approval for config changes; default off.
pub const KEY_SANDBOX_CONFIG_REQUIRE_APPROVAL: &str = "sandbox_config_require_approval";
/// Days to keep sandbox config version history and audit diffs.
pub const KEY_SANDBOX_CONFIG_RETENTION_DAYS: &str = "sandbox_config_history_retention_days";
/// sandboxd long-poll period in seconds.
pub const KEY_SANDBOX_CONFIG_POLL_SECONDS: &str = "sandbox_config_poll_seconds";
/// How long a host may lag behind the desired version before non-default
/// profile dispatch is paused.
pub const KEY_SANDBOX_CONFIG_STALE_SECONDS: &str = "sandbox_config_dispatch_stale_after_seconds";
/// How long a pending job may be overtaken by smaller ones (backfill) before
/// the runner stops filling gaps and waits for it. 0 disables aging.
pub const KEY_SCHEDULER_STARVATION_SECONDS: &str = "scheduler_starvation_seconds";
/// Terminal-job log retention in days (0 = keep forever).
pub const KEY_JOB_LOG_RETENTION_DAYS: &str = "job_log_retention_days";
/// When on, resource-profile grant/revoke requests create a pending change
/// (two-person flow) instead of applying directly.
pub const KEY_GRANTS_REQUIRE_APPROVAL: &str = "grants_require_approval";

/// Every key the Manager interprets, in the order the override query binds
/// them. Rows for retired keys are ignored (forward compatibility).
pub const OVERRIDE_KEYS: &[&str] = &[
    KEY_MARKETPLACE_URL,
    KEY_PLUGIN_REVIEW_REQUIRED,
    KEY_PLUGIN_INDEX_LOCAL_DIR,
    KEY_PLUGIN_REVIEWED_MANIFEST,
    KEY_PLUGIN_SYNC_TIMEOUT,
    KEY_PLUGIN_REQUIRE_SIGNED_INDEX,
    KEY_PLUGIN_INDEX_PUBLIC_KEY,
    KEY_SANDBOX_CONFIG_REQUIRE_APPROVAL,
    KEY_SANDBOX_CONFIG_RETENTION_DAYS,
    KEY_SANDBOX_CONFIG_POLL_SECONDS,
    KEY_SANDBOX_CONFIG_STALE_SECONDS,
    KEY_SCHEDULER_STARVATION_SECONDS,
    KEY_GRANTS_REQUIRE_APPROVAL,
    KEY_JOB_LOG_RETENTION_DAYS,
];

/// Placeholder count in the override query must match [`OVERRIDE_KEYS`].
const _: () = assert!(OVERRIDE_KEYS.len() == 14);

/// All known-key overrides that have a row.
pub async fn overrides(pool: &DbPool) -> Result<Vec<(String, String)>, AppError> {
    let binds: Vec<SqlValue> = OVERRIDE_KEYS.iter().map(|k| SqlValue::from(*k)).collect();
    Ok(exec::fetch_all(
        pool,
        "SELECT key, value FROM platform_settings WHERE key IN (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        &binds,
    )
    .await?
    .into_iter()
    .map(|r| (r.text(0), r.text(1)))
    .collect())
}

/// One key's override (`None` = no row).
pub async fn get(pool: &DbPool, key: &str) -> Result<Option<String>, AppError> {
    Ok(exec::fetch_optional(
        pool,
        "SELECT value FROM platform_settings WHERE key = ?",
        &[key.into()],
    )
    .await?
    .map(|r| r.text(0)))
}

pub async fn upsert(
    pool: &DbPool,
    key: &str,
    value: &str,
    updated_by: &str,
) -> Result<(), AppError> {
    exec::execute(
        pool,
        "INSERT INTO platform_settings (key, value, updated_by, updated_at)
         VALUES (?, ?, ?, datetime('now'))
         ON CONFLICT(key) DO UPDATE SET value = excluded.value,
           updated_by = excluded.updated_by, updated_at = excluded.updated_at",
        &[key.into(), value.into(), updated_by.into()],
    )
    .await?;
    Ok(())
}

pub async fn delete(pool: &DbPool, key: &str) -> Result<(), AppError> {
    exec::execute(
        pool,
        "DELETE FROM platform_settings WHERE key = ?",
        &[key.into()],
    )
    .await?;
    Ok(())
}
