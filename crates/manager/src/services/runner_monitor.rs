use crate::config::RunnerConfig;
use crate::store::exec::DbPool;
use std::time::Duration;
use tokio::time;

/// Periodically scan for runners whose last heartbeat is older than
/// `heartbeat_timeout_seconds` and mark them as `offline`.
pub fn spawn_monitor(pool: DbPool, runner_config: RunnerConfig) {
    let timeout_secs = runner_config.heartbeat_timeout_seconds;
    tokio::spawn(async move {
        tracing::info!(
            "Runner monitor started (heartbeat timeout: {timeout_secs}s, check every 15s)"
        );

        let mut interval = time::interval(Duration::from_secs(15));

        loop {
            interval.tick().await;

            // Fetch the runners that should be marked offline
            let stale_rows =
                match crate::store::runners::stale_online_runners(&pool, timeout_secs).await {
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
            let count = match crate::store::runners::mark_stale_runners_offline(&pool, timeout_secs)
                .await
            {
                Ok(n) => n,
                Err(e) => {
                    tracing::error!("Runner monitor update failed: {e}");
                    continue;
                }
            };

            tracing::info!("Runner monitor: marked {count} runner(s) as offline");

            // Log an event for each stale runner
            for (id, name) in &stale_rows {
                crate::store::runners::log_runner_event(
                    &pool,
                    id,
                    "offline",
                    &format!("Heartbeat timeout — runner {name} marked offline"),
                )
                .await;

                // ── Fail any running/pending jobs left behind by this runner ──
                let orphaned = crate::store::runners::orphaned_job_ids(&pool, id)
                    .await
                    .unwrap_or_default();

                for job_id in &orphaned {
                    crate::store::jobs::fail_orphaned_job(
                        &pool,
                        job_id,
                        &format!(
                            "==> RUNNER LOST\nRunner '{name}' went offline while executing this job. The job has been marked as failed.\n"
                        ),
                    )
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
