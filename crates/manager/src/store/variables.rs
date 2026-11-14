//! Platform variables: the platform-level `${vars.NAME}` defaults.
//!
//! Values are frequently credentials; the store returns stored values
//! verbatim and the handler decides who may see them.
//!
//! On the dual-backend dispatcher (`store::exec`): statements travel as plain
//! SQLite-dialect strings with backend-neutral binds, rows decode through the
//! runtime type cascade.

use crate::store::exec::{self, DbPool, SqlValue};

use shared::error::AppError;

/// `(id, name, value, updated_at)` rows, name-ordered — the list page.
pub async fn list_all(
    pool: &DbPool,
) -> Result<Vec<(String, String, String, Option<String>)>, AppError> {
    let rows = exec::fetch_all(
        pool,
        "SELECT id, name, value, updated_at FROM platform_vars ORDER BY name ASC",
        &[],
    )
    .await?;
    Ok(rows
        .iter()
        .map(|r| {
            (
                r.text_of("id"),
                r.text_of("name"),
                r.text_of("value"),
                r.opt_text_of("updated_at"),
            )
        })
        .collect())
}

/// Id of the variable with this name — the create-time duplicate check.
pub async fn find_id_by_name(pool: &DbPool, name: &str) -> Result<Option<String>, AppError> {
    let row = exec::fetch_optional(
        pool,
        "SELECT id FROM platform_vars WHERE name = ?",
        &[name.into()],
    )
    .await?;
    Ok(row.map(|r| r.text_of("id")))
}

/// Insert one variable.
pub async fn insert(pool: &DbPool, id: &str, name: &str, value: &str) -> Result<(), AppError> {
    exec::execute(
        pool,
        "INSERT INTO platform_vars (id, name, value) VALUES (?, ?, ?)",
        &[id.into(), name.into(), value.into()],
    )
    .await?;
    Ok(())
}

/// `(id, name, value, updated_at)` for one variable (`None` = no such row).
pub async fn find_by_id(
    pool: &DbPool,
    id: &str,
) -> Result<Option<(String, String, String, Option<String>)>, AppError> {
    let row = exec::fetch_optional(
        pool,
        "SELECT id, name, value, updated_at FROM platform_vars WHERE id = ?",
        &[id.into()],
    )
    .await?;
    Ok(row.map(|r| {
        (
            r.text_of("id"),
            r.text_of("name"),
            r.text_of("value"),
            r.opt_text_of("updated_at"),
        )
    }))
}

/// `(name, value)` of one variable — the audit before-snapshot facts
/// (`None` = no such row).
pub async fn name_value(pool: &DbPool, id: &str) -> Result<Option<(String, String)>, AppError> {
    let row = exec::fetch_optional(
        pool,
        "SELECT name, value FROM platform_vars WHERE id = ?",
        &[id.into()],
    )
    .await?;
    Ok(row.map(|r| (r.text_of("name"), r.text_of("value"))))
}

/// Replace one variable's value.
pub async fn update_value(pool: &DbPool, id: &str, value: &str) -> Result<(), AppError> {
    exec::execute(
        pool,
        "UPDATE platform_vars SET value = ?, updated_at = datetime('now') WHERE id = ?",
        &[value.into(), id.into()],
    )
    .await?;
    Ok(())
}

/// Delete one variable row.
pub async fn delete(pool: &DbPool, id: &str) -> Result<(), AppError> {
    exec::execute(pool, "DELETE FROM platform_vars WHERE id = ?", &[id.into()]).await?;
    Ok(())
}

/// True when the variable exists — the bind paths' existence gate.
pub async fn exists(pool: &DbPool, id: &str) -> Result<bool, AppError> {
    Ok(exec::fetch_optional(
        pool,
        "SELECT id FROM platform_vars WHERE id = ?",
        &[id.into()],
    )
    .await?
    .is_some())
}

/// Display name of one variable (audit + remote-delete calls read this).
pub async fn variable_name(pool: &DbPool, id: &str) -> Result<String, AppError> {
    let row = exec::fetch_optional(
        pool,
        "SELECT name FROM platform_vars WHERE id = ?",
        &[id.into()],
    )
    .await?
    .ok_or(sqlx::Error::RowNotFound)?;
    Ok(row.text_of("name"))
}

/// Display name of one variable, tolerated absent (`None` = no such row).
pub async fn variable_name_optional(pool: &DbPool, id: &str) -> Result<Option<String>, AppError> {
    let row = exec::fetch_optional(
        pool,
        "SELECT name FROM platform_vars WHERE id = ?",
        &[id.into()],
    )
    .await?;
    Ok(row.and_then(|r| r.opt_text_of("name")))
}

/// Which of these names exist as platform variables — the YAML `variables:` check.
pub async fn existing_platform_names(
    pool: &DbPool,
    names: &[String],
) -> Result<std::collections::HashSet<String>, AppError> {
    if names.is_empty() {
        return Ok(std::collections::HashSet::new());
    }
    let placeholders = vec!["?"; names.len()].join(",");
    let sql = format!("SELECT name FROM platform_vars WHERE name IN ({placeholders})");
    let binds: Vec<SqlValue> = names.iter().map(|n| SqlValue::from(n.as_str())).collect();
    Ok(exec::fetch_all(pool, &sql, &binds)
        .await?
        .into_iter()
        .map(|r| r.text(0))
        .collect())
}

/// `(name, value)` for the GitHub push path (strict: errors when absent).
pub async fn push_facts(pool: &DbPool, id: &str) -> Result<(String, String), AppError> {
    let row = exec::fetch_optional(
        pool,
        "SELECT name, value FROM platform_vars WHERE id = ?",
        &[id.into()],
    )
    .await?
    .ok_or(sqlx::Error::RowNotFound)?;
    Ok((row.text_of("name"), row.text_of("value")))
}
