//! sandboxd-facing configuration reads and writes: the desired-document
//! fetch, the database-restore reconciliation writes, and the applied-status
//! bookkeeping. The long-poll orchestration stays with the handler.
//!
//! `reconcile_db_restore` composes four of these statements read-then-write
//! without a transaction today (single-writer SQLite makes that safe); the
//! PG port must decide the serialisation point — see
//! `transaction-boundaries.md`.

use sqlx::SqlitePool;

use shared::error::AppError;

/// `(desired_version, stored document?)` for one host — the fetch path's
/// facts. The document is the exact stored bytes for `desired_version`
/// (`None` = nothing published yet). `None` = host not enrolled.
pub async fn desired_row(
    pool: &SqlitePool,
    host_id: &str,
    instance_id: &str,
) -> Result<Option<(i64, Option<String>)>, AppError> {
    let row: Option<(i64, Option<String>)> = sqlx::query_as(
        "SELECT h.desired_version, c.document
         FROM sandbox_hosts h
         LEFT JOIN sandbox_host_configs c
           ON c.host_id = h.host_id AND c.instance_id = h.instance_id
              AND c.version = h.desired_version
         WHERE h.host_id = ? AND h.instance_id = ?",
    )
    .bind(host_id)
    .bind(instance_id)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// Current `desired_version` of one host (`None` = host not enrolled).
pub async fn desired_version(
    pool: &SqlitePool,
    host_id: &str,
    instance_id: &str,
) -> Result<Option<i64>, AppError> {
    Ok(sqlx::query_scalar(
        "SELECT desired_version FROM sandbox_hosts WHERE host_id = ? AND instance_id = ?",
    )
    .bind(host_id)
    .bind(instance_id)
    .fetch_optional(pool)
    .await?)
}

/// Highest-version stored document for one host, any version — the
/// db-restore reconciliation body. `None` = nothing stored at all.
pub async fn latest_config_document(
    pool: &SqlitePool,
    host_id: &str,
    instance_id: &str,
) -> Result<Option<String>, AppError> {
    let stored: Option<String> = sqlx::query_scalar(
        "SELECT document FROM sandbox_host_configs
         WHERE host_id = ? AND instance_id = ?
         ORDER BY version DESC LIMIT 1",
    )
    .bind(host_id)
    .bind(instance_id)
    .fetch_optional(pool)
    .await?
    .flatten();
    Ok(stored)
}

/// Store the restoration document at `version` (no-op when the version row
/// already exists — `INSERT OR IGNORE` keyed by the full version key).
pub async fn insert_restore_config(
    pool: &SqlitePool,
    host_id: &str,
    instance_id: &str,
    version: i64,
    document_json: &str,
    document_sha256: &str,
) -> Result<(), AppError> {
    sqlx::query(
        "INSERT OR IGNORE INTO sandbox_host_configs
         (host_id, instance_id, version, document, document_sha256, created_by)
         VALUES (?, ?, ?, ?, ?, 'db-restore-reconcile')",
    )
    .bind(host_id)
    .bind(instance_id)
    .bind(version)
    .bind(document_json)
    .bind(document_sha256)
    .execute(pool)
    .await?;
    Ok(())
}

/// Point the host at the restored `version` and stamp the reported
/// `applied_version` in the same update — the host must never be told to
/// apply an older version than it runs.
pub async fn bump_versions_for_restore(
    pool: &SqlitePool,
    host_id: &str,
    instance_id: &str,
    version: i64,
    applied_version: i64,
) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE sandbox_hosts SET desired_version = ?, applied_version = ?, updated_at = datetime('now')
         WHERE host_id = ? AND instance_id = ?",
    )
    .bind(version)
    .bind(applied_version)
    .bind(host_id)
    .bind(instance_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Stamp a successful config fetch (best-effort; caller ignores the result).
pub async fn stamp_fetch_ok(
    pool: &SqlitePool,
    host_id: &str,
    instance_id: &str,
) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE sandbox_hosts SET last_fetch_at = datetime('now'),
         last_fetch_error = NULL, consecutive_fetch_failures = 0,
         status = CASE WHEN status = 'enrolled' THEN 'syncing' ELSE status END,
         updated_at = datetime('now')
         WHERE host_id = ? AND instance_id = ?",
    )
    .bind(host_id)
    .bind(instance_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// `(applied_version, desired_version)` before a status report is applied —
/// the snapshot that lets the log tell a fresh apply from a heartbeat.
/// `None` = host not enrolled.
pub async fn status_snapshot(
    pool: &SqlitePool,
    host_id: &str,
    instance_id: &str,
) -> Result<Option<(i64, i64)>, AppError> {
    let before: Option<(i64, i64)> = sqlx::query_as(
        "SELECT applied_version, desired_version FROM sandbox_hosts
         WHERE host_id = ? AND instance_id = ?",
    )
    .bind(host_id)
    .bind(instance_id)
    .fetch_optional(pool)
    .await?;
    Ok(before)
}

/// Apply one status report: applied version/sha, heartbeat stamps, fetch
/// error bookkeeping and the insync/pending status CASE. `applied_at` only
/// advances when the reported version differs from the stored one, so
/// heartbeat reports never overwrite when that version landed.
#[allow(clippy::too_many_arguments)]
pub async fn apply_status_report(
    pool: &SqlitePool,
    host_id: &str,
    instance_id: &str,
    applied_version: i64,
    applied_sha256: &str,
    last_fetch_error: Option<&str>,
    consecutive_failures: i64,
) -> Result<(), AppError> {
    sqlx::query(
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
    )
    .bind(applied_version)
    .bind(applied_sha256)
    .bind(applied_version)
    .bind(applied_version)
    .bind(last_fetch_error)
    .bind(consecutive_failures)
    .bind(applied_version)
    .bind(applied_version)
    .bind(applied_version)
    .bind(host_id)
    .bind(instance_id)
    .execute(pool)
    .await?;
    Ok(())
}
