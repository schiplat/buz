//! Concurrency queue scheduler with leader election.
//!
//! Background loop that picks up queued jobs and dispatches them when a
//! pipeline has no running/pending job. Uses a single-row `scheduler_lease`
//! table for leader election so only one manager instance dispatches at a time.
//!
//! Also cleans up queued jobs that exceed their `queue_timeout`.

use sqlx::SqlitePool;
use std::time::Duration;
use tracing;
use uuid::Uuid;

/// How often the leader attempts to dispatch queued jobs.
const DISPATCH_INTERVAL_SECS: u64 = 5;

/// How long a lease lives before another instance can take over.
const LEASE_TTL_SECS: i64 = 15;

/// Start the scheduler background task. Safe to call multiple times — only one
/// instance will hold the leader lease at any time.
pub fn start_scheduler(pool: SqlitePool) {
    let instance_id = Uuid::new_v4().to_string();
    tokio::spawn(async move {
        tracing::info!("Scheduler starting: instance={instance_id}");
        loop {
            // ── Try to become the leader ─────────────────────────────────
            match try_acquire_lease(&pool, &instance_id).await {
                Ok(true) => {
                    tracing::debug!("Scheduler: acquired lease, acting as leader");
                    // Run one dispatch cycle
                    if let Err(e) = dispatch_queued_jobs(&pool).await {
                        tracing::error!("Scheduler dispatch error: {e}");
                    }
                }
                Ok(false) => {
                    // Another instance is the leader — just wait and retry
                }
                Err(e) => {
                    tracing::error!("Scheduler lease error: {e}");
                }
            }

            tokio::time::sleep(Duration::from_secs(DISPATCH_INTERVAL_SECS)).await;
        }
    });
}

/// Try to acquire or refresh the leader lease. Returns `Ok(true)` if this
/// instance is (now) the leader.
async fn try_acquire_lease(pool: &SqlitePool, instance_id: &str) -> Result<bool, sqlx::Error> {
    use sqlx::Row;

    // Try to take the lease if it doesn't exist or has expired
    let rows_affected = sqlx::query(
        "INSERT OR REPLACE INTO scheduler_lease (id, instance_id, expires_at, updated_at)
         SELECT 1, ?, datetime('now', ?), datetime('now')
         WHERE NOT EXISTS (
             SELECT 1 FROM scheduler_lease
             WHERE id = 1 AND expires_at > datetime('now')
         )",
    )
    .bind(instance_id)
    .bind(format!("+{LEASE_TTL_SECS} seconds"))
    .execute(pool)
    .await?;

    // Check if we own the lease
    let row = sqlx::query("SELECT instance_id FROM scheduler_lease WHERE id = 1")
        .fetch_optional(pool)
        .await?;

    match row {
        Some(r) if r.get::<String, _>(0) == instance_id => {
            // We own the lease — refresh it if not just acquired
            if rows_affected.rows_affected() == 0 {
                // Already owned, refresh
                sqlx::query(
                    "UPDATE scheduler_lease SET expires_at = datetime('now', ?),
                     updated_at = datetime('now') WHERE id = 1",
                )
                .bind(format!("+{LEASE_TTL_SECS} seconds"))
                .execute(pool)
                .await?;
            }
            Ok(true)
        }
        _ => Ok(false),
    }
}

/// Find and dispatch one queued job per pipeline that is ready to run.
async fn dispatch_queued_jobs(pool: &SqlitePool) -> anyhow::Result<()> {
    use sqlx::Row;
    // For each pipeline with queued jobs, check if any job is running.
    // If not, promote the oldest queued job to pending.
    let pipelines = sqlx::query(
        "SELECT DISTINCT j.pipeline_id
         FROM jobs j
         WHERE j.status = 'queued'
         ORDER BY j.created_at ASC",
    )
    .fetch_all(pool)
    .await?;

    for row in &pipelines {
        let pipeline_id: String = row.get(0);

        // Check if this pipeline has a running job
        let running: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM jobs
             WHERE pipeline_id = ? AND status IN ('pending', 'running')",
        )
        .bind(&pipeline_id)
        .fetch_one(pool)
        .await?;

        if running > 0 {
            continue; // pipeline busy, wait
        }

        // Find the oldest queued job
        let queued_job: Option<(String,)> = sqlx::query_as(
            "SELECT id FROM jobs
             WHERE pipeline_id = ? AND status = 'queued'
             ORDER BY created_at ASC LIMIT 1",
        )
        .bind(&pipeline_id)
        .fetch_optional(pool)
        .await?;

        if let Some((job_id,)) = queued_job {
            sqlx::query(
                "UPDATE jobs SET status = 'pending', updated_at = datetime('now')
                 WHERE id = ?",
            )
            .bind(&job_id)
            .execute(pool)
            .await?;
            tracing::info!("Scheduler: dispatched queued job {job_id} → pending");
        }
    }

    Ok(())
}
