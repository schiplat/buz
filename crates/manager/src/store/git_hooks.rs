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

/// One hook of a pipeline, list-page shape (no secret material).
#[derive(Debug, Clone)]
pub struct GitHookRow {
    pub id: String,
    pub label: String,
    pub provider: String,
    pub hook_key: String,
    pub branch_filter: String,
    pub enabled: bool,
    pub environment: String,
    pub event_filter: String,
}

/// All hooks of one pipeline, creation-ordered.
pub async fn list_for_pipeline(
    pool: &SqlitePool,
    pipeline_id: &str,
) -> Result<Vec<GitHookRow>, AppError> {
    let rows = sqlx::query(
        "SELECT id, label, provider, hook_key, branch_filter, enabled, environment, event_filter
         FROM pipeline_git_hooks WHERE pipeline_id = ?
         ORDER BY created_at ASC",
    )
    .bind(pipeline_id)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| GitHookRow {
            id: r.get(0),
            label: r.get(1),
            provider: r.get(2),
            hook_key: r.get(3),
            branch_filter: r.get(4),
            enabled: r.get::<i64, _>(5) != 0,
            environment: r.get(6),
            event_filter: r.get(7),
        })
        .collect())
}

/// Everything needed to insert one hook. The caller generates `id`,
/// `hook_key` and the plaintext secret; only the ciphertext is stored.
#[derive(Debug, Clone)]
pub struct GitHookInsert {
    pub id: String,
    pub pipeline_id: String,
    pub label: String,
    pub provider: String,
    pub hook_key: String,
    pub secret_ciphertext: String,
    pub branch_filter: String,
    pub environment: String,
    pub enabled: bool,
    pub event_filter: String,
}

/// Insert one hook row.
pub async fn insert(pool: &SqlitePool, h: &GitHookInsert) -> Result<(), AppError> {
    sqlx::query(
        "INSERT INTO pipeline_git_hooks
         (id, pipeline_id, label, provider, hook_key, secret_ciphertext, branch_filter, environment, enabled, event_filter)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&h.id)
    .bind(&h.pipeline_id)
    .bind(&h.label)
    .bind(&h.provider)
    .bind(&h.hook_key)
    .bind(&h.secret_ciphertext)
    .bind(&h.branch_filter)
    .bind(&h.environment)
    .bind(if h.enabled { 1 } else { 0 })
    .bind(&h.event_filter)
    .execute(pool)
    .await?;
    Ok(())
}

/// Mutable-settings row facts plus the immutable identity (`provider`,
/// `hook_key`) — the update path's pre-read. `None` = no such hook on
/// that pipeline.
#[derive(Debug, Clone)]
pub struct GitHookForUpdate {
    pub label: String,
    pub branch_filter: String,
    pub event_filter: String,
    pub environment: String,
    pub enabled: bool,
    pub provider: String,
    pub hook_key: String,
}

pub async fn find_for_update(
    pool: &SqlitePool,
    hook_id: &str,
    pipeline_id: &str,
) -> Result<Option<GitHookForUpdate>, AppError> {
    let row = sqlx::query(
        "SELECT label, branch_filter, event_filter, environment, enabled, provider, hook_key
         FROM pipeline_git_hooks WHERE id = ? AND pipeline_id = ?",
    )
    .bind(hook_id)
    .bind(pipeline_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| GitHookForUpdate {
        label: r.get(0),
        branch_filter: r.get(1),
        event_filter: r.get(2),
        environment: r.get(3),
        enabled: r.get::<i64, _>(4) != 0,
        provider: r.get(5),
        hook_key: r.get(6),
    }))
}

/// Replace a hook's mutable settings. Provider, hook key and secret are
/// immutable here — changing those is delete + recreate.
#[allow(clippy::too_many_arguments)]
pub async fn update_hook(
    pool: &SqlitePool,
    pipeline_id: &str,
    hook_id: &str,
    label: &str,
    branch_filter: &str,
    event_filter: &str,
    environment: &str,
    enabled: bool,
) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE pipeline_git_hooks SET label = ?, branch_filter = ?, event_filter = ?,
         environment = ?, enabled = ?, updated_at = datetime('now')
         WHERE id = ? AND pipeline_id = ?",
    )
    .bind(label)
    .bind(branch_filter)
    .bind(event_filter)
    .bind(environment)
    .bind(if enabled { 1 } else { 0 })
    .bind(hook_id)
    .bind(pipeline_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// The hook's identity + settings — the delete path's pre-read (audit
/// before-snapshot and display name come from this). `None` = no such hook.
#[derive(Debug, Clone)]
pub struct GitHookDeleteFacts {
    pub label: String,
    pub provider: String,
    pub hook_key: String,
    pub branch_filter: String,
    pub environment: String,
    pub enabled: bool,
}

pub async fn delete_facts(
    pool: &SqlitePool,
    hook_id: &str,
    pipeline_id: &str,
) -> Result<Option<GitHookDeleteFacts>, AppError> {
    let row = sqlx::query(
        "SELECT label, provider, hook_key, branch_filter, environment, enabled FROM pipeline_git_hooks
         WHERE id = ? AND pipeline_id = ?",
    )
    .bind(hook_id)
    .bind(pipeline_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| GitHookDeleteFacts {
        label: r.get(0),
        provider: r.get(1),
        hook_key: r.get(2),
        branch_filter: r.get(3),
        environment: r.get(4),
        enabled: r.get::<i64, _>(5) != 0,
    }))
}

/// Delete one hook; returns `false` when nothing matched (caller surfaces
/// `NotFound`).
pub async fn delete_hook(
    pool: &SqlitePool,
    hook_id: &str,
    pipeline_id: &str,
) -> Result<bool, AppError> {
    let res = sqlx::query("DELETE FROM pipeline_git_hooks WHERE id = ? AND pipeline_id = ?")
        .bind(hook_id)
        .bind(pipeline_id)
        .execute(pool)
        .await?;
    Ok(res.rows_affected() > 0)
}
