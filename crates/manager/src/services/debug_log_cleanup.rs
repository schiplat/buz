use sqlx::SqlitePool;
use std::time::Duration;
use tokio::time;

use crate::services::log_broadcast::LogBroadcaster;

/// Debug (unredacted) job logs are kept for this many hours, then purged.
pub const DEBUG_LOG_RETENTION_HOURS: i64 = 24;

/// Delete log rows for debug jobs older than the retention window, reset
/// their `debug_unredact` flag so the admin-only read gate and the UI banner
/// stop applying once the plaintext logs are gone, and drop the in-memory
/// WebSocket replay sink so the purged chunks can never be replayed. The job
/// row itself is kept for history/audit. Exposed for testing; the spawned
/// task calls it on a timer.
pub async fn purge_expired_debug_logs(
    pool: &SqlitePool,
    broadcaster: &LogBroadcaster,
) -> anyhow::Result<usize> {
    let cutoff = format!("-{DEBUG_LOG_RETENTION_HOURS} hours");
    let expired: Vec<(String,)> = sqlx::query_as(
        "SELECT id FROM jobs
         WHERE debug_unredact = 1
           AND created_at < datetime('now', ?)",
    )
    .bind(&cutoff)
    .fetch_all(pool)
    .await?;

    for (job_id,) in &expired {
        sqlx::query("DELETE FROM job_logs WHERE job_id = ?")
            .bind(job_id)
            .execute(pool)
            .await?;
        sqlx::query(
            "UPDATE jobs SET debug_unredact = 0, updated_at = datetime('now') WHERE id = ?",
        )
        .bind(job_id)
        .execute(pool)
        .await?;
        // Drop the in-memory sink: `broadcast()` retained up to 256 unredacted
        // chunks per job regardless of WS clients, and `cleanup()` normally
        // only runs on WS disconnect — so without this a non-admin could
        // replay the plaintext chunks via the job log stream after the DB rows
        // (and the admin-only gate) are gone.
        broadcaster.cleanup(job_id);
    }

    Ok(expired.len())
}

/// Periodically purge debug job logs older than [`DEBUG_LOG_RETENTION_HOURS`].
pub fn spawn_debug_log_cleanup(pool: SqlitePool, broadcaster: LogBroadcaster) {
    tokio::spawn(async move {
        tracing::info!(
            "Debug log cleanup started (retention: {}h, check every 5m)",
            DEBUG_LOG_RETENTION_HOURS
        );

        let mut interval = time::interval(Duration::from_secs(300));

        loop {
            interval.tick().await;

            match purge_expired_debug_logs(&pool, &broadcaster).await {
                Ok(0) => {}
                Ok(n) => {
                    tracing::info!("Debug log cleanup: purged logs for {n} expired debug job(s)")
                }
                Err(e) => tracing::error!("Debug log cleanup failed: {e}"),
            }
        }
    });
}
