//! Runner rows: admin management operations and the bookkeeping writes the
//! poller performs on its own runner.
//!
//! The claim decision itself (candidate selection, slot gate, environment
//! gates, cancel_in_progress supersede) stays in `api/runners.rs`: it is
//! interleaved with a loop's control flow and moves in a separate step.

use serde_json::{json, Value};
use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use shared::error::AppError;

/// Why the last poll round skipped a pending job. Display-only, cleared on
/// claim. Logged only when the reason changes, so a job parked on one reason
/// does not repeat itself every poll round.
pub async fn record_dispatch_reason(pool: &SqlitePool, job_id: &str, reason: &str) {
    match sqlx::query(
        "UPDATE jobs SET dispatch_reason = ? WHERE id = ? AND status = 'pending'
         AND COALESCE(dispatch_reason, '') != ?",
    )
    .bind(reason)
    .bind(job_id)
    .bind(reason)
    .execute(pool)
    .await
    {
        Ok(done) if done.rows_affected() > 0 => {
            tracing::info!(job = %job_id, reason, "job not dispatched");
        }
        Ok(_) => {}
        Err(e) => {
            tracing::warn!(job = %job_id, error = %e, "failed to record dispatch reason");
        }
    }
}

/// Append one runner event row. Fire-and-forget: a failure is logged and
/// not propagated, because event history must never block an operator action.
pub async fn log_runner_event(pool: &SqlitePool, runner_id: &str, event_type: &str, message: &str) {
    let id = Uuid::new_v4().to_string();
    if let Err(e) = sqlx::query(
        "INSERT INTO runner_events (id, runner_id, event_type, message) VALUES (?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(runner_id)
    .bind(event_type)
    .bind(message)
    .execute(pool)
    .await
    {
        tracing::warn!("Failed to log runner event for {runner_id}: {e}");
    }
}

/// `(name, team_id, status)` snapshot taken before a row is deleted: the audit
/// record outlives the row and is the last description of it.
pub async fn runner_snapshot(
    pool: &SqlitePool,
    id: &str,
) -> Result<Option<(String, Option<String>, String)>, AppError> {
    let row = sqlx::query("SELECT name, team_id, status FROM runners WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await?;
    Ok(row.map(|r| (r.get(0), r.try_get(1).ok().flatten(), r.get(2))))
}

/// Delete one runner. False when no row matched.
///
/// `jobs.runner_id` carries a plain FK to `runners(id)` with no `ON DELETE`
/// action, so the referencing rows must be cleared or the delete fails.
pub async fn delete_runner(pool: &SqlitePool, id: &str) -> Result<bool, AppError> {
    sqlx::query("UPDATE jobs SET runner_id = NULL WHERE runner_id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    let result = sqlx::query("DELETE FROM runners WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() != 0)
}

/// Flip the freeze flag: `(name, was_frozen, now_frozen)`.
///
/// Read-modify-write in one `BEGIN IMMEDIATE`, so two concurrent toggles cannot
/// both read the same value and write the same result.
pub async fn toggle_freeze(pool: &SqlitePool, id: &str) -> Result<(String, bool, bool), AppError> {
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    let row = sqlx::query("SELECT name, is_frozen FROM runners WHERE id = ?")
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| AppError::NotFound("runner not found".into()))?;
    let name: String = row.get(0);
    let was_frozen = row.get::<i64, _>(1) != 0;
    let now_frozen = !was_frozen;
    sqlx::query("UPDATE runners SET is_frozen = ?, updated_at = datetime('now') WHERE id = ?")
        .bind(i64::from(now_frozen))
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    crate::services::poll_events::wake_all();
    Ok((name, was_frozen, now_frozen))
}

/// What a restart request needs to know about the runner.
pub struct RunnerRestartState {
    pub name: String,
    /// Ids of jobs still running on it (`ORDER BY created_at`).
    pub running_job_ids: Vec<String>,
}

pub async fn runner_restart_state(
    pool: &SqlitePool,
    id: &str,
) -> Result<RunnerRestartState, AppError> {
    let name: Option<String> = sqlx::query_scalar("SELECT name FROM runners WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await?;
    let running_job_ids: Vec<String> = sqlx::query_scalar(
        "SELECT id FROM jobs WHERE runner_id = ? AND status = 'running' ORDER BY created_at",
    )
    .bind(id)
    .fetch_all(pool)
    .await?;
    Ok(RunnerRestartState {
        name: name.ok_or_else(|| AppError::NotFound("runner not found".into()))?,
        running_job_ids,
    })
}

/// Set the restart flag served on the runner's next heartbeat.
pub async fn request_restart(pool: &SqlitePool, id: &str) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE runners SET restart_requested = 1, updated_at = datetime('now') WHERE id = ?",
    )
    .bind(id)
    .execute(pool)
    .await?;
    crate::services::poll_events::wake_all();
    Ok(())
}

/// Set the one-shot capability re-probe flag.
pub async fn request_capability_reprobe(pool: &SqlitePool, id: &str) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE runners SET capability_reprobe_requested = 1,
            capability_reprobe_requested_at = datetime('now'),
            capability_reprobe_served_at = NULL,
            updated_at = datetime('now') WHERE id = ?",
    )
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Replace the one-shot register token. Returns the runner name, or `None`
/// when the row does not exist received.
///
/// The caller passes an already-hashed token: plaintext is never stored.
pub async fn reissue_register_token(
    pool: &SqlitePool,
    id: &str,
    token_hash: &str,
) -> Result<Option<String>, AppError> {
    Ok(sqlx::query_scalar(
        "UPDATE runners SET register_token = ?, updated_at = datetime('now') WHERE id = ? RETURNING name",
    )
    .bind(token_hash)
    .bind(id)
    .fetch_optional(pool)
    .await?)
}

/// Runner name by id (`None` when absent).
pub async fn runner_name(pool: &SqlitePool, id: &str) -> Result<Option<String>, AppError> {
    Ok(sqlx::query_scalar("SELECT name FROM runners WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await?)
}

/// True when the runner row exists.
pub async fn exists(pool: &SqlitePool, id: &str) -> Result<bool, AppError> {
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runners WHERE id = ?")
        .bind(id)
        .fetch_one(pool)
        .await?;
    Ok(count != 0)
}

/// One page of a runner's event history plus the total count.
pub struct RunnerEventPage {
    pub events: Vec<Value>,
    pub total: i64,
}

pub async fn event_page(
    pool: &SqlitePool,
    runner_id: &str,
    limit: i64,
    offset: i64,
) -> Result<RunnerEventPage, AppError> {
    let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runner_events WHERE runner_id = ?")
        .bind(runner_id)
        .fetch_one(pool)
        .await?;
    let rows = sqlx::query(
        "SELECT id, event_type, message, created_at
         FROM runner_events
         WHERE runner_id = ?
         ORDER BY created_at DESC
         LIMIT ? OFFSET ?",
    )
    .bind(runner_id)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await?;
    let events = rows
        .into_iter()
        .map(|r| {
            json!({
                "id": r.get::<String, _>(0),
                "event_type": r.get::<String, _>(1),
                "message": r.get::<String, _>(2),
                "created_at": r.get::<Option<String>, _>(3),
            })
        })
        .collect();
    Ok(RunnerEventPage { events, total })
}
