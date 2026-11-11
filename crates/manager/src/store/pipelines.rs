//! Pipeline row lookups shared across handlers.

use sqlx::{Row, SqlitePool};

use shared::error::AppError;

/// `(source, yaml_path, name)` for one pipeline — the definition-page header
/// facts (origin basename + storage source).
pub async fn pipeline_source_row(
    pool: &SqlitePool,
    id: &str,
) -> Result<(String, Option<String>, String), AppError> {
    let row = sqlx::query("SELECT source, yaml_path, name FROM pipelines WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| AppError::NotFound("pipeline not found".into()))?;
    Ok((row.get(0), row.get(1), row.get(2)))
}

/// Pipeline concurrency decision inside the caller's write transaction:
/// `pending` (run now) or `queued`, or a conflict when the mode forbids it.
///
/// Guarded writes only — the caller owns the `BEGIN IMMEDIATE` scope and any
/// post-commit fan-out.
pub async fn resolve_concurrency_status_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    pipeline_id: &str,
    concurrency_group: Option<&str>,
    concurrency_mode: &str,
    concurrency_max_queue: i64,
) -> Result<&'static str, AppError> {
    use sqlx::Row;

    let Some(group) = concurrency_group.filter(|g| !g.is_empty()) else {
        return Ok("pending");
    };

    let mode = if concurrency_mode.is_empty() {
        "discard-new"
    } else {
        concurrency_mode
    };

    match mode {
        "discard-new" => {
            let running = sqlx::query(
                "SELECT COUNT(*) as cnt FROM jobs
                 WHERE pipeline_id = ? AND status IN ('pending', 'running', 'queued')",
            )
            .bind(pipeline_id)
            .fetch_one(&mut **tx)
            .await?;
            let count: i64 = running.get(0);
            if count > 0 {
                return Err(AppError::Conflict(format!(
                    "concurrency group '{group}' has {count} active/queued job(s)"
                )));
            }
            Ok("pending")
        }

        "discard-old" => {
            sqlx::query(
                "UPDATE jobs SET status = 'cancelled', updated_at = datetime('now')
                 WHERE pipeline_id = ? AND status = 'queued'",
            )
            .bind(pipeline_id)
            .execute(&mut **tx)
            .await?;
            Ok("pending")
        }

        "queue" => {
            let queued_count: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM jobs
                 WHERE pipeline_id = ? AND status = 'queued'",
            )
            .bind(pipeline_id)
            .fetch_one(&mut **tx)
            .await?;

            if queued_count >= concurrency_max_queue {
                return Err(AppError::Conflict(format!(
                    "queue full: {queued_count} jobs already queued (max {concurrency_max_queue})"
                )));
            }

            let running_count: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM jobs
                 WHERE pipeline_id = ? AND status IN ('pending', 'running')",
            )
            .bind(pipeline_id)
            .fetch_one(&mut **tx)
            .await?;

            if running_count == 0 {
                Ok("pending")
            } else {
                Ok("queued")
            }
        }

        _ => Err(AppError::ValidationFailed(format!(
            "unknown concurrency mode '{mode}'"
        ))),
    }
}

/// Pipeline concurrency decision, owning its transaction — the read-only-gate
/// variant used by re-run and trigger when no other writes share the scope.
pub async fn resolve_concurrency_status(
    pool: &SqlitePool,
    pipeline_id: &str,
    concurrency_group: Option<&str>,
    concurrency_mode: &str,
    concurrency_max_queue: i64,
) -> Result<&'static str, AppError> {
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    let status = resolve_concurrency_status_in_tx(
        &mut tx,
        pipeline_id,
        concurrency_group,
        concurrency_mode,
        concurrency_max_queue,
    )
    .await?;
    tx.commit().await?;
    Ok(status)
}
