//! User administration (platform-admin plane): listing, creation, the
//! per-field update path with its last-admin guards, and deletion.

use sqlx::{Row, SqlitePool};

use shared::error::AppError;

/// One user row, list-page shape.
#[derive(Debug, Clone)]
pub struct UserRow {
    pub id: String,
    pub username: String,
    pub display_name: String,
    pub is_platform_admin: bool,
    pub is_super_admin: bool,
    pub is_active: bool,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    pub is_ops: bool,
}

/// All users, username-ordered.
pub async fn list_all(pool: &SqlitePool) -> Result<Vec<UserRow>, AppError> {
    let rows = sqlx::query(
        "SELECT id, username, display_name, is_platform_admin, is_super_admin, is_active, created_at, updated_at, is_ops
         FROM users ORDER BY username ASC",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| UserRow {
            id: r.get(0),
            username: r.get(1),
            display_name: r.get(2),
            is_platform_admin: r.get::<i64, _>(3) != 0,
            is_super_admin: r.get::<i64, _>(4) != 0,
            is_active: r.get::<i64, _>(5) != 0,
            created_at: r.get(6),
            updated_at: r.get(7),
            is_ops: r.get::<i64, _>(8) != 0,
        })
        .collect())
}

/// Insert one active user. A duplicate username (UNIQUE constraint)
/// surfaces as `Conflict` — the create path's contract.
#[allow(clippy::too_many_arguments)]
pub async fn insert(
    pool: &SqlitePool,
    id: &str,
    username: &str,
    password_hash: &str,
    display_name: &str,
    is_platform_admin: bool,
    is_super_admin: bool,
    is_ops: bool,
) -> Result<(), AppError> {
    sqlx::query(
        "INSERT INTO users (id, username, password_hash, display_name, is_platform_admin, is_super_admin, is_ops, is_active)
         VALUES (?, ?, ?, ?, ?, ?, ?, 1)",
    )
    .bind(id)
    .bind(username)
    .bind(password_hash)
    .bind(display_name)
    .bind(if is_platform_admin { 1 } else { 0 })
    .bind(if is_super_admin { 1 } else { 0 })
    .bind(if is_ops { 1 } else { 0 })
    .execute(pool)
    .await
    .map_err(|e| {
        if e.to_string().contains("UNIQUE") {
            AppError::Conflict("username already exists".into())
        } else {
            AppError::DatabaseError(e.to_string())
        }
    })?;
    Ok(())
}

/// Pre-update facts (`None` = no such user) — the audit before-snapshot.
#[derive(Debug, Clone)]
pub struct UserUpdateBefore {
    pub id: String,
    pub username: String,
    pub display_name: String,
    pub is_platform_admin: bool,
    pub is_super_admin: bool,
    pub is_active: bool,
}

pub async fn update_facts(
    pool: &SqlitePool,
    id: &str,
) -> Result<Option<UserUpdateBefore>, AppError> {
    let row = sqlx::query(
        "SELECT id, username, display_name, is_platform_admin, is_super_admin, is_active FROM users WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| UserUpdateBefore {
        id: r.get(0),
        username: r.get(1),
        display_name: r.get(2),
        is_platform_admin: r.get::<i64, _>(3) != 0,
        is_super_admin: r.get::<i64, _>(4) != 0,
        is_active: r.get::<i64, _>(5) != 0,
    }))
}

/// Replace one user's password hash.
pub async fn update_password(
    pool: &SqlitePool,
    id: &str,
    password_hash: &str,
) -> Result<(), AppError> {
    sqlx::query("UPDATE users SET password_hash = ?, updated_at = datetime('now') WHERE id = ?")
        .bind(password_hash)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Replace one user's display name.
pub async fn update_display_name(
    pool: &SqlitePool,
    id: &str,
    display_name: &str,
) -> Result<(), AppError> {
    sqlx::query("UPDATE users SET display_name = ?, updated_at = datetime('now') WHERE id = ?")
        .bind(display_name)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Active platform-admin count — the "cannot remove the last platform
/// admin" guard reads this.
pub async fn count_active_platform_admins(pool: &SqlitePool) -> Result<i64, AppError> {
    Ok(sqlx::query_scalar(
        "SELECT COUNT(*) FROM users WHERE is_platform_admin = 1 AND is_active = 1",
    )
    .fetch_one(pool)
    .await?)
}

/// Set one user's platform-admin flag.
pub async fn update_is_platform_admin(
    pool: &SqlitePool,
    id: &str,
    flag: bool,
) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE users SET is_platform_admin = ?, updated_at = datetime('now') WHERE id = ?",
    )
    .bind(if flag { 1 } else { 0 })
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Active super-admin count — the "cannot remove the last super admin"
/// guard reads this.
pub async fn count_active_super_admins(pool: &SqlitePool) -> Result<i64, AppError> {
    Ok(
        sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE is_super_admin = 1 AND is_active = 1")
            .fetch_one(pool)
            .await?,
    )
}

/// Set one user's super-admin flag.
pub async fn update_is_super_admin(
    pool: &SqlitePool,
    id: &str,
    flag: bool,
) -> Result<(), AppError> {
    sqlx::query("UPDATE users SET is_super_admin = ?, updated_at = datetime('now') WHERE id = ?")
        .bind(if flag { 1 } else { 0 })
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Set one user's platform-ops flag.
pub async fn update_is_ops(pool: &SqlitePool, id: &str, flag: bool) -> Result<(), AppError> {
    sqlx::query("UPDATE users SET is_ops = ?, updated_at = datetime('now') WHERE id = ?")
        .bind(if flag { 1 } else { 0 })
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Set one user's active flag.
pub async fn update_is_active(pool: &SqlitePool, id: &str, flag: bool) -> Result<(), AppError> {
    sqlx::query("UPDATE users SET is_active = ?, updated_at = datetime('now') WHERE id = ?")
        .bind(if flag { 1 } else { 0 })
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Post-update facts (all seven columns, including `is_ops`) — the audit
/// after-snapshot.
#[derive(Debug, Clone)]
pub struct UserUpdateAfter {
    pub id: String,
    pub username: String,
    pub display_name: String,
    pub is_platform_admin: bool,
    pub is_super_admin: bool,
    pub is_active: bool,
    pub is_ops: bool,
}

pub async fn update_after(pool: &SqlitePool, id: &str) -> Result<UserUpdateAfter, AppError> {
    let row = sqlx::query(
        "SELECT id, username, display_name, is_platform_admin, is_super_admin, is_active, is_ops FROM users WHERE id = ?",
    )
    .bind(id)
    .fetch_one(pool)
    .await?;
    Ok(UserUpdateAfter {
        id: row.get(0),
        username: row.get(1),
        display_name: row.get(2),
        is_platform_admin: row.get::<i64, _>(3) != 0,
        is_super_admin: row.get::<i64, _>(4) != 0,
        is_active: row.get::<i64, _>(5) != 0,
        is_ops: row.get::<i64, _>(6) != 0,
    })
}

/// Pre-delete facts (`None` = no such user) — the audit before-snapshot.
#[derive(Debug, Clone)]
pub struct UserDeleteBefore {
    pub username: String,
    pub display_name: String,
    pub is_platform_admin: bool,
    pub is_super_admin: bool,
    pub is_active: bool,
}

pub async fn delete_facts(
    pool: &SqlitePool,
    id: &str,
) -> Result<Option<UserDeleteBefore>, AppError> {
    let row = sqlx::query(
        "SELECT username, display_name, is_platform_admin, is_super_admin, is_active FROM users WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| UserDeleteBefore {
        username: r.get(0),
        display_name: r.get(1),
        is_platform_admin: r.get::<i64, _>(2) != 0,
        is_super_admin: r.get::<i64, _>(3) != 0,
        is_active: r.get::<i64, _>(4) != 0,
    }))
}

/// Delete one user; returns `false` when nothing matched (caller surfaces
/// `NotFound`).
pub async fn delete(pool: &SqlitePool, id: &str) -> Result<bool, AppError> {
    let result = sqlx::query("DELETE FROM users WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}
