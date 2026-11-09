//! Terminal-job log retention.
//!
//! `job_logs` grows unbounded otherwise: every uploaded chunk is a row, and
//! nothing pruned them (the debug cleaner only covers `debug_unredact`
//! jobs). This purges the log rows of TERMINAL jobs past the retention
//! window — the job rows themselves stay for history, dashboards, and
//! audit; only the log bodies are dropped. The window is the
//! `job_log_retention_days` platform setting (default 30, `0` = keep
//! forever).

use sqlx::SqlitePool;
use std::time::Duration;
use tokio::time;

use crate::services::log_broadcast::LogBroadcaster;
use crate::services::platform_settings::{
    effective, DEFAULT_JOB_LOG_RETENTION_DAYS, KEY_JOB_LOG_RETENTION_DAYS,
};

/// Rows purged per cycle — bounds each tick's work on a large backlog; the
/// next hourly tick continues from the `logs_purged` marker.
const BATCH: i64 = 500;

/// Purge log rows of terminal jobs older than the retention window.
/// Returns the number of jobs whose logs were purged. Exposed for testing;
/// the spawned task calls it on an hourly timer.
pub async fn purge_expired_job_logs(
    pool: &SqlitePool,
    broadcaster: &LogBroadcaster,
) -> anyhow::Result<usize> {
    let days: i64 = effective(
        pool,
        KEY_JOB_LOG_RETENTION_DAYS,
        &DEFAULT_JOB_LOG_RETENTION_DAYS.to_string(),
    )
    .await
    .trim()
    .parse()
    .unwrap_or(DEFAULT_JOB_LOG_RETENTION_DAYS as i64);
    if days <= 0 {
        return Ok(0); // keep forever
    }

    let expired: Vec<(String,)> = sqlx::query_as(
        "SELECT id FROM jobs
         WHERE status IN ('success', 'failed', 'cancelled', 'timeout')
           AND debug_unredact = 0
           AND logs_purged = 0
           AND created_at < datetime('now', ?)
         ORDER BY created_at
         LIMIT ?",
    )
    .bind(format!("-{days} days"))
    .bind(BATCH)
    .fetch_all(pool)
    .await?;

    let mut purged = 0usize;
    for (job_id,) in &expired {
        let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
        sqlx::query("DELETE FROM job_logs WHERE job_id = ?")
            .bind(job_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE jobs SET logs_purged = 1 WHERE id = ?")
            .bind(job_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        // Drop the in-memory WS replay sink so purged chunks can never be
        // replayed over the job log stream.
        broadcaster.cleanup(job_id);
        purged += 1;
    }

    Ok(purged)
}

/// Hourly retention sweep. Debug (unredacted) jobs are excluded here — the
/// dedicated debug cleaner drops their plaintext logs after 24h and resets
/// the flag, after which normal retention applies to the (already empty)
/// remainder.
pub fn spawn_job_log_retention(pool: SqlitePool, broadcaster: LogBroadcaster) {
    tokio::spawn(async move {
        tracing::info!(
            "Job log retention started (default: {}d via {KEY_JOB_LOG_RETENTION_DAYS}, check hourly)",
            DEFAULT_JOB_LOG_RETENTION_DAYS
        );

        let mut interval = time::interval(Duration::from_secs(3600));

        loop {
            interval.tick().await;

            match purge_expired_job_logs(&pool, &broadcaster).await {
                Ok(0) => {}
                Ok(n) => tracing::info!("Job log retention: purged logs of {n} expired job(s)"),
                Err(e) => tracing::error!("Job log retention failed: {e}"),
            }
        }
    });
}
