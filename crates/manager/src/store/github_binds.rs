//! Secret / variable -> GitHub repo bindings.
//!
//! These are the join rows behind GitHub Actions sync. Ciphertext handling for
//! the override value stays in the handler: the store takes an already
//! encrypted value (or `None` for "inherit"), and never returns secret override
//! plaintext.
//!
//! `tests/store_sql_validation_test.rs` compiles every literal here against the
//! migrated schema, so a wrong column name fails the suite instead of the
//! endpoint.

use sqlx::{Row, SqlitePool};

use shared::error::AppError;

/// One bind row joined with its repo and connection, shaped for the API.
///
/// `has_override` is what the UI shows; for secrets only the ciphertext's
/// presence is ever read, for variables the plaintext override.
pub struct BindRow {
    pub id: String,
    pub repo_id: String,
    pub connection_name: String,
    pub owner: String,
    pub repo: String,
    pub last_pushed_at: Option<String>,
    pub last_status: String,
    pub last_error: String,
    pub last_op: String,
    pub has_override: bool,
    pub override_value: Option<String>,
}

/// One bind row plus the repo/connection facts needed to push it.
pub struct BindTarget {
    pub id: String,
    pub owner: String,
    pub repo: String,
    pub api_base_url: String,
    pub token_ciphertext: String,
    pub connection_name: String,
    pub secret_name: String,
    pub override_ciphertext: Option<String>,
}

/// Every secret bind for one secret, secret override plaintext never selected.
pub async fn list_secret_binds(
    pool: &SqlitePool,
    secret_id: &str,
) -> Result<Vec<BindRow>, AppError> {
    let rows = sqlx::query(
        "SELECT b.id, b.repo_id, c.name, r.owner, r.repo,
                b.last_pushed_at, b.last_status, b.last_error, b.last_op, b.override_value
         FROM secret_github_binds b
         JOIN github_repos r ON r.id = b.repo_id
         JOIN github_connections c ON c.id = r.connection_id
         WHERE b.secret_id = ?
         ORDER BY r.owner ASC, r.repo ASC",
    )
    .bind(secret_id)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|r| BindRow {
            id: r.get(0),
            repo_id: r.get(1),
            connection_name: r.get(2),
            owner: r.get(3),
            repo: r.get(4),
            last_pushed_at: r.get(5),
            last_status: r.get(6),
            last_error: r.get(7),
            last_op: r.get(8),
            has_override: r.get::<Option<String>, _>(9).is_some(),
            override_value: None,
        })
        .collect())
}

/// Owner/repo plus connection name for a repo id, used when binding.
pub async fn repo_identity(
    pool: &SqlitePool,
    repo_id: &str,
) -> Result<Option<(String, String, String)>, AppError> {
    let row = sqlx::query(
        "SELECT r.owner, r.repo, c.name FROM github_repos r
         JOIN github_connections c ON c.id = r.connection_id
         WHERE r.id = ?",
    )
    .bind(repo_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| (r.get(0), r.get(1), r.get(2))))
}

/// True when this secret is already bound to the repo.
pub async fn secret_bind_exists(
    pool: &SqlitePool,
    secret_id: &str,
    repo_id: &str,
) -> Result<bool, AppError> {
    let found: Option<String> = sqlx::query_scalar(
        "SELECT id FROM secret_github_binds WHERE secret_id = ? AND repo_id = ?",
    )
    .bind(secret_id)
    .bind(repo_id)
    .fetch_optional(pool)
    .await?;
    Ok(found.is_some())
}

/// Create a secret bind. `override_ciphertext` is already encrypted by the caller.
pub async fn insert_secret_bind(
    pool: &SqlitePool,
    id: &str,
    secret_id: &str,
    repo_id: &str,
    override_ciphertext: Option<&str>,
) -> Result<(), AppError> {
    sqlx::query(
        "INSERT INTO secret_github_binds (id, secret_id, repo_id, last_op, override_value) VALUES (?, ?, ?, 'bind', ?)",
    )
    .bind(id)
    .bind(secret_id)
    .bind(repo_id)
    .bind(override_ciphertext)
    .execute(pool)
    .await?;
    Ok(())
}

/// Replace the stored override ciphertext (`None` clears it, inheriting the default).
pub async fn set_secret_bind_override(
    pool: &SqlitePool,
    bind_id: &str,
    secret_id: &str,
    override_ciphertext: Option<&str>,
) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE secret_github_binds SET override_value = ?, updated_at = datetime('now') \
         WHERE id = ? AND secret_id = ?",
    )
    .bind(override_ciphertext)
    .bind(bind_id)
    .bind(secret_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// After a bind override is patched, mark that bind `stale` if it was previously
/// `ok` (the effective push value changed). Other states are left alone.
pub async fn mark_secret_bind_stale_if_ok(
    pool: &SqlitePool,
    bind_id: &str,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query(
        "UPDATE secret_github_binds
         SET last_status = 'stale', last_error = '', updated_at = datetime('now')
         WHERE id = ? AND last_status = 'ok'",
    )
    .bind(bind_id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}

/// Same for variable binds: mark every `ok` bind on the variable stale.
pub async fn mark_variable_bind_stale_if_ok(
    pool: &SqlitePool,
    bind_id: &str,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query(
        "UPDATE variable_github_binds
         SET last_status = 'stale', last_error = '', updated_at = datetime('now')
         WHERE id = ? AND last_status = 'ok'",
    )
    .bind(bind_id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}
