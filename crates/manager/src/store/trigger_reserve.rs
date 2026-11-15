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
    /// `waiting_approval` when the environment gate already stopped the job;
    /// otherwise the concurrency decision is re-computed inside this
    /// transaction (the caller's pre-check is only a fast path).
    pub status: &'a str,
    /// Concurrency inputs for the authoritative decision inside the write
    /// transaction. Ignored when `status` is `waiting_approval`.
    pub concurrency_group: Option<&'a str>,
    pub concurrency_mode: &'a str,
    pub concurrency_max_queue: i64,
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
/// The concurrency decision is recomputed here (pipeline row lock + counts in
/// the same transaction) so two concurrent triggers cannot both pass the
/// caller's earlier check.
///
/// Returns the allocated `run_number`.
pub async fn reserve_job(pool: &DbPool, job: &ReservedJob<'_>) -> Result<i64, AppError> {
    let mut tx = pool
        .begin_immediate()
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;
    // Serialise with `disable`: on Postgres the read below takes no lock of
    // its own, so this row lock is what closes the check-to-insert race.
    crate::store::locks::lock_pipeline_tx(&mut tx, job.pipeline_id).await?;

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

    // Authoritative concurrency decision inside this transaction. The caller
    // resolved it once for a fast rejection, but that check ran in a different
    // transaction — two simultaneous triggers could both pass it. Re-running
    // it here (same pipeline row lock + counts) is what makes the decision
    // atomic with the insert on both backends.
    let status: &str = if job.status == "waiting_approval" {
        job.status
    } else {
        crate::store::pipelines::resolve_concurrency_status_in_tx(
            &mut tx,
            job.pipeline_id,
            job.concurrency_group,
            job.concurrency_mode,
            job.concurrency_max_queue,
        )
        .await?
    };

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
            status.into(),
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
