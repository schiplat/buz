use sqlx::{Row, SqlitePool};
use std::time::Duration;
use tokio::time;
use uuid::Uuid;

use crate::config::RunnerConfig;

/// Periodically scan for runners whose last heartbeat is older than
/// `heartbeat_timeout_seconds` and mark them as `offline`.
pub fn spawn_monitor(pool: SqlitePool, runner_config: RunnerConfig) {
    let timeout_secs = runner_config.heartbeat_timeout_seconds;
    tokio::spawn(async move {
        tracing::info!(
            "Runner monitor started (heartbeat timeout: {timeout_secs}s, check every 15s)"
        );

        let mut interval = time::interval(Duration::from_secs(15));

        loop {
            interval.tick().await;

            // Fetch IDs of runners that should be marked offline
            let stale_rows = match sqlx::query(
                "SELECT id, name FROM runners
                 WHERE status = 'online'
                   AND is_frozen = 0
                   AND last_seen < datetime('now', ?)",
            )
            .bind(format!("-{timeout_secs} seconds"))
            .fetch_all(&pool)
            .await
            {
                Ok(r) => r,
                Err(e) => {
                    tracing::error!("Runner monitor query failed: {e}");
                    continue;
                }
            };

            if stale_rows.is_empty() {
                continue;
            }

            // Mark them offline
            let result = sqlx::query(
                "UPDATE runners
                 SET status = 'offline',
                     updated_at = datetime('now')
                 WHERE status = 'online'
                   AND is_frozen = 0
                   AND last_seen < datetime('now', ?)",
            )
            .bind(format!("-{timeout_secs} seconds"))
            .execute(&pool)
            .await;

            let count = match result {
                Ok(r) => r.rows_affected(),
                Err(e) => {
                    tracing::error!("Runner monitor update failed: {e}");
                    continue;
                }
            };

            tracing::info!("Runner monitor: marked {count} runner(s) as offline");

            // Log an event for each stale runner
            for row in &stale_rows {
                let id: String = row.get(0);
                let name: String = row.get(1);
                let event_id = Uuid::new_v4().to_string();
                let _ = sqlx::query(
                    "INSERT INTO runner_events (id, runner_id, event_type, message)
                     VALUES (?, ?, 'offline', ?)",
                )
                .bind(&event_id)
                .bind(&id)
                .bind(format!("Heartbeat timeout — runner {name} marked offline"))
                .execute(&pool)
                .await;

                // ── Fail any running/pending jobs left behind by this runner ──
                let orphaned: Vec<(String,)> = sqlx::query_as(
                    "SELECT id FROM jobs WHERE runner_id = ? AND status IN ('running', 'pending')",
                )
                .bind(&id)
                .fetch_all(&pool)
                .await
                .unwrap_or_default();

                for (job_id,) in &orphaned {
                    // Mark job as failed
                    let _ = sqlx::query(
                        "UPDATE jobs SET status = 'failed', end_time = datetime('now'), updated_at = datetime('now') WHERE id = ?",
                    )
                    .bind(job_id)
                    .execute(&pool)
                    .await;

                    // Append a log chunk explaining what happened
                    let log_id = Uuid::new_v4().to_string();
                    let _ = sqlx::query(
                        "INSERT INTO job_logs (id, job_id, chunk_index, step_order, content, created_at)
                         VALUES (?, ?, -2, -1, ?, datetime('now'))",
                    )
                    .bind(&log_id)
                    .bind(job_id)
                    .bind(format!(
                        "==> RUNNER LOST\nRunner '{name}' went offline while executing this job. The job has been marked as failed.\n"
                    ))
                    .execute(&pool)
                    .await;
                }

                if !orphaned.is_empty() {
                    tracing::warn!(
                        "Runner monitor: failed {} orphaned job(s) left by runner '{name}'",
                        orphaned.len()
                    );
                }
            }
        }
    });
}
