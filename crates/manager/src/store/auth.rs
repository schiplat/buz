//! Identity plane: user counts, first-admin provisioning, login lookup and
//! password changes. The first-admin setup transaction is owned by the
//! handler (it carries the audit record); these tx-internal steps speak SQL
//! inside it.
//!
//! Pool-level functions run on the dual-backend dispatcher (`store::exec`).
//! The `*_tx` helpers stay on `sqlx::Transaction<Sqlite>` — they share the
//! setup transaction scope with `crate::audit::record_in` and migrate with
//! the audit tx-scope slice.

use crate::store::exec::{self, DbPool};

use shared::error::AppError;

type Tx<'a> = sqlx::Transaction<'a, sqlx::Sqlite>;

/// Total user count — the "is this instance initialized" fact.
pub async fn count_users(pool: &DbPool) -> Result<i64, AppError> {
    Ok(exec::fetch_scalar_i64(pool, "SELECT COUNT(*) FROM users", &[]).await?)
}

/// Total user count inside the caller's transaction (setup's re-check).
pub async fn count_users_tx(tx: &mut Tx<'_>) -> Result<i64, AppError> {
    Ok(sqlx::query_scalar("SELECT COUNT(*) FROM users")
        .fetch_one(&mut **tx)
        .await?)
}

/// Insert the first platform admin (super admin, active).
pub async fn insert_first_admin_tx(
    tx: &mut Tx<'_>,
    user_id: &str,
    username: &str,
    password_hash: &str,
) -> Result<(), AppError> {
    sqlx::query(
        "INSERT INTO users (id, username, password_hash, display_name, is_platform_admin, is_super_admin, is_active)
         VALUES (?, ?, ?, ?, 1, 1, 1)",
    )
    .bind(user_id)
    .bind(username)
    .bind(password_hash)
    .bind(username)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Id of the bootstrap default team, if it exists.
pub async fn default_team_id_tx(tx: &mut Tx<'_>) -> Result<Option<String>, AppError> {
    Ok(
        sqlx::query_scalar("SELECT id FROM teams WHERE slug = 'default'")
            .fetch_optional(&mut **tx)
            .await?,
    )
}

/// Attach a user to a team as owner (no-op when already a member).
pub async fn attach_team_owner_tx(
    tx: &mut Tx<'_>,
    team_id: &str,
    user_id: &str,
) -> Result<(), AppError> {
    sqlx::query(
        "INSERT OR IGNORE INTO team_members (team_id, user_id, role) VALUES (?, ?, 'owner')",
    )
    .bind(team_id)
    .bind(user_id)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// One user's login facts by username (`None` = no such user). The password
/// hash is needed for verification only — never log it.
#[derive(Debug, Clone)]
pub struct LoginUser {
    pub id: String,
    pub username: String,
    pub password_hash: String,
    pub is_platform_admin: bool,
    pub is_active: bool,
    pub is_ops: bool,
}

pub async fn find_login_by_username(
    pool: &DbPool,
    username: &str,
) -> Result<Option<LoginUser>, AppError> {
    let row = exec::fetch_optional(
        pool,
        "SELECT id, username, password_hash, is_platform_admin, is_active, is_ops
         FROM users WHERE username = ?",
        &[username.into()],
    )
    .await?;
    Ok(row.map(|r| LoginUser {
        id: r.text_of("id"),
        username: r.text_of("username"),
        password_hash: r.text_of("password_hash"),
        is_platform_admin: r.i64_of("is_platform_admin") != 0,
        is_active: r.i64_of("is_active") != 0,
        is_ops: r.i64_of("is_ops") != 0,
    }))
}

/// Stored password hash of one user (`None` = no such user).
pub async fn password_hash_of(pool: &DbPool, user_id: &str) -> Result<Option<String>, AppError> {
    Ok(exec::fetch_optional(
        pool,
        "SELECT password_hash FROM users WHERE id = ?",
        &[user_id.into()],
    )
    .await?
    .map(|r| r.text_of("password_hash")))
}

/// Replace one user's password hash.
pub async fn update_password(
    pool: &DbPool,
    user_id: &str,
    password_hash: &str,
) -> Result<(), AppError> {
    exec::execute(
        pool,
        "UPDATE users SET password_hash = ?, updated_at = datetime('now') WHERE id = ?",
        &[password_hash.into(), user_id.into()],
    )
    .await?;
    Ok(())
}
