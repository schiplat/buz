//! User administration (platform-admin plane): listing, creation, the
//! per-field update path with its last-admin guards, and deletion.
//!
//! On the dual-backend dispatcher (`store::exec`): statements travel as plain
//! SQLite-dialect strings with backend-neutral binds, rows decode by column
//! name. Strict one-row reads keep `fetch_one` semantics via
//! `ok_or(sqlx::Error::RowNotFound)`.

use crate::store::exec::{self, DbPool, SqlValue};

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
pub async fn list_all(pool: &DbPool) -> Result<Vec<UserRow>, AppError> {
    let rows = exec::fetch_all(
        pool,
        "SELECT id, username, display_name, is_platform_admin, is_super_admin, is_active, created_at, updated_at, is_ops
         FROM users ORDER BY username ASC",
        &[],
    )
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| UserRow {
            id: r.text_of("id"),
            username: r.text_of("username"),
            display_name: r.text_of("display_name"),
            is_platform_admin: r.i64_of("is_platform_admin") != 0,
            is_super_admin: r.i64_of("is_super_admin") != 0,
            is_active: r.i64_of("is_active") != 0,
            created_at: r.opt_text_of("created_at"),
            updated_at: r.opt_text_of("updated_at"),
            is_ops: r.i64_of("is_ops") != 0,
        })
        .collect())
}

/// Insert one active user. A duplicate username (UNIQUE constraint)
/// surfaces as `Conflict` — the create path's contract.
#[allow(clippy::too_many_arguments)]
pub async fn insert(
    pool: &DbPool,
    id: &str,
    username: &str,
    password_hash: &str,
    display_name: &str,
    is_platform_admin: bool,
    is_super_admin: bool,
    is_ops: bool,
) -> Result<(), AppError> {
    exec::execute(
        pool,
        "INSERT INTO users (id, username, password_hash, display_name, is_platform_admin, is_super_admin, is_ops, is_active)
         VALUES (?, ?, ?, ?, ?, ?, ?, 1)",
        &[
            id.into(),
            username.into(),
            password_hash.into(),
            display_name.into(),
            SqlValue::Int(if is_platform_admin { 1 } else { 0 }),
            SqlValue::Int(if is_super_admin { 1 } else { 0 }),
            SqlValue::Int(if is_ops { 1 } else { 0 }),
        ],
    )
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

pub async fn update_facts(pool: &DbPool, id: &str) -> Result<Option<UserUpdateBefore>, AppError> {
    let row = exec::fetch_optional(
        pool,
        "SELECT id, username, display_name, is_platform_admin, is_super_admin, is_active FROM users WHERE id = ?",
        &[id.into()],
    )
    .await?;
    Ok(row.map(|r| UserUpdateBefore {
        id: r.text_of("id"),
        username: r.text_of("username"),
        display_name: r.text_of("display_name"),
        is_platform_admin: r.i64_of("is_platform_admin") != 0,
        is_super_admin: r.i64_of("is_super_admin") != 0,
        is_active: r.i64_of("is_active") != 0,
    }))
}

/// Replace one user's password hash.
pub async fn update_password(pool: &DbPool, id: &str, password_hash: &str) -> Result<(), AppError> {
    exec::execute(
        pool,
        "UPDATE users SET password_hash = ?, updated_at = datetime('now') WHERE id = ?",
        &[password_hash.into(), id.into()],
    )
    .await?;
    Ok(())
}

/// Replace one user's display name.
pub async fn update_display_name(
    pool: &DbPool,
    id: &str,
    display_name: &str,
) -> Result<(), AppError> {
    exec::execute(
        pool,
        "UPDATE users SET display_name = ?, updated_at = datetime('now') WHERE id = ?",
        &[display_name.into(), id.into()],
    )
    .await?;
    Ok(())
}

/// Active platform-admin count — the "cannot remove the last platform
/// admin" guard reads this.
pub async fn count_active_platform_admins(pool: &DbPool) -> Result<i64, AppError> {
    Ok(exec::fetch_scalar_i64(
        pool,
        "SELECT COUNT(*) FROM users WHERE is_platform_admin = 1 AND is_active = 1",
        &[],
    )
    .await?)
}

/// Set one user's platform-admin flag.
pub async fn update_is_platform_admin(pool: &DbPool, id: &str, flag: bool) -> Result<(), AppError> {
    exec::execute(
        pool,
        "UPDATE users SET is_platform_admin = ?, updated_at = datetime('now') WHERE id = ?",
        &[SqlValue::Int(if flag { 1 } else { 0 }), id.into()],
    )
    .await?;
    Ok(())
}

/// Active super-admin count — the "cannot remove the last super admin"
/// guard reads this.
pub async fn count_active_super_admins(pool: &DbPool) -> Result<i64, AppError> {
    Ok(exec::fetch_scalar_i64(
        pool,
        "SELECT COUNT(*) FROM users WHERE is_super_admin = 1 AND is_active = 1",
        &[],
    )
    .await?)
}

/// Set one user's super-admin flag.
pub async fn update_is_super_admin(pool: &DbPool, id: &str, flag: bool) -> Result<(), AppError> {
    exec::execute(
        pool,
        "UPDATE users SET is_super_admin = ?, updated_at = datetime('now') WHERE id = ?",
        &[SqlValue::Int(if flag { 1 } else { 0 }), id.into()],
    )
    .await?;
    Ok(())
}

/// Set one user's platform-ops flag.
pub async fn update_is_ops(pool: &DbPool, id: &str, flag: bool) -> Result<(), AppError> {
    exec::execute(
        pool,
        "UPDATE users SET is_ops = ?, updated_at = datetime('now') WHERE id = ?",
        &[SqlValue::Int(if flag { 1 } else { 0 }), id.into()],
    )
    .await?;
    Ok(())
}

/// Set one user's active flag.
pub async fn update_is_active(pool: &DbPool, id: &str, flag: bool) -> Result<(), AppError> {
    exec::execute(
        pool,
        "UPDATE users SET is_active = ?, updated_at = datetime('now') WHERE id = ?",
        &[SqlValue::Int(if flag { 1 } else { 0 }), id.into()],
    )
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

pub async fn update_after(pool: &DbPool, id: &str) -> Result<UserUpdateAfter, AppError> {
    let row = exec::fetch_optional(
        pool,
        "SELECT id, username, display_name, is_platform_admin, is_super_admin, is_active, is_ops FROM users WHERE id = ?",
        &[id.into()],
    )
    .await?
    .ok_or(sqlx::Error::RowNotFound)?;
    Ok(UserUpdateAfter {
        id: row.text_of("id"),
        username: row.text_of("username"),
        display_name: row.text_of("display_name"),
        is_platform_admin: row.i64_of("is_platform_admin") != 0,
        is_super_admin: row.i64_of("is_super_admin") != 0,
        is_active: row.i64_of("is_active") != 0,
        is_ops: row.i64_of("is_ops") != 0,
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

pub async fn delete_facts(pool: &DbPool, id: &str) -> Result<Option<UserDeleteBefore>, AppError> {
    let row = exec::fetch_optional(
        pool,
        "SELECT username, display_name, is_platform_admin, is_super_admin, is_active FROM users WHERE id = ?",
        &[id.into()],
    )
    .await?;
    Ok(row.map(|r| UserDeleteBefore {
        username: r.text_of("username"),
        display_name: r.text_of("display_name"),
        is_platform_admin: r.i64_of("is_platform_admin") != 0,
        is_super_admin: r.i64_of("is_super_admin") != 0,
        is_active: r.i64_of("is_active") != 0,
    }))
}

/// Delete one user; returns `false` when nothing matched (caller surfaces
/// `NotFound`).
pub async fn delete(pool: &DbPool, id: &str) -> Result<bool, AppError> {
    let n = exec::execute(pool, "DELETE FROM users WHERE id = ?", &[id.into()]).await?;
    Ok(n > 0)
}
