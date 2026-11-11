//! Job log chunks: the runner upload path (including the runner↔job
//! binding check) and the three read windows — cursor-bounded tail,
//! forward page, and the legacy full read.
//!
//! Rows are append-only per `(step_order, chunk_index)`, so every window
//! is stable while a job runs; the tail window reads `ORDER BY ... DESC`
//! and returns rows re-ordered ascending, matching the full read.

use sqlx::{Row, SqlitePool};

use shared::error::AppError;
use shared::protocol::LogLevel;
use uuid::Uuid;

/// `(pipeline_id, debug_unredact)` for one job — the facts that gate log
/// viewing: team scope comes from the pipeline, debug (unredacted) logs are
/// platform-admin only. `None` = no such job.
pub async fn job_view_facts(
    pool: &SqlitePool,
    job_id: &str,
) -> Result<Option<(String, i64)>, AppError> {
    let row = sqlx::query("SELECT pipeline_id, debug_unredact FROM jobs WHERE id = ?")
        .bind(job_id)
        .fetch_optional(pool)
        .await?;
    Ok(row.map(|r| (r.get(0), r.get(1))))
}

/// Runner the job was dispatched to — the upload path accepts chunks only
/// from that runner. `None` = no such job.
pub async fn job_runner_id(pool: &SqlitePool, job_id: &str) -> Result<Option<String>, AppError> {
    let row = sqlx::query("SELECT runner_id FROM jobs WHERE id = ?")
        .bind(job_id)
        .fetch_optional(pool)
        .await?;
    Ok(row.map(|r| r.get(0)))
}

/// Append one log chunk. `INSERT OR REPLACE` keyed by the per-job unique
/// `(job_id, step_order, chunk_index)` — a runner retry of the same chunk
/// overwrites instead of duplicating.
pub async fn insert_chunk(
    pool: &SqlitePool,
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

fn log_values(rows: Vec<sqlx::sqlite::SqliteRow>) -> Vec<serde_json::Value> {
    let logs: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|r| {
            serde_json::json!({
                "chunk_index": r.get::<i64, _>(0),
                "step_order": r.get::<i64, _>(1),
                "content": r.get::<String, _>(2),
                "level": LogLevel::parse(r.get::<Option<String>, _>(3).as_deref()),
                "created_at": r.get::<Option<String>, _>(4),
            })
        })
        .collect();
    logs
}

/// Cursor-bounded tail read: the last `window` rows strictly before
/// `(before_step, before_chunk)`, returned in ascending order.
pub async fn read_windowed(
    pool: &SqlitePool,
    job_id: &str,
    before_step: Option<i64>,
    before_chunk: Option<i64>,
    window: i64,
) -> Result<Vec<serde_json::Value>, AppError> {
    let mut rows = sqlx::query(
        "SELECT chunk_index, step_order, log_content, level, created_at
             FROM job_logs WHERE job_id = ?
             AND (? IS NULL OR step_order < ? OR (step_order = ? AND chunk_index < ?))
             ORDER BY step_order DESC, chunk_index DESC
             LIMIT ?",
    )
    .bind(job_id)
    .bind(before_step)
    .bind(before_step.unwrap_or(0))
    .bind(before_step.unwrap_or(0))
    .bind(before_chunk.unwrap_or(0))
    .bind(window)
    .fetch_all(pool)
    .await?;
    rows.reverse();
    Ok(log_values(rows))
}

/// Forward page: `limit` rows from the front, skipping `offset`.
pub async fn read_page(
    pool: &SqlitePool,
    job_id: &str,
    limit: i64,
    offset: i64,
) -> Result<Vec<serde_json::Value>, AppError> {
    let rows = sqlx::query(
        "SELECT chunk_index, step_order, log_content, level, created_at
             FROM job_logs WHERE job_id = ?
             ORDER BY step_order, chunk_index
             LIMIT ? OFFSET ?",
    )
    .bind(job_id)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await?;
    Ok(log_values(rows))
}

/// Full read (legacy shape — the dashboard's job view relies on it).
pub async fn read_all(pool: &SqlitePool, job_id: &str) -> Result<Vec<serde_json::Value>, AppError> {
    let rows = sqlx::query(
        "SELECT chunk_index, step_order, log_content, level, created_at
             FROM job_logs WHERE job_id = ?
             ORDER BY step_order, chunk_index",
    )
    .bind(job_id)
    .fetch_all(pool)
    .await?;
    Ok(log_values(rows))
}
