//! Git hook lookups for the signed webhook path.

use sqlx::{Row, SqlitePool};

use shared::error::AppError;

/// A `pipeline_git_hooks` row joined with its pipeline's identity state —
/// everything the webhook handler needs to verify and dispatch.
#[derive(Debug, Clone)]
pub struct GitHookAndPipeline {
    pub pipeline_id: String,
    pub secret_ciphertext: String,
    pub branch_filter: String,
    pub enabled: bool,
    pub provider: String,
    pub pipeline_name: String,
    pub pipeline_active: bool,
    pub environment: String,
    pub event_filter: String,
}

/// Load the hook registered for `hook_key`, joined with its pipeline.
pub async fn find_by_hook_key(
    pool: &SqlitePool,
    hook_key: &str,
) -> Result<GitHookAndPipeline, AppError> {
    let row = sqlx::query(
        "SELECT h.pipeline_id, h.secret_ciphertext, h.branch_filter, h.enabled, h.provider, p.name, p.is_active, h.environment, h.event_filter
         FROM pipeline_git_hooks h
         JOIN pipelines p ON p.id = h.pipeline_id
         WHERE h.hook_key = ?",
    )
    .bind(hook_key)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| AppError::NotFound(format!("no git hook for key '{hook_key}'")))?;

    Ok(GitHookAndPipeline {
        pipeline_id: row.get(0),
        secret_ciphertext: row.get(1),
        branch_filter: row.get(2),
        enabled: row.get::<i64, _>(3) != 0,
        provider: row.get(4),
        pipeline_name: row.get(5),
        pipeline_active: row.get::<i64, _>(6) != 0,
        environment: row.get(7),
        event_filter: row.get(8),
    })
}
