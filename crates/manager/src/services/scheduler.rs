//! Concurrency queue scheduler with leader election.
//!
//! Background loop that picks up queued jobs and dispatches them when a
//! pipeline has no running/pending job. Uses a single-row `scheduler_lease`
//! table for leader election so only one manager instance dispatches at a time.
//!
//! Also cleans up queued jobs that exceed their `queue_timeout`, and cancels
//! `waiting_approval` jobs whose environment approval timeout has expired.

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
            match try_acquire_lease(&pool, JOB_DISPATCH_LEASE_ID, &instance_id).await {
                Ok(true) => {
                    tracing::debug!("Scheduler: acquired lease, acting as leader");
                    // Run one dispatch cycle
                    if let Err(e) = dispatch_queued_jobs(&pool).await {
                        tracing::error!("Scheduler dispatch error: {e}");
                    }
                    if let Err(e) = expire_approval_timeouts(&pool).await {
                        tracing::error!("Scheduler approval-timeout error: {e}");
                    }
                    if let Err(e) = expire_queue_timeouts(&pool).await {
                        tracing::error!("Scheduler queue-timeout error: {e}");
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

/// Lease row used by the job-dispatch scheduler (`scheduler_lease.id`).
pub const JOB_DISPATCH_LEASE_ID: i64 = 1;

/// Lease row used by the cron trigger scheduler (`scheduler_lease.id`).
/// Rows 1/2 are the job-dispatch and GitHub-fetch schedulers; a dedicated
/// row per scheduler so none ever evicts another's lease.
pub const CRON_TRIGGER_LEASE_ID: i64 = 3;

/// Try to acquire or refresh the leader lease on a specific lease row.
///
/// Different background schedulers use different `lease_id` rows of the shared
/// `scheduler_lease` table so they never kick each other out. Exposed for
/// testing.
pub async fn try_acquire_lease(
    pool: &SqlitePool,
    lease_id: i64,
    instance_id: &str,
) -> Result<bool, sqlx::Error> {
    use sqlx::Row;

    // Try to take the lease if it doesn't exist or has expired
    let rows_affected = sqlx::query(
        "INSERT OR REPLACE INTO scheduler_lease (id, instance_id, expires_at, updated_at)
         SELECT ?, ?, datetime('now', ?), datetime('now')
         WHERE NOT EXISTS (
             SELECT 1 FROM scheduler_lease
             WHERE id = ? AND expires_at > datetime('now')
         )",
    )
    .bind(lease_id)
    .bind(instance_id)
    .bind(format!("+{LEASE_TTL_SECS} seconds"))
    .bind(lease_id)
    .execute(pool)
    .await?;

    // Check if we own the lease
    let row = sqlx::query("SELECT instance_id FROM scheduler_lease WHERE id = ?")
        .bind(lease_id)
        .fetch_optional(pool)
        .await?;

    match row {
        Some(r) if r.get::<String, _>(0) == instance_id => {
            // We own the lease — refresh it if not just acquired
            if rows_affected.rows_affected() == 0 {
                // Already owned, refresh
                sqlx::query(
                    "UPDATE scheduler_lease SET expires_at = datetime('now', ?),
                     updated_at = datetime('now') WHERE id = ?",
                )
                .bind(format!("+{LEASE_TTL_SECS} seconds"))
                .bind(lease_id)
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
            crate::services::poll_events::wake_all();
            tracing::info!("Scheduler: dispatched queued job {job_id} → pending");
        }
    }

    Ok(())
}

/// Cancel `waiting_approval` jobs whose per-environment approval timeout elapsed.
async fn expire_approval_timeouts(pool: &SqlitePool) -> anyhow::Result<()> {
    use sqlx::Row;

    tracing::debug!("environments: approval timeout scan started");

    let rows =
        sqlx::query("SELECT id, snapshot, created_at FROM jobs WHERE status = 'waiting_approval'")
            .fetch_all(pool)
            .await?;

    let now = chrono::Utc::now();
    let mut cancelled = 0i64;

    for row in rows {
        let job_id: String = row.get(0);
        let snapshot_raw: String = row.get(1);
        let created_at_raw: String = row.get(2);

        let snapshot: serde_json::Value =
            serde_json::from_str(&snapshot_raw).unwrap_or(serde_json::json!({}));
        let timeout_secs = snapshot
            .get("protection")
            .and_then(|p| p.get("approval_timeout_seconds"))
            .and_then(|v| v.as_i64())
            .unwrap_or(86400)
            .max(1);

        let created = parse_sqlite_datetime(&created_at_raw).unwrap_or(now);
        let age = (now - created).num_seconds();
        if age < timeout_secs {
            continue;
        }

        let mut snapshot = snapshot;
        let received = snapshot
            .get("approval")
            .and_then(|a| a.get("received"))
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        if let Some(map) = snapshot.as_object_mut() {
            map.insert(
                "approval".into(),
                serde_json::json!({
                    "required": true,
                    "received": received,
                    "outcome": "timeout",
                }),
            );
        }
        let snapshot_str = serde_json::to_string(&snapshot).unwrap_or_default();

        let result = sqlx::query(
            "UPDATE jobs SET status = 'cancelled', snapshot = ?,
                    end_time = datetime('now'), updated_at = datetime('now')
             WHERE id = ? AND status = 'waiting_approval'",
        )
        .bind(&snapshot_str)
        .bind(&job_id)
        .execute(pool)
        .await?;

        if result.rows_affected() > 0 {
            cancelled += 1;
            let _ = sqlx::query(
                "INSERT INTO job_approvals (id, job_id, user_id, username, action, comment)
                 VALUES (?, ?, 'system', 'system', 'timeout', 'approval timeout')",
            )
            .bind(uuid::Uuid::new_v4().to_string())
            .bind(&job_id)
            .execute(pool)
            .await;
            tracing::info!(
                job_id = %job_id,
                timeout_secs,
                age_secs = age,
                "environments: approval timed out"
            );
        }
    }

    if cancelled > 0 {
        crate::services::poll_events::wake_all();
        tracing::info!(cancelled, "environments: approval timeout scan finished");
    } else {
        tracing::debug!("environments: approval timeout scan finished (none expired)");
    }

    Ok(())
}

/// Cancel `queued` jobs that have waited past their pipeline's concurrency
/// `queue_timeout` (seconds). The value is frozen into the job snapshot at
/// trigger time; absent → the 1800s save-time default. `updated_at` on a
/// queued job marks its last status transition — the moment it entered the
/// queue. System cancellation mirrors [`expire_approval_timeouts`]: no audit
/// row, no notify (neither scheduler expiry has the notify context here);
/// the snapshot annotation is the cancel-reason trail. Exposed for testing;
/// the leader loop calls it on its dispatch cadence.
pub async fn expire_queue_timeouts(pool: &SqlitePool) -> anyhow::Result<()> {
    use sqlx::Row;

    let rows = sqlx::query("SELECT id, snapshot, updated_at FROM jobs WHERE status = 'queued'")
        .fetch_all(pool)
        .await?;

    let now = chrono::Utc::now();
    let mut cancelled = 0i64;

    for row in rows {
        let job_id: String = row.get(0);
        let snapshot_raw: String = row.get(1);
        let updated_at_raw: String = row.get(2);

        let snapshot: serde_json::Value =
            serde_json::from_str(&snapshot_raw).unwrap_or(serde_json::json!({}));
        let timeout_secs = snapshot
            .get("concurrency_queue_timeout_seconds")
            .and_then(|v| v.as_i64())
            .unwrap_or(1800)
            .max(1);

        let entered = parse_sqlite_datetime(&updated_at_raw).unwrap_or(now);
        let age = (now - entered).num_seconds();
        if age < timeout_secs {
            continue;
        }

        let mut snapshot = snapshot;
        if let Some(map) = snapshot.as_object_mut() {
            map.insert(
                "concurrency_queue_timed_out".into(),
                serde_json::json!(true),
            );
        }
        let snapshot_str = serde_json::to_string(&snapshot).unwrap_or_default();

        let result = sqlx::query(
            "UPDATE jobs SET status = 'cancelled', snapshot = ?,
                    end_time = datetime('now'), updated_at = datetime('now')
             WHERE id = ? AND status = 'queued'",
        )
        .bind(&snapshot_str)
        .bind(&job_id)
        .execute(pool)
        .await?;

        if result.rows_affected() > 0 {
            cancelled += 1;
            tracing::info!(
                job_id = %job_id,
                timeout_secs,
                age_secs = age,
                "scheduler: queued job cancelled (queue timeout)"
            );
        }
    }

    if cancelled > 0 {
        crate::services::poll_events::wake_all();
    }

    Ok(())
}

fn parse_sqlite_datetime(s: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    // SQLite datetime('now') → "YYYY-MM-DD HH:MM:SS"
    chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S")
        .ok()
        .map(|ndt| ndt.and_utc())
        .or_else(|| {
            chrono::DateTime::parse_from_rfc3339(s)
                .ok()
                .map(|dt| dt.with_timezone(&chrono::Utc))
        })
}
