//! Plugin-pool index rows.
//!
//! The sync/refresh writers live in `services::plugin_pool`; this module owns
//! the table access they and YAML validation share.

use crate::store::exec::{self, DbPool};

use shared::error::AppError;

/// One enabled pool entry (the curator's allowlist for remote sources).
pub struct EnabledEntry {
    pub namespace: String,
    pub plugin_id: String,
    pub version: String,
    pub sha256: String,
}

/// Enabled pool rows — the `uses:` availability check.
pub async fn enabled_entries(pool: &DbPool) -> Result<Vec<EnabledEntry>, AppError> {
    Ok(exec::fetch_all(
        pool,
        "SELECT namespace, plugin_id, version, sha256 FROM plugin_pool WHERE enabled = 1",
        &[],
    )
    .await?
    .into_iter()
    .map(|r| EnabledEntry {
        namespace: r.text(0),
        plugin_id: r.text(1),
        version: r.text(2),
        sha256: r.text(3),
    })
    .collect())
}
