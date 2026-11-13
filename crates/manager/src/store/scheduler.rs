//! Scheduler lease and queue-transition statements.
//!
//! The leader lease is one row per scheduler in `scheduler_lease`; the queue
//! transitions are guarded single-statement updates, so no transaction is
//! needed here (see `transaction-boundaries.md`).

use crate::store::exec::{self, DbPool, SqlValue};

use shared::error::AppError;

/// Try to take or renew one scheduler lease row. `ttl_secs` is the lease
/// lifetime; a row another instance holds unexpired is left untouched.
pub async fn try_acquire_lease(
    pool: &DbPool,
    lease_id: i64,
    instance_id: &str,
    ttl_secs: i64,
) -> Result<bool, AppError> {
    let expires = format!("+{ttl_secs} seconds");
    let acquired = exec::execute(
        pool,
        "INSERT OR REPLACE INTO scheduler_lease (id, instance_id, expires_at, updated_at)
         SELECT ?, ?, datetime('now', ?), datetime('now')
         WHERE NOT EXISTS (
             SELECT 1 FROM scheduler_lease
             WHERE id = ? AND expires_at > datetime('now')
         )",
        &[
            SqlValue::Int(lease_id),
            instance_id.into(),
            expires.as_str().into(),
            SqlValue::Int(lease_id),
        ],
    )
    .await?;

    let owner = exec::fetch_optional(
        pool,
        "SELECT instance_id FROM scheduler_lease WHERE id = ?",
        &[SqlValue::Int(lease_id)],
    )
    .await?
    .map(|r| r.text(0));

    match owner {
        Some(owner) if owner == instance_id => {
            // We own the lease — refresh it when it was not just acquired.
            if acquired == 0 {
                exec::execute(
                    pool,
                    "UPDATE scheduler_lease SET expires_at = datetime('now', ?),
                     updated_at = datetime('now') WHERE id = ?",
                    &[expires.as_str().into(), SqlValue::Int(lease_id)],
                )
                .await?;
            }
            Ok(true)
        }
        _ => Ok(false),
    }
}

/// Pipeline ids with at least one queued job, oldest queued first.
pub async fn queued_pipeline_ids(pool: &DbPool) -> Result<Vec<String>, AppError> {
    Ok(exec::fetch_all(
        pool,
        "SELECT DISTINCT j.pipeline_id
         FROM jobs j
         WHERE j.status = 'queued'
         ORDER BY j.created_at ASC",
        &[],
    )
    .await?
    .into_iter()
    .map(|r| r.text(0))
    .collect())
}

/// Pending/running job count for one pipeline (the promotion gate).
pub async fn active_job_count(pool: &DbPool, pipeline_id: &str) -> Result<i64, AppError> {
    Ok(exec::fetch_scalar_i64(
        pool,
        "SELECT COUNT(*) FROM jobs
         WHERE pipeline_id = ? AND status IN ('pending', 'running')",
        &[pipeline_id.into()],
    )
    .await?)
}

/// Oldest queued job id for one pipeline (`None` = none left).
pub async fn oldest_queued_job(
    pool: &DbPool,
    pipeline_id: &str,
) -> Result<Option<String>, AppError> {
    Ok(exec::fetch_optional(
        pool,
        "SELECT id FROM jobs
         WHERE pipeline_id = ? AND status = 'queued'
         ORDER BY created_at ASC LIMIT 1",
        &[pipeline_id.into()],
    )
    .await?
    .map(|r| r.text(0)))
}

/// Promote one queued job to pending.
pub async fn promote_queued_job(pool: &DbPool, job_id: &str) -> Result<(), AppError> {
    exec::execute(
        pool,
        "UPDATE jobs SET status = 'pending', updated_at = datetime('now')
         WHERE id = ?",
        &[job_id.into()],
    )
    .await?;
    Ok(())
}

/// `(id, snapshot, created_at)` of every job still waiting for approval.
pub async fn waiting_approval_jobs(
    pool: &DbPool,
) -> Result<Vec<(String, String, String)>, AppError> {
    Ok(exec::fetch_all(
        pool,
        "SELECT id, snapshot, created_at FROM jobs WHERE status = 'waiting_approval'",
        &[],
    )
    .await?
    .into_iter()
    .map(|r| (r.text(0), r.text(1), r.text(2)))
    .collect())
}

/// Cancel one waiting_approval job past its approval timeout, recording the
/// timeout outcome. Returns whether the guarded update applied. The approval
/// row is best effort, as before.
pub async fn cancel_approval_timeout(
    pool: &DbPool,
    job_id: &str,
    snapshot: &str,
) -> Result<bool, AppError> {
    let affected = exec::execute(
        pool,
        "UPDATE jobs SET status = 'cancelled', snapshot = ?,
                end_time = datetime('now'), updated_at = datetime('now')
         WHERE id = ? AND status = 'waiting_approval'",
        &[snapshot.into(), job_id.into()],
    )
    .await?;
    if affected > 0 {
        let _ = exec::execute(
            pool,
            "INSERT INTO job_approvals (id, job_id, user_id, username, action, comment)
             VALUES (?, ?, 'system', 'system', 'timeout', 'approval timeout')",
            &[uuid::Uuid::new_v4().to_string().into(), job_id.into()],
        )
        .await;
    }
    Ok(affected > 0)
}

/// `(id, snapshot, updated_at)` of every queued job.
pub async fn queued_jobs(pool: &DbPool) -> Result<Vec<(String, String, String)>, AppError> {
    Ok(exec::fetch_all(
        pool,
        "SELECT id, snapshot, updated_at FROM jobs WHERE status = 'queued'",
        &[],
    )
    .await?
    .into_iter()
    .map(|r| (r.text(0), r.text(1), r.text(2)))
    .collect())
}

/// Cancel one queued job past its queue timeout. Returns whether the guarded
/// update applied.
pub async fn cancel_queued_job(
    pool: &DbPool,
    job_id: &str,
    snapshot: &str,
) -> Result<bool, AppError> {
    let affected = exec::execute(
        pool,
        "UPDATE jobs SET status = 'cancelled', snapshot = ?,
                end_time = datetime('now'), updated_at = datetime('now')
         WHERE id = ? AND status = 'queued'",
        &[snapshot.into(), job_id.into()],
    )
    .await?;
    Ok(affected > 0)
}
