//! Plugin source writes (admin surface). Reads live in `plugin_pool`.
//!
//! Every write runs on the dual-backend dispatcher (`store::exec`);
//! `toggle_pool_entry` opens its own transaction around
//! `crate::audit::record_in_tx`.

use crate::store::exec::{self, DbPool, SqlValue};

use shared::error::AppError;

pub async fn insert(
    pool: &DbPool,
    id: &str,
    name: &str,
    url: &str,
    mirror_url: Option<&str>,
    auto_enable: bool,
    created_by: &str,
) -> Result<(), AppError> {
    exec::execute(
        pool,
        "INSERT INTO plugin_sources (id, name, url, mirror_url, auto_enable, enabled, created_by)
         VALUES (?, ?, ?, ?, ?, 1, ?)",
        &[
            id.into(),
            name.into(),
            url.into(),
            mirror_url.into(),
            SqlValue::Int(if auto_enable { 1 } else { 0 }),
            created_by.into(),
        ],
    )
    .await
    .map_err(|e| AppError::Internal(e.to_string()))?;
    Ok(())
}

/// Forget the accepted-index watermark so the next sync re-establishes it
/// (anti-rollback window re-opens until that sync verifies).
pub async fn reset_index_watermark(pool: &DbPool, id: &str) -> Result<(), AppError> {
    exec::execute(
        pool,
        "UPDATE plugin_sources SET last_index_sequence = NULL, last_index_sha256 = NULL, \
         last_index_issued_at = NULL WHERE id = ?",
        &[id.into()],
    )
    .await
    .map_err(|e| AppError::Internal(e.to_string()))?;
    Ok(())
}

pub async fn delete(pool: &DbPool, id: &str) -> Result<(), AppError> {
    exec::execute(
        pool,
        "DELETE FROM plugin_sources WHERE id = ?",
        &[id.into()],
    )
    .await
    .map_err(|e| AppError::Internal(e.to_string()))?;
    Ok(())
}

/// Remember that the factory-seeded source was deliberately removed, so the
/// startup migration does not re-seed it on every boot.
pub async fn mark_official_source_dismissed(
    pool: &DbPool,
    updated_by: &str,
) -> Result<(), AppError> {
    exec::execute(
        pool,
        "INSERT INTO platform_settings (key, value, updated_by, updated_at) \
         VALUES ('plugin_official_source_dismissed', 'true', ?, datetime('now')) \
         ON CONFLICT(key) DO UPDATE SET value = 'true', updated_by = excluded.updated_by, \
           updated_at = excluded.updated_at",
        &[updated_by.into()],
    )
    .await
    .map_err(|e| AppError::Internal(e.to_string()))?;
    Ok(())
}

/// Enable/disable one plugin-pool entry.
///
/// One transaction: the state change, its provenance (who/when) and the audit
/// record land together or not at all. `Err(NotFound)` when the entry does not
/// exist (transaction rolled back).
pub async fn toggle_pool_entry(
    pool: &DbPool,
    namespace: &str,
    plugin_id: &str,
    version: &str,
    enabled: bool,
    actor: &crate::middleware::AuthUser,
) -> Result<(), AppError> {
    let mut tx = pool
        .begin_immediate()
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;
    let previous_enabled = tx
        .fetch_optional(
            "SELECT enabled FROM plugin_pool \
         WHERE namespace = ? AND plugin_id = ? AND version = ?",
            &[namespace.into(), plugin_id.into(), version.into()],
        )
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?
        .map(|r| r.i64(0));
    let affected = tx
        .execute(
            "UPDATE plugin_pool SET enabled = ?, updated_by = ?, updated_at = datetime('now') \
         WHERE namespace = ? AND plugin_id = ? AND version = ?",
            &[
                enabled.into(),
                actor.username.as_str().into(),
                namespace.into(),
                plugin_id.into(),
                version.into(),
            ],
        )
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;
    if affected == 0 {
        tx.rollback().await.ok();
        return Err(AppError::NotFound("pool entry not found".into()));
    }
    crate::audit::record_in_tx(
        &mut tx,
        crate::audit::AuditEvent::new(crate::audit::Action::PluginPoolToggle, actor)
            .on(crate::audit::Resource::plugin_pool(
                namespace, plugin_id, version,
            ))
            .changes(
                Some(serde_json::json!({ "enabled": previous_enabled.map(|v| v != 0) })),
                Some(serde_json::json!({ "enabled": enabled })),
            ),
    )
    .await
    .map_err(|e| AppError::Internal(e.to_string()))?;
    tx.commit()
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;
    Ok(())
}
