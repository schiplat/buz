//! Job reservation at trigger time.
//!
//! One named transaction owns the two invariants that make a trigger safe:
//! build numbers are gapless per pipeline, and a concurrent `disable` cannot
//! slip a job in between the trigger's earlier `is_active` check and the row
//! insert.

use serde_json::Value;
use shared::error::AppError;
use sqlx::SqlitePool;

use crate::store::tx;

/// Everything needed to insert one triggered job.
pub struct ReservedJob<'a> {
    pub job_id: &'a str,
    pub pipeline_id: &'a str,
    /// `pending` or `waiting_approval` (approval gate already decided).
    pub status: &'a str,
    pub triggered_by: &'a str,
    pub trigger_params_json: &'a str,
    pub snapshot: &'a Value,
    pub debug_unredact: bool,
    pub verbose: bool,
    pub environment_id: Option<&'a str>,
    pub trigger_user_id: Option<&'a str>,
    pub resource_profile: &'a str,
}

/// Reserve the next build number and insert the job, atomically.
///
/// `BEGIN IMMEDIATE` serializes against `disable`, whose transaction flips
/// `is_active` and cancels in-flight jobs under the same write lock. Re-checking
/// `is_active` inside this transaction is what closes the check-to-insert race.
///
/// Returns the allocated `run_number`.
pub async fn reserve_job(pool: &SqlitePool, job: &ReservedJob<'_>) -> Result<i64, AppError> {
    let mut tx = tx::begin_immediate(pool).await?;

    match sqlx::query_scalar::<_, i64>("SELECT is_active FROM pipelines WHERE id = ?")
        .bind(job.pipeline_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?
    {
        None => return Err(AppError::NotFound("pipeline not found".into())),
        Some(0) => {
            return Err(AppError::Conflict(
                "pipeline is disabled — enable it before triggering".into(),
            ))
        }
        Some(_) => {}
    }

    let run_number = crate::db::allocate_run_number(&mut tx, job.pipeline_id)
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;

    sqlx::query(
        "INSERT INTO jobs (id, pipeline_id, status, trigger_by, trigger_params, snapshot,
                           debug_unredact, verbose_logs, environment_id, trigger_user_id,
                           run_number, run_attempt, resource_profile, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 1, ?, datetime('now'), datetime('now'))",
    )
    .bind(job.job_id)
    .bind(job.pipeline_id)
    .bind(job.status)
    .bind(job.triggered_by)
    .bind(job.trigger_params_json)
    .bind(serde_json::to_string(job.snapshot).unwrap_or_else(|_| "{}".into()))
    .bind(if job.debug_unredact { 1 } else { 0 })
    .bind(if job.verbose { 1 } else { 0 })
    .bind(job.environment_id)
    .bind(job.trigger_user_id)
    .bind(run_number)
    .bind(job.resource_profile)
    .execute(&mut *tx)
    .await?;

    tx.commit()
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;
    Ok(run_number)
}
