//! Job log chunks: the runner upload path (including the runner↔job
//! binding check) and the three read windows — cursor-bounded tail,
//! forward page, and the legacy full read.
//!
//! Rows are append-only per `(step_order, chunk_index)`, so every window
//! is stable while a job runs; the tail window reads `ORDER BY ... DESC`
//! and returns rows re-ordered ascending, matching the full read.
//!
//! Migration status: the read paths run on the dual-backend dispatcher
//! (`store::exec`). `insert_chunk` stays on `SqlitePool` — its `INSERT OR
//! REPLACE` needs the per-site PG upsert decision (ON CONFLICT DO UPDATE
//! with a named conflict target), which the statement rewriter deliberately
//! does not guess (see `store::dialect`'s known-gaps note).

use crate::store::exec::{self, DbPool};

use shared::error::AppError;
use shared::protocol::LogLevel;
use uuid::Uuid;

/// `(pipeline_id, debug_unredact)` for one job — the facts that gate log
/// viewing: team scope comes from the pipeline, debug (unredacted) logs are
/// platform-admin only. `None` = no such job.
pub async fn job_view_facts(
    pool: &DbPool,
    job_id: &str,
) -> Result<Option<(String, i64)>, AppError> {
    let row = exec::fetch_optional(
        pool,
        "SELECT pipeline_id, debug_unredact FROM jobs WHERE id = ?",
        &[job_id.into()],
    )
    .await?;
    Ok(row.map(|r| (r.text_of("pipeline_id"), r.i64_of("debug_unredact"))))
}

/// Runner the job was dispatched to — the upload path accepts chunks only
/// from that runner. `None` = no such job; `Some(None)` = the job exists but
/// has not been dispatched (its `runner_id` is NULL), which must be refused.
pub async fn job_runner_id(
    pool: &DbPool,
    job_id: &str,
) -> Result<Option<Option<String>>, AppError> {
    let row = exec::fetch_optional(
        pool,
        "SELECT runner_id FROM jobs WHERE id = ?",
        &[job_id.into()],
    )
    .await?;
    // Two Nones with different meanings: outer = no such job, inner = the
    // job exists but has not been dispatched. See the doc comment above.
    Ok(row.map(|r| r.opt_text_of("runner_id")))
}

/// Append one log chunk. `INSERT OR REPLACE` keyed by the per-job unique
/// `(job_id, step_order, chunk_index)` — a runner retry of the same chunk
/// overwrites instead of duplicating.
pub async fn insert_chunk(
    pool: &sqlx::SqlitePool,
    job_id: &str,
    chunk_index: i32,
    step_order: i32,
    content: &str,
    level: Option<&str>,
) -> Result<(), AppError> {
    sqlx::query(
        "INSERT OR REPLACE INTO job_logs (id, job_id, chunk_index, step_order, log_content, is_masked, level)
         VALUES (?, ?, ?, ?, ?, 1, ?)",
    )
    .bind(Uuid::new_v4().to_string())
    .bind(job_id)
    .bind(chunk_index)
    .bind(step_order)
    .bind(content)
    .bind(level)
    .execute(pool)
    .await?;
    Ok(())
}

fn log_values(rows: Vec<crate::store::exec::GenericRow>) -> Vec<serde_json::Value> {
    rows.into_iter()
        .map(|r| {
            serde_json::json!({
                "chunk_index": r.i64_of("chunk_index"),
                "step_order": r.i64_of("step_order"),
                "content": r.text_of("log_content"),
                "level": LogLevel::parse(r.opt_text_of("level").as_deref()),
                "created_at": r.opt_text_of("created_at"),
            })
        })
        .collect()
}

/// Cursor-bounded tail read: the last `window` rows strictly before
/// `(before_step, before_chunk)`, returned in ascending order.
pub async fn read_windowed(
    pool: &DbPool,
    job_id: &str,
    before_step: Option<i64>,
    before_chunk: Option<i64>,
    window: i64,
) -> Result<Vec<serde_json::Value>, AppError> {
    let mut rows = exec::fetch_all(
        pool,
        "SELECT chunk_index, step_order, log_content, level, created_at
             FROM job_logs WHERE job_id = ?
             AND (? IS NULL OR step_order < ? OR (step_order = ? AND chunk_index < ?))
             ORDER BY step_order DESC, chunk_index DESC
             LIMIT ?",
        &[
            job_id.into(),
            before_step.into(),
            before_step.unwrap_or(0).into(),
            before_step.unwrap_or(0).into(),
            before_chunk.unwrap_or(0).into(),
            window.into(),
        ],
    )
    .await?;
    rows.reverse();
    Ok(log_values(rows))
}

/// Forward page: `limit` rows from the front, skipping `offset`.
pub async fn read_page(
    pool: &DbPool,
    job_id: &str,
    limit: i64,
    offset: i64,
) -> Result<Vec<serde_json::Value>, AppError> {
    let rows = exec::fetch_all(
        pool,
        "SELECT chunk_index, step_order, log_content, level, created_at
             FROM job_logs WHERE job_id = ?
             ORDER BY step_order, chunk_index
             LIMIT ? OFFSET ?",
        &[job_id.into(), limit.into(), offset.into()],
    )
    .await?;
    Ok(log_values(rows))
}

/// Full read (legacy shape — the dashboard's job view relies on it).
pub async fn read_all(pool: &DbPool, job_id: &str) -> Result<Vec<serde_json::Value>, AppError> {
    let rows = exec::fetch_all(
        pool,
        "SELECT chunk_index, step_order, log_content, level, created_at
             FROM job_logs WHERE job_id = ?
             ORDER BY step_order, chunk_index",
        &[job_id.into()],
    )
    .await?;
    Ok(log_values(rows))
}
