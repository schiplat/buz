//! Platform variables: the platform-level `${vars.NAME}` defaults.
//!
//! Values are frequently credentials; the store returns stored values
//! verbatim and the handler decides who may see them.

use sqlx::{Row, SqlitePool};

use shared::error::AppError;

/// `(id, name, value, updated_at)` rows, name-ordered — the list page.
pub async fn list_all(
    pool: &SqlitePool,
) -> Result<Vec<(String, String, String, Option<String>)>, AppError> {
    let rows =
        sqlx::query("SELECT id, name, value, updated_at FROM platform_vars ORDER BY name ASC")
            .fetch_all(pool)
            .await?;
    Ok(rows
        .iter()
        .map(|r| (r.get(0), r.get(1), r.get(2), r.get(3)))
        .collect())
}

/// Id of the variable with this name — the create-time duplicate check.
pub async fn find_id_by_name(pool: &SqlitePool, name: &str) -> Result<Option<String>, AppError> {
    let row = sqlx::query("SELECT id FROM platform_vars WHERE name = ?")
        .bind(name)
        .fetch_optional(pool)
        .await?;
    Ok(row.map(|r| r.get(0)))
}

/// Insert one variable.
pub async fn insert(pool: &SqlitePool, id: &str, name: &str, value: &str) -> Result<(), AppError> {
    sqlx::query("INSERT INTO platform_vars (id, name, value) VALUES (?, ?, ?)")
        .bind(id)
        .bind(name)
        .bind(value)
        .execute(pool)
        .await?;
    Ok(())
}

/// `(id, name, value, updated_at)` for one variable (`None` = no such row).
pub async fn find_by_id(
    pool: &SqlitePool,
    id: &str,
) -> Result<Option<(String, String, String, Option<String>)>, AppError> {
    let row = sqlx::query("SELECT id, name, value, updated_at FROM platform_vars WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await?;
    Ok(row.map(|r| (r.get(0), r.get(1), r.get(2), r.get(3))))
}

/// `(name, value)` of one variable — the audit before-snapshot facts
/// (`None` = no such row).
pub async fn name_value(pool: &SqlitePool, id: &str) -> Result<Option<(String, String)>, AppError> {
    let row = sqlx::query("SELECT name, value FROM platform_vars WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await?;
    Ok(row.map(|r| (r.get(0), r.get(1))))
}

/// Replace one variable's value.
pub async fn update_value(pool: &SqlitePool, id: &str, value: &str) -> Result<(), AppError> {
    sqlx::query("UPDATE platform_vars SET value = ?, updated_at = datetime('now') WHERE id = ?")
        .bind(value)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Delete one variable row.
pub async fn delete(pool: &SqlitePool, id: &str) -> Result<(), AppError> {
    sqlx::query("DELETE FROM platform_vars WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/// True when the variable exists — the bind paths' existence gate.
pub async fn exists(pool: &SqlitePool, id: &str) -> Result<bool, AppError> {
    Ok(sqlx::query("SELECT id FROM platform_vars WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await?
        .is_some())
}

/// Display name of one variable (audit + remote-delete calls read this).
pub async fn variable_name(pool: &SqlitePool, id: &str) -> Result<String, AppError> {
    Ok(
        sqlx::query_scalar("SELECT name FROM platform_vars WHERE id = ?")
            .bind(id)
            .fetch_one(pool)
            .await?,
    )
}

/// Display name of one variable, tolerated absent (`None` = no such row).
pub async fn variable_name_optional(
    pool: &SqlitePool,
    id: &str,
) -> Result<Option<String>, AppError> {
    Ok(
        sqlx::query_scalar("SELECT name FROM platform_vars WHERE id = ?")
            .bind(id)
            .fetch_optional(pool)
            .await?,
    )
}

/// `(name, value)` for the GitHub push path (strict: errors when absent).
pub async fn push_facts(pool: &SqlitePool, id: &str) -> Result<(String, String), AppError> {
    let row = sqlx::query("SELECT name, value FROM platform_vars WHERE id = ?")
        .bind(id)
        .fetch_one(pool)
        .await?;
    Ok((row.get(0), row.get(1)))
}
