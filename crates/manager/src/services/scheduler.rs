//! Concurrency queue scheduler with leader election.
//!
//! Background loop that picks up queued jobs and dispatches them when a
//! pipeline has no running/pending job. Uses a single-row `scheduler_lease`
//! table for leader election so only one manager instance dispatches at a time.
//!
//! Also cancels `queued` jobs past their frozen `queue_timeout` and
//! `waiting_approval` jobs whose environment approval timeout has expired.
//!
//! # Where this sits in the scheduling chain
//!
//! Five gates can hold a run. Full table (with docs links):
//! `docs/content/docs/01-architecture/execution/scheduling-precedence.md`.
//!
//! 1. **Trigger** (`api::pipelines::do_trigger`): pipeline disabled →
//!    resource-profile grant → input validation → declared environment →
//!    environment host allowlist → **protection** (→ `waiting_approval`, which
//!    skips concurrency) → **pipeline concurrency** (`pending` / `queued` /
//!    409). Pipeline concurrency is evaluated again when approvals complete.
//! 2. **This loop** (leader only, every 5s): expire `queue_timeout`, expire
//!    approval timeouts, promote the oldest `queued` job to `pending` when its
//!    pipeline has no `pending`/`running` job.
//! 3. **Claim** (`api::runners::poll_once`, inside `BEGIN IMMEDIATE`): Runner
//!    frozen/capacity → team binding → **environment deployment_concurrency** →
//!    labels → profile grant recheck → **sandbox host slot budget**
//!    (`slot_scheduler`, `backfill`/`fifo` + starvation aging).
//!
//! Rules worth keeping in mind when changing any of these:
//!
//! - Approval outranks concurrency: `waiting_approval` never consults the
//!   concurrency group, and the queue clock starts only when it enters
//!   `queued` (tracked via `updated_at`).
//! - Pipeline concurrency filters on `pipeline_id`; the `concurrency.group`
//!   string is a label, not a cross-pipeline lock.
//! - Environment concurrency and the slot budget are evaluated at claim, not
//!   at trigger, so a job can pass one and still wait at the other.
//! - `backfill`/`fifo` and starvation aging only matter when jobs wait for
//!   resource slots; with only the default profile they are equivalent.

use crate::store::exec::DbPool;
use std::time::Duration;
use tracing;
use uuid::Uuid;

/// How often the leader attempts to dispatch queued jobs.
const DISPATCH_INTERVAL_SECS: u64 = 5;

/// How long a lease lives before another instance can take over.
const LEASE_TTL_SECS: i64 = 15;

/// Start the scheduler background task. Safe to call multiple times — only one
/// instance will hold the leader lease at any time.
pub fn start_scheduler(pool: DbPool) {
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
    pool: &DbPool,
    lease_id: i64,
    instance_id: &str,
) -> Result<bool, sqlx::Error> {
    crate::store::scheduler::try_acquire_lease(pool, lease_id, instance_id, LEASE_TTL_SECS)
        .await
        .map_err(|e| sqlx::Error::Configuration(Box::new(e)))
}

/// Find and dispatch one queued job per pipeline that is ready to run.
async fn dispatch_queued_jobs(pool: &DbPool) -> anyhow::Result<()> {
    // For each pipeline with queued jobs, check if any job is running.
    // If not, promote the oldest queued job to pending.
    let pipelines = crate::store::scheduler::queued_pipeline_ids(pool)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    for pipeline_id in &pipelines {
        // Check if this pipeline has a running job
        let running = crate::store::scheduler::active_job_count(pool, pipeline_id)
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?;

        if running > 0 {
            continue; // pipeline busy, wait
        }

        // Find the oldest queued job
        let queued_job = crate::store::scheduler::oldest_queued_job(pool, pipeline_id)
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?;

        if let Some(job_id) = queued_job {
            crate::store::scheduler::promote_queued_job(pool, &job_id)
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            crate::services::poll_events::wake_all();
            tracing::info!("Scheduler: dispatched queued job {job_id} → pending");
        }
    }

    Ok(())
}

/// Cancel `waiting_approval` jobs whose per-environment approval timeout elapsed.
async fn expire_approval_timeouts(pool: &DbPool) -> anyhow::Result<()> {
    tracing::debug!("environments: approval timeout scan started");

    let rows = crate::store::scheduler::waiting_approval_jobs(pool)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    let now = chrono::Utc::now();
    let mut cancelled = 0i64;

    for (job_id, snapshot_raw, created_at_raw) in rows {
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

        let applied =
            crate::store::scheduler::cancel_approval_timeout(pool, &job_id, &snapshot_str)
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))?;

        if applied {
            cancelled += 1;
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
pub async fn expire_queue_timeouts(pool: &DbPool) -> anyhow::Result<()> {
    let rows = crate::store::scheduler::queued_jobs(pool)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    let now = chrono::Utc::now();
    let mut cancelled = 0i64;

    for (job_id, snapshot_raw, updated_at_raw) in rows {
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

        let applied = crate::store::scheduler::cancel_queued_job(pool, &job_id, &snapshot_str)
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?;

        if applied {
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

/// Parse SQLite `datetime('now')` ("YYYY-MM-DD HH:MM:SS") or RFC 3339.
pub(crate) fn parse_sqlite_datetime(s: &str) -> Option<chrono::DateTime<chrono::Utc>> {
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
