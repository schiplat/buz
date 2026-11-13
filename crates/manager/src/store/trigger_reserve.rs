//! Job reservation at trigger time.
//!
//! One named transaction owns the two invariants that make a trigger safe:
//! build numbers are gapless per pipeline, and a concurrent `disable` cannot
//! slip a job in between the trigger's earlier `is_active` check and the row
//! insert.

use serde_json::Value;
use shared::error::AppError;

use crate::store::exec::{DbPool, SqlValue};

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
pub async fn reserve_job(pool: &DbPool, job: &ReservedJob<'_>) -> Result<i64, AppError> {
    let mut tx = pool
        .begin_immediate()
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;

    match tx
        .fetch_optional(
            "SELECT is_active FROM pipelines WHERE id = ?",
            &[job.pipeline_id.into()],
        )
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?
    {
        None => return Err(AppError::NotFound("pipeline not found".into())),
        Some(row) if row.i64(0) == 0 => {
            return Err(AppError::Conflict(
                "pipeline is disabled — enable it before triggering".into(),
            ))
        }
        Some(_) => {}
    }

    // Gapless per-pipeline build numbers: the UPDATE…RETURNING runs under the
    // same write lock as the insert (the SQLite trigger path's replacement).
    let run_number = tx
        .fetch_optional(
            "UPDATE pipelines SET run_counter = run_counter + 1 \
             WHERE id = ? RETURNING run_counter",
            &[job.pipeline_id.into()],
        )
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?
        .map(|r| r.i64(0))
        .ok_or_else(|| AppError::NotFound("pipeline not found".into()))?;

    tx.execute(
        "INSERT INTO jobs (id, pipeline_id, status, trigger_by, trigger_params, snapshot,
                           debug_unredact, verbose_logs, environment_id, trigger_user_id,
                           run_number, run_attempt, resource_profile, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 1, ?, datetime('now'), datetime('now'))",
        &[
            job.job_id.into(),
            job.pipeline_id.into(),
            job.status.into(),
            job.triggered_by.into(),
            job.trigger_params_json.into(),
            serde_json::to_string(job.snapshot)
                .unwrap_or_else(|_| "{}".into())
                .into(),
            SqlValue::Int(if job.debug_unredact { 1 } else { 0 }),
            SqlValue::Int(if job.verbose { 1 } else { 0 }),
            job.environment_id.into(),
            job.trigger_user_id.into(),
            SqlValue::Int(run_number),
            job.resource_profile.into(),
        ],
    )
    .await?;

    tx.commit()
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;
    Ok(run_number)
}
