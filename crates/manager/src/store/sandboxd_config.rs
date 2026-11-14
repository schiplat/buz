//! sandboxd-facing configuration reads and writes: the desired-document
//! fetch, the database-restore reconciliation writes, and the applied-status
//! bookkeeping. The long-poll orchestration stays with the handler.
//!
//! On the dual-backend dispatcher (`store::exec`): statements travel as plain
//! SQLite-dialect strings with backend-neutral binds; tuple projections decode
//! positionally off the generic row.
//!
//! `reconcile_db_restore` composes four of these statements read-then-write
//! without a transaction today (single-writer SQLite makes that safe); the
//! PG port must decide the serialisation point — see
//! `transaction-boundaries.md`.

use crate::store::exec::{self, DbPool};

use shared::error::AppError;

/// `(desired_version, stored document?)` for one host — the fetch path's
/// facts. The document is the exact stored bytes for `desired_version`
/// (`None` = nothing published yet). `None` = host not enrolled.
pub async fn desired_row(
    pool: &DbPool,
    host_id: &str,
    instance_id: &str,
) -> Result<Option<(i64, Option<String>)>, AppError> {
    let row = exec::fetch_optional(
        pool,
        "SELECT h.desired_version, c.document
         FROM sandbox_hosts h
         LEFT JOIN sandbox_host_configs c
           ON c.host_id = h.host_id AND c.instance_id = h.instance_id
              AND c.version = h.desired_version
         WHERE h.host_id = ? AND h.instance_id = ?",
        &[host_id.into(), instance_id.into()],
    )
    .await?;
    Ok(row.map(|r| (r.i64(0), r.opt_text(1))))
}

/// Current `desired_version` of one host (`None` = host not enrolled).
pub async fn desired_version(
    pool: &DbPool,
    host_id: &str,
    instance_id: &str,
) -> Result<Option<i64>, AppError> {
    Ok(exec::fetch_optional(
        pool,
        "SELECT desired_version FROM sandbox_hosts WHERE host_id = ? AND instance_id = ?",
        &[host_id.into(), instance_id.into()],
    )
    .await?
    .map(|r| r.i64(0)))
}

/// Highest-version stored document for one host, any version — the
/// db-restore reconciliation body. `None` = nothing stored at all.
pub async fn latest_config_document(
    pool: &DbPool,
    host_id: &str,
    instance_id: &str,
) -> Result<Option<String>, AppError> {
    Ok(exec::fetch_optional(
        pool,
        "SELECT document FROM sandbox_host_configs
         WHERE host_id = ? AND instance_id = ?
         ORDER BY version DESC LIMIT 1",
        &[host_id.into(), instance_id.into()],
    )
    .await?
    .and_then(|r| r.opt_text(0)))
}

/// Store the restoration document at `version` (no-op when the version row
/// already exists — `ON CONFLICT DO NOTHING` keyed by the full version key).
pub async fn insert_restore_config(
    pool: &DbPool,
    host_id: &str,
    instance_id: &str,
    version: i64,
    document_json: &str,
    document_sha256: &str,
) -> Result<(), AppError> {
    exec::execute(
        pool,
        "INSERT INTO sandbox_host_configs
         (host_id, instance_id, version, document, document_sha256, created_by)
         VALUES (?, ?, ?, ?, ?, 'db-restore-reconcile')
         ON CONFLICT DO NOTHING",
        &[
            host_id.into(),
            instance_id.into(),
            version.into(),
            document_json.into(),
            document_sha256.into(),
        ],
    )
    .await?;
    Ok(())
}

/// Point the host at the restored `version` and stamp the reported
/// `applied_version` in the same update — the host must never be told to
/// apply an older version than it runs.
pub async fn bump_versions_for_restore(
    pool: &DbPool,
    host_id: &str,
    instance_id: &str,
    version: i64,
    applied_version: i64,
) -> Result<(), AppError> {
    exec::execute(
        pool,
        "UPDATE sandbox_hosts SET desired_version = ?, applied_version = ?, updated_at = datetime('now')
         WHERE host_id = ? AND instance_id = ?",
        &[
            version.into(),
            applied_version.into(),
            host_id.into(),
            instance_id.into(),
        ],
    )
    .await?;
    Ok(())
}

/// Stamp a successful config fetch (best-effort; caller ignores the result).
pub async fn stamp_fetch_ok(
    pool: &DbPool,
    host_id: &str,
    instance_id: &str,
) -> Result<(), AppError> {
    exec::execute(
        pool,
        "UPDATE sandbox_hosts SET last_fetch_at = datetime('now'),
         last_fetch_error = NULL, consecutive_fetch_failures = 0,
         status = CASE WHEN status = 'enrolled' THEN 'syncing' ELSE status END,
         updated_at = datetime('now')
         WHERE host_id = ? AND instance_id = ?",
        &[host_id.into(), instance_id.into()],
    )
    .await?;
    Ok(())
}

/// `(applied_version, desired_version)` before a status report is applied —
/// the snapshot that lets the log tell a fresh apply from a heartbeat.
/// `None` = host not enrolled.
pub async fn status_snapshot(
    pool: &DbPool,
    host_id: &str,
    instance_id: &str,
) -> Result<Option<(i64, i64)>, AppError> {
    let row = exec::fetch_optional(
        pool,
        "SELECT applied_version, desired_version FROM sandbox_hosts
         WHERE host_id = ? AND instance_id = ?",
        &[host_id.into(), instance_id.into()],
    )
    .await?;
    Ok(row.map(|r| (r.i64(0), r.i64(1))))
}

/// Apply one status report: applied version/sha, heartbeat stamps, fetch
/// error bookkeeping and the insync/pending status CASE. `applied_at` only
/// advances when the reported version differs from the stored one, so
/// heartbeat reports never overwrite when that version landed.
#[allow(clippy::too_many_arguments)]
pub async fn apply_status_report(
    pool: &DbPool,
    host_id: &str,
    instance_id: &str,
    applied_version: i64,
    applied_sha256: &str,
    last_fetch_error: Option<&str>,
    consecutive_failures: i64,
) -> Result<(), AppError> {
    exec::execute(
        pool,
        "UPDATE sandbox_hosts SET
           applied_version = ?,
           applied_sha256 = ?,
           applied_at = CASE
             WHEN ? > 0 AND ? != applied_version THEN datetime('now')
             ELSE applied_at END,
           last_fetch_at = datetime('now'),
           last_fetch_error = ?,
           consecutive_fetch_failures = ?,
           status = CASE
             WHEN ? > 0 AND desired_version = ? THEN 'insync'
             WHEN ? > 0 THEN 'pending'
             ELSE status END,
           updated_at = datetime('now')
         WHERE host_id = ? AND instance_id = ?",
        &[
            applied_version.into(),
            applied_sha256.into(),
            applied_version.into(),
            applied_version.into(),
            last_fetch_error.into(),
            consecutive_failures.into(),
            applied_version.into(),
            applied_version.into(),
            applied_version.into(),
            host_id.into(),
            instance_id.into(),
        ],
    )
    .await?;
    Ok(())
}
