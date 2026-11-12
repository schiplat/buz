//! Git hook lookups for the signed webhook path.
//!
//! On the dual-backend dispatcher (`store::exec`): statements travel as plain
//! SQLite-dialect strings with backend-neutral binds; rows decode by column
//! name.

use crate::store::exec::{self, DbPool, SqlValue};

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
    pool: &DbPool,
    hook_key: &str,
) -> Result<GitHookAndPipeline, AppError> {
    let row = exec::fetch_optional(
        pool,
        "SELECT h.pipeline_id, h.secret_ciphertext, h.branch_filter, h.enabled, h.provider, p.name, p.is_active, h.environment, h.event_filter
         FROM pipeline_git_hooks h
         JOIN pipelines p ON p.id = h.pipeline_id
         WHERE h.hook_key = ?",
        &[hook_key.into()],
    )
    .await?
    .ok_or_else(|| AppError::NotFound(format!("no git hook for key '{hook_key}'")))?;

    Ok(GitHookAndPipeline {
        pipeline_id: row.text_of("pipeline_id"),
        secret_ciphertext: row.text_of("secret_ciphertext"),
        branch_filter: row.text_of("branch_filter"),
        enabled: row.i64_of("enabled") != 0,
        provider: row.text_of("provider"),
        pipeline_name: row.text_of("name"),
        pipeline_active: row.i64_of("is_active") != 0,
        environment: row.text_of("environment"),
        event_filter: row.text_of("event_filter"),
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
    pool: &DbPool,
    pipeline_id: &str,
) -> Result<Vec<GitHookRow>, AppError> {
    let rows = exec::fetch_all(
        pool,
        "SELECT id, label, provider, hook_key, branch_filter, enabled, environment, event_filter
         FROM pipeline_git_hooks WHERE pipeline_id = ?
         ORDER BY created_at ASC",
        &[pipeline_id.into()],
    )
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| GitHookRow {
            id: r.text_of("id"),
            label: r.text_of("label"),
            provider: r.text_of("provider"),
            hook_key: r.text_of("hook_key"),
            branch_filter: r.text_of("branch_filter"),
            enabled: r.i64_of("enabled") != 0,
            environment: r.text_of("environment"),
            event_filter: r.text_of("event_filter"),
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
pub async fn insert(pool: &DbPool, h: &GitHookInsert) -> Result<(), AppError> {
    exec::execute(
        pool,
        "INSERT INTO pipeline_git_hooks
         (id, pipeline_id, label, provider, hook_key, secret_ciphertext, branch_filter, environment, enabled, event_filter)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        &[
            h.id.as_str().into(),
            h.pipeline_id.as_str().into(),
            h.label.as_str().into(),
            h.provider.as_str().into(),
            h.hook_key.as_str().into(),
            h.secret_ciphertext.as_str().into(),
            h.branch_filter.as_str().into(),
            h.environment.as_str().into(),
            SqlValue::Int(if h.enabled { 1 } else { 0 }),
            h.event_filter.as_str().into(),
        ],
    )
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
    pool: &DbPool,
    hook_id: &str,
    pipeline_id: &str,
) -> Result<Option<GitHookForUpdate>, AppError> {
    let row = exec::fetch_optional(
        pool,
        "SELECT label, branch_filter, event_filter, environment, enabled, provider, hook_key
         FROM pipeline_git_hooks WHERE id = ? AND pipeline_id = ?",
        &[hook_id.into(), pipeline_id.into()],
    )
    .await?;
    Ok(row.map(|r| GitHookForUpdate {
        label: r.text_of("label"),
        branch_filter: r.text_of("branch_filter"),
        event_filter: r.text_of("event_filter"),
        environment: r.text_of("environment"),
        enabled: r.i64_of("enabled") != 0,
        provider: r.text_of("provider"),
        hook_key: r.text_of("hook_key"),
    }))
}

/// Replace a hook's mutable settings. Provider, hook key and secret are
/// immutable here — changing those is delete + recreate.
#[allow(clippy::too_many_arguments)]
pub async fn update_hook(
    pool: &DbPool,
    pipeline_id: &str,
    hook_id: &str,
    label: &str,
    branch_filter: &str,
    event_filter: &str,
    environment: &str,
    enabled: bool,
) -> Result<(), AppError> {
    exec::execute(
        pool,
        "UPDATE pipeline_git_hooks SET label = ?, branch_filter = ?, event_filter = ?,
         environment = ?, enabled = ?, updated_at = datetime('now')
         WHERE id = ? AND pipeline_id = ?",
        &[
            label.into(),
            branch_filter.into(),
            event_filter.into(),
            environment.into(),
            SqlValue::Int(if enabled { 1 } else { 0 }),
            hook_id.into(),
            pipeline_id.into(),
        ],
    )
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
    pool: &DbPool,
    hook_id: &str,
    pipeline_id: &str,
) -> Result<Option<GitHookDeleteFacts>, AppError> {
    let row = exec::fetch_optional(
        pool,
        "SELECT label, provider, hook_key, branch_filter, environment, enabled FROM pipeline_git_hooks
         WHERE id = ? AND pipeline_id = ?",
        &[hook_id.into(), pipeline_id.into()],
    )
    .await?;
    Ok(row.map(|r| GitHookDeleteFacts {
        label: r.text_of("label"),
        provider: r.text_of("provider"),
        hook_key: r.text_of("hook_key"),
        branch_filter: r.text_of("branch_filter"),
        environment: r.text_of("environment"),
        enabled: r.i64_of("enabled") != 0,
    }))
}

/// Delete one hook; returns `false` when nothing matched (caller surfaces
/// `NotFound`).
pub async fn delete_hook(
    pool: &DbPool,
    hook_id: &str,
    pipeline_id: &str,
) -> Result<bool, AppError> {
    let n = exec::execute(
        pool,
        "DELETE FROM pipeline_git_hooks WHERE id = ? AND pipeline_id = ?",
        &[hook_id.into(), pipeline_id.into()],
    )
    .await?;
    Ok(n > 0)
}
