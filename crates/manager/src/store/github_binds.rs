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
//!
//! On the dual-backend dispatcher (`store::exec`): statements keep the SQLite
//! dialect and the PG arm rewrites at execution time; projections are fixed,
//! so rows decode by position.

use crate::store::exec::{self, DbPool, SqlValue};

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
pub async fn list_secret_binds(pool: &DbPool, secret_id: &str) -> Result<Vec<BindRow>, AppError> {
    let rows = exec::fetch_all(
        pool,
        "SELECT b.id, b.repo_id, c.name, r.owner, r.repo,
                b.last_pushed_at, b.last_status, b.last_error, b.last_op, b.override_value
         FROM secret_github_binds b
         JOIN github_repos r ON r.id = b.repo_id
         JOIN github_connections c ON c.id = r.connection_id
         WHERE b.secret_id = ?
         ORDER BY r.owner ASC, r.repo ASC",
        &[secret_id.into()],
    )
    .await?;

    Ok(rows
        .into_iter()
        .map(|r| BindRow {
            id: r.text(0),
            repo_id: r.text(1),
            connection_name: r.text(2),
            owner: r.text(3),
            repo: r.text(4),
            last_pushed_at: r.opt_text(5),
            last_status: r.text(6),
            last_error: r.text(7),
            last_op: r.text(8),
            has_override: r.opt_text(9).is_some(),
            override_value: None,
        })
        .collect())
}

/// Owner/repo plus connection name for a repo id, used when binding.
pub async fn repo_identity(
    pool: &DbPool,
    repo_id: &str,
) -> Result<Option<(String, String, String)>, AppError> {
    Ok(exec::fetch_optional(
        pool,
        "SELECT r.owner, r.repo, c.name FROM github_repos r
         JOIN github_connections c ON c.id = r.connection_id
         WHERE r.id = ?",
        &[repo_id.into()],
    )
    .await?
    .map(|r| (r.text(0), r.text(1), r.text(2))))
}

/// True when this secret is already bound to the repo.
pub async fn secret_bind_exists(
    pool: &DbPool,
    secret_id: &str,
    repo_id: &str,
) -> Result<bool, AppError> {
    let found = exec::fetch_optional(
        pool,
        "SELECT id FROM secret_github_binds WHERE secret_id = ? AND repo_id = ?",
        &[secret_id.into(), repo_id.into()],
    )
    .await?;
    Ok(found.is_some())
}

/// Create a secret bind. `override_ciphertext` is already encrypted by the caller.
pub async fn insert_secret_bind(
    pool: &DbPool,
    id: &str,
    secret_id: &str,
    repo_id: &str,
    override_ciphertext: Option<&str>,
) -> Result<(), AppError> {
    exec::execute(
        pool,
        "INSERT INTO secret_github_binds (id, secret_id, repo_id, last_op, override_value) VALUES (?, ?, ?, 'bind', ?)",
        &[id.into(), secret_id.into(), repo_id.into(), override_ciphertext.into()],
    )
    .await?;
    Ok(())
}

/// Replace the stored override ciphertext (`None` clears it, inheriting the default).
pub async fn set_secret_bind_override(
    pool: &DbPool,
    bind_id: &str,
    secret_id: &str,
    override_ciphertext: Option<&str>,
) -> Result<(), AppError> {
    exec::execute(
        pool,
        "UPDATE secret_github_binds SET override_value = ?, updated_at = datetime('now') \
         WHERE id = ? AND secret_id = ?",
        &[override_ciphertext.into(), bind_id.into(), secret_id.into()],
    )
    .await?;
    Ok(())
}

/// After a bind override is patched, mark that bind `stale` if it was previously
/// `ok` (the effective push value changed). Other states are left alone.
pub async fn mark_secret_bind_stale_if_ok(
    pool: &DbPool,
    bind_id: &str,
) -> Result<bool, sqlx::Error> {
    let affected = exec::execute(
        pool,
        "UPDATE secret_github_binds
         SET last_status = 'stale', last_error = '', updated_at = datetime('now')
         WHERE id = ? AND last_status = 'ok'",
        &[bind_id.into()],
    )
    .await?;
    Ok(affected > 0)
}

/// Same for variable binds: mark every `ok` bind on the variable stale.
pub async fn mark_variable_bind_stale_if_ok(
    pool: &DbPool,
    bind_id: &str,
) -> Result<bool, sqlx::Error> {
    let affected = exec::execute(
        pool,
        "UPDATE variable_github_binds
         SET last_status = 'stale', last_error = '', updated_at = datetime('now')
         WHERE id = ? AND last_status = 'ok'",
        &[bind_id.into()],
    )
    .await?;
    Ok(affected > 0)
}

/// Every variable bind for one variable. Variable overrides are plaintext, so
/// unlike the secret variant this returns the stored override value.
pub async fn list_variable_binds(pool: &DbPool, var_id: &str) -> Result<Vec<BindRow>, AppError> {
    let rows = exec::fetch_all(
        pool,
        "SELECT b.id, b.repo_id, c.name, r.owner, r.repo,
                b.last_pushed_at, b.last_status, b.last_error, b.last_op, b.override_value
         FROM variable_github_binds b
         JOIN github_repos r ON r.id = b.repo_id
         JOIN github_connections c ON c.id = r.connection_id
         WHERE b.var_id = ?
         ORDER BY r.owner ASC, r.repo ASC",
        &[var_id.into()],
    )
    .await?;

    Ok(rows
        .into_iter()
        .map(|r| {
            let override_value = r.opt_text(9);
            BindRow {
                id: r.text(0),
                repo_id: r.text(1),
                connection_name: r.text(2),
                owner: r.text(3),
                repo: r.text(4),
                last_pushed_at: r.opt_text(5),
                last_status: r.text(6),
                last_error: r.text(7),
                last_op: r.text(8),
                has_override: override_value.is_some(),
                override_value,
            }
        })
        .collect())
}

/// Create a variable bind. Variable overrides are plaintext.
pub async fn insert_variable_bind(
    pool: &DbPool,
    id: &str,
    var_id: &str,
    repo_id: &str,
    override_value: Option<&str>,
) -> Result<(), AppError> {
    exec::execute(
        pool,
        "INSERT INTO variable_github_binds (id, var_id, repo_id, last_op, override_value) VALUES (?, ?, ?, 'bind', ?)",
        &[id.into(), var_id.into(), repo_id.into(), override_value.into()],
    )
    .await?;
    Ok(())
}

/// True when this variable is already bound to the repo.
pub async fn variable_bind_exists(
    pool: &DbPool,
    var_id: &str,
    repo_id: &str,
) -> Result<bool, AppError> {
    let found = exec::fetch_optional(
        pool,
        "SELECT id FROM variable_github_binds WHERE var_id = ? AND repo_id = ?",
        &[var_id.into(), repo_id.into()],
    )
    .await?;
    Ok(found.is_some())
}

/// Replace a variable bind's plaintext override (`None` inherits the default).
pub async fn set_variable_bind_override(
    pool: &DbPool,
    bind_id: &str,
    var_id: &str,
    override_value: Option<&str>,
) -> Result<(), AppError> {
    exec::execute(
        pool,
        "UPDATE variable_github_binds SET override_value = ?, updated_at = datetime('now') \
         WHERE id = ? AND var_id = ?",
        &[override_value.into(), bind_id.into(), var_id.into()],
    )
    .await?;
    Ok(())
}

/// One bind row plus the connection facts needed by variable push / unbind.
pub struct VariableBindTarget {
    pub owner: String,
    pub repo: String,
    pub api_base_url: String,
    pub token_ciphertext: String,
    pub connection_name: String,
    pub override_value: Option<String>,
}

/// Load a variable bind by id, scoped to its variable (`None` = not bound).
pub async fn variable_bind_target(
    pool: &DbPool,
    bind_id: &str,
    var_id: &str,
) -> Result<Option<VariableBindTarget>, AppError> {
    Ok(exec::fetch_optional(
        pool,
        "SELECT b.id, r.owner, r.repo, c.api_base_url, c.token_ciphertext, c.name, b.override_value
         FROM variable_github_binds b
         JOIN github_repos r ON r.id = b.repo_id
         JOIN github_connections c ON c.id = r.connection_id
         WHERE b.id = ? AND b.var_id = ?",
        &[bind_id.into(), var_id.into()],
    )
    .await?
    .map(|r| VariableBindTarget {
        owner: r.text(1),
        repo: r.text(2),
        api_base_url: r.text(3),
        token_ciphertext: r.text(4),
        connection_name: r.text(5),
        override_value: r.opt_text(6),
    }))
}

/// Ids of a variable's binds (`bind_id` set = just that one, still scoped).
pub async fn variable_bind_ids(
    pool: &DbPool,
    var_id: &str,
    bind_id: Option<&str>,
) -> Result<Vec<String>, AppError> {
    match bind_id {
        Some(bid) if !bid.is_empty() => Ok(vec![bid.to_string()]),
        _ => Ok(exec::fetch_all(
            pool,
            "SELECT id FROM variable_github_binds WHERE var_id = ?",
            &[var_id.into()],
        )
        .await?
        .into_iter()
        .map(|r| r.text(0))
        .collect()),
    }
}

/// Record a successful push of a variable bind.
pub async fn mark_variable_bind_pushed_ok(pool: &DbPool, bind_id: &str) -> Result<(), AppError> {
    exec::execute(
        pool,
        "UPDATE variable_github_binds SET last_status = 'ok', last_error = '', \
         last_pushed_at = datetime('now'), last_op = 'push', updated_at = datetime('now') \
         WHERE id = ?",
        &[bind_id.into()],
    )
    .await?;
    Ok(())
}

/// Record a failed push of a variable bind.
pub async fn mark_variable_bind_push_failed(
    pool: &DbPool,
    bind_id: &str,
    error: &str,
) -> Result<(), AppError> {
    exec::execute(
        pool,
        "UPDATE variable_github_binds SET last_status = 'failed', last_error = ?, \
         last_op = 'push', updated_at = datetime('now') WHERE id = ?",
        &[error.into(), bind_id.into()],
    )
    .await?;
    Ok(())
}

/// Record a failed remote unbind, keeping the local row so the operator can retry.
pub async fn mark_variable_bind_unbind_failed(
    pool: &DbPool,
    bind_id: &str,
    var_id: &str,
    error: &str,
) -> Result<(), AppError> {
    exec::execute(
        pool,
        "UPDATE variable_github_binds SET last_op = 'unbind', last_status = 'failed', \
         last_error = ?, updated_at = datetime('now') WHERE id = ? AND var_id = ?",
        &[error.into(), bind_id.into(), var_id.into()],
    )
    .await?;
    Ok(())
}

/// Delete one variable bind; `false` when it did not exist.
pub async fn delete_variable_bind(
    pool: &DbPool,
    bind_id: &str,
    var_id: &str,
) -> Result<bool, AppError> {
    let affected = exec::execute(
        pool,
        "DELETE FROM variable_github_binds WHERE id = ? AND var_id = ?",
        &[bind_id.into(), var_id.into()],
    )
    .await?;
    Ok(affected != 0)
}

// ───────────────────── secret-side mirrors of the variable paths ─────────────────────

/// Ids of a secret's binds (`bind_id` set = just that one, still scoped).
pub async fn secret_bind_ids(
    pool: &DbPool,
    secret_id: &str,
    bind_id: Option<&str>,
) -> Result<Vec<String>, AppError> {
    match bind_id {
        Some(bid) if !bid.is_empty() => Ok(vec![bid.to_string()]),
        _ => Ok(exec::fetch_all(
            pool,
            "SELECT id FROM secret_github_binds WHERE secret_id = ?",
            &[secret_id.into()],
        )
        .await?
        .into_iter()
        .map(|r| r.text(0))
        .collect()),
    }
}

/// One bind row plus the connection facts needed by secret push / unbind.
pub struct SecretBindTarget {
    pub owner: String,
    pub repo: String,
    pub api_base_url: String,
    pub token_ciphertext: String,
    pub connection_name: String,
    pub override_ciphertext: Option<String>,
}

/// Load a secret bind by id, scoped to its secret (`None` = not bound).
pub async fn secret_bind_target(
    pool: &DbPool,
    bind_id: &str,
    secret_id: &str,
) -> Result<Option<SecretBindTarget>, AppError> {
    Ok(exec::fetch_optional(
        pool,
        "SELECT b.id, r.owner, r.repo, c.api_base_url, c.token_ciphertext, c.name, b.override_value
             FROM secret_github_binds b
             JOIN github_repos r ON r.id = b.repo_id
             JOIN github_connections c ON c.id = r.connection_id
             WHERE b.id = ? AND b.secret_id = ?",
        &[bind_id.into(), secret_id.into()],
    )
    .await?
    .map(|r| SecretBindTarget {
        owner: r.text(1),
        repo: r.text(2),
        api_base_url: r.text(3),
        token_ciphertext: r.text(4),
        connection_name: r.text(5),
        override_ciphertext: r.opt_text(6),
    }))
}

/// Record a successful push of a secret bind.
pub async fn mark_secret_bind_pushed_ok(pool: &DbPool, bind_id: &str) -> Result<(), AppError> {
    exec::execute(
        pool,
        "UPDATE secret_github_binds SET last_status = 'ok', last_error = '', last_pushed_at = datetime('now'), last_op = 'push', updated_at = datetime('now') WHERE id = ?",
        &[bind_id.into()],
    )
    .await?;
    Ok(())
}

/// Record a failed push of a secret bind.
pub async fn mark_secret_bind_push_failed(
    pool: &DbPool,
    bind_id: &str,
    error: &str,
) -> Result<(), AppError> {
    exec::execute(
        pool,
        "UPDATE secret_github_binds SET last_status = 'failed', last_error = ?, last_op = 'push', updated_at = datetime('now') WHERE id = ?",
        &[error.into(), bind_id.into()],
    )
    .await?;
    Ok(())
}

/// Record a failed remote unbind, keeping the local row so the operator can retry.
pub async fn mark_secret_bind_unbind_failed(
    pool: &DbPool,
    bind_id: &str,
    secret_id: &str,
    error: &str,
) -> Result<(), AppError> {
    exec::execute(
        pool,
        "UPDATE secret_github_binds SET last_op = 'unbind', last_status = 'failed', last_error = ?, updated_at = datetime('now') WHERE id = ? AND secret_id = ?",
        &[error.into(), bind_id.into(), secret_id.into()],
    )
    .await?;
    Ok(())
}

/// Delete one secret bind; `false` when it did not exist.
pub async fn delete_secret_bind(
    pool: &DbPool,
    bind_id: &str,
    secret_id: &str,
) -> Result<bool, AppError> {
    let affected = exec::execute(
        pool,
        "DELETE FROM secret_github_binds WHERE id = ? AND secret_id = ?",
        &[bind_id.into(), secret_id.into()],
    )
    .await?;
    Ok(affected != 0)
}

/// The bind + repo/connection facts the patch paths echo back
/// (9-column pre-read; the override itself is written separately).
pub struct BindPatchFacts {
    pub repo_id: String,
    pub connection_name: String,
    pub owner: String,
    pub repo: String,
    pub last_pushed_at: Option<String>,
    pub last_status: String,
    pub last_error: String,
    pub last_op: String,
}

fn patch_facts_row(r: exec::GenericRow) -> BindPatchFacts {
    BindPatchFacts {
        repo_id: r.text(1),
        connection_name: r.text(2),
        owner: r.text(3),
        repo: r.text(4),
        last_pushed_at: r.opt_text(5),
        last_status: r.text(6),
        last_error: r.text(7),
        last_op: r.text(8),
    }
}

/// Load one secret bind's patch facts, scoped to its secret
/// (`None` = no such bind).
pub async fn secret_bind_patch_facts(
    pool: &DbPool,
    bind_id: &str,
    secret_id: &str,
) -> Result<Option<BindPatchFacts>, AppError> {
    Ok(exec::fetch_optional(
        pool,
        "SELECT b.id, b.repo_id, c.name, r.owner, r.repo, b.last_pushed_at, b.last_status, b.last_error, b.last_op
         FROM secret_github_binds b
         JOIN github_repos r ON r.id = b.repo_id
         JOIN github_connections c ON c.id = r.connection_id
         WHERE b.id = ? AND b.secret_id = ?",
        &[bind_id.into(), secret_id.into()],
    )
    .await?
    .map(patch_facts_row))
}

/// Load one variable bind's patch facts, scoped to its variable
/// (`None` = no such bind).
pub async fn variable_bind_patch_facts(
    pool: &DbPool,
    bind_id: &str,
    var_id: &str,
) -> Result<Option<BindPatchFacts>, AppError> {
    Ok(exec::fetch_optional(
        pool,
        "SELECT b.id, b.repo_id, c.name, r.owner, r.repo, b.last_pushed_at, b.last_status, b.last_error, b.last_op
         FROM variable_github_binds b
         JOIN github_repos r ON r.id = b.repo_id
         JOIN github_connections c ON c.id = r.connection_id
         WHERE b.id = ? AND b.var_id = ?",
        &[bind_id.into(), var_id.into()],
    )
    .await?
    .map(patch_facts_row))
}

/// After the platform default secret value changes, mark inherit binds
/// (`override_value IS NULL`) that were successfully pushed as `stale`.
/// Custom-override binds are left alone. Returns how many rows were marked.
pub async fn mark_secret_binds_stale_on_default_change(
    pool: &DbPool,
    secret_id: &str,
) -> Result<u64, sqlx::Error> {
    exec::execute(
        pool,
        "UPDATE secret_github_binds
         SET last_status = 'stale', last_error = '', updated_at = datetime('now')
         WHERE secret_id = ? AND override_value IS NULL AND last_status = 'ok'",
        &[secret_id.into()],
    )
    .await
}

/// After the platform default variable value changes, mark inherit binds as `stale`.
pub async fn mark_variable_binds_stale_on_default_change(
    pool: &DbPool,
    var_id: &str,
) -> Result<u64, sqlx::Error> {
    exec::execute(
        pool,
        "UPDATE variable_github_binds
         SET last_status = 'stale', last_error = '', updated_at = datetime('now')
         WHERE var_id = ? AND override_value IS NULL AND last_status = 'ok'",
        &[var_id.into()],
    )
    .await
}

// ─────────────────────────── list-enrichment summaries ───────────────────────────

/// One bind summary row for the repos list (no values, metadata only).
pub struct RepoBindSummary {
    pub repo_id: String,
    pub id: String,
    pub name: String,
    pub last_status: String,
    pub last_op: String,
}

/// Secret binds for a batch of repos, secret-name-ordered.
pub async fn repo_secret_bind_summaries(
    pool: &DbPool,
    repo_ids: &[String],
) -> Result<Vec<RepoBindSummary>, AppError> {
    if repo_ids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = repo_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let secret_sql = format!(
        "SELECT b.repo_id, b.id, s.name, b.last_status, b.last_op
         FROM secret_github_binds b
         JOIN secrets s ON s.id = b.secret_id
         WHERE b.repo_id IN ({placeholders})
         ORDER BY s.name ASC"
    );
    let binds: Vec<SqlValue> = repo_ids.iter().map(|id| id.as_str().into()).collect();
    Ok(exec::fetch_all(pool, &secret_sql, &binds)
        .await?
        .into_iter()
        .map(|r| RepoBindSummary {
            repo_id: r.text(0),
            id: r.text(1),
            name: r.text(2),
            last_status: r.text(3),
            last_op: r.text(4),
        })
        .collect())
}

/// Variable binds for a batch of repos, variable-name-ordered.
pub async fn repo_variable_bind_summaries(
    pool: &DbPool,
    repo_ids: &[String],
) -> Result<Vec<RepoBindSummary>, AppError> {
    if repo_ids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = repo_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let var_sql = format!(
        "SELECT b.repo_id, b.id, v.name, b.last_status, b.last_op
         FROM variable_github_binds b
         JOIN platform_vars v ON v.id = b.var_id
         WHERE b.repo_id IN ({placeholders})
         ORDER BY v.name ASC"
    );
    let binds: Vec<SqlValue> = repo_ids.iter().map(|id| id.as_str().into()).collect();
    Ok(exec::fetch_all(pool, &var_sql, &binds)
        .await?
        .into_iter()
        .map(|r| RepoBindSummary {
            repo_id: r.text(0),
            id: r.text(1),
            name: r.text(2),
            last_status: r.text(3),
            last_op: r.text(4),
        })
        .collect())
}

/// One bind row for the secrets/variables list enrichment (the raw 11-column
/// join; override_value is stored material — secrets never expose its plaintext).
pub struct BindListRow {
    pub resource_id: String,
    pub id: String,
    pub repo_id: String,
    pub connection_name: String,
    pub owner: String,
    pub repo: String,
    pub last_status: String,
    pub last_pushed_at: Option<String>,
    pub last_error: String,
    pub last_op: String,
    pub override_value: Option<String>,
}

fn bind_list_row(r: exec::GenericRow) -> BindListRow {
    BindListRow {
        resource_id: r.text(0),
        id: r.text(1),
        repo_id: r.text(2),
        connection_name: r.text(3),
        owner: r.text(4),
        repo: r.text(5),
        last_status: r.text(6),
        last_pushed_at: r.opt_text(7),
        last_error: r.text(8),
        last_op: r.text(9),
        override_value: r.opt_text(10),
    }
}

/// Secret binds for a batch of secrets, owner/repo-ordered.
pub async fn secret_bind_summaries(
    pool: &DbPool,
    secret_ids: &[String],
) -> Result<Vec<BindListRow>, AppError> {
    if secret_ids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = secret_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let sql = format!(
        "SELECT b.secret_id, b.id, b.repo_id, c.name, r.owner, r.repo, b.last_status, b.last_pushed_at, b.last_error, b.last_op, b.override_value
         FROM secret_github_binds b
         JOIN github_repos r ON r.id = b.repo_id
         JOIN github_connections c ON c.id = r.connection_id
         WHERE b.secret_id IN ({placeholders})
         ORDER BY r.owner ASC, r.repo ASC"
    );
    let binds: Vec<SqlValue> = secret_ids.iter().map(|id| id.as_str().into()).collect();
    Ok(exec::fetch_all(pool, &sql, &binds)
        .await?
        .into_iter()
        .map(bind_list_row)
        .collect())
}

/// Variable binds for a batch of variables, owner/repo-ordered.
pub async fn variable_bind_summaries(
    pool: &DbPool,
    var_ids: &[String],
) -> Result<Vec<BindListRow>, AppError> {
    if var_ids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = var_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let sql = format!(
        "SELECT b.var_id, b.id, b.repo_id, c.name, r.owner, r.repo, b.last_status, b.last_pushed_at, b.last_error, b.last_op, b.override_value
         FROM variable_github_binds b
         JOIN github_repos r ON r.id = b.repo_id
         JOIN github_connections c ON c.id = r.connection_id
         WHERE b.var_id IN ({placeholders})
         ORDER BY r.owner ASC, r.repo ASC"
    );
    let binds: Vec<SqlValue> = var_ids.iter().map(|id| id.as_str().into()).collect();
    Ok(exec::fetch_all(pool, &sql, &binds)
        .await?
        .into_iter()
        .map(bind_list_row)
        .collect())
}
