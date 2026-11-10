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
