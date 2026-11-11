//! Secret CRUD (platform-level `${secrets.NAME}` credentials).
//!
//! Values are stored AES-encrypted (`crypto::encrypt`, JWT-secret keyed);
//! the store never sees plaintext except as a ciphertext argument. The
//! list/read rows carry the stored value verbatim — decryption, masking and
//! the `reencrypt_required` fallback are API-display concerns.

use sqlx::{Row, SqlitePool};

use shared::error::AppError;

/// One secret row as stored. `value` is the ciphertext; `team_shared` is the
/// raw 0/1 column.
#[derive(Debug, Clone)]
pub struct SecretRow {
    pub id: String,
    pub name: String,
    pub value: String,
    pub owner_group: String,
    pub team_id: String,
    pub team_shared: i64,
    pub kind: String,
    pub updated_at: Option<String>,
}

fn secret_row(r: sqlx::sqlite::SqliteRow) -> SecretRow {
    SecretRow {
        id: r.get(0),
        name: r.get(1),
        value: r.get(2),
        owner_group: r.get(3),
        team_id: r.get(4),
        team_shared: r.get(5),
        kind: r.get(6),
        updated_at: r.get(7),
    }
}

/// All secrets, name-ordered. `visible_team_ids = None` is unrestricted
/// (super admin); `Some([])` yields nothing (the caller's team filter is
/// unsatisfiable).
pub async fn list(
    pool: &SqlitePool,
    visible_team_ids: Option<&[String]>,
) -> Result<Vec<SecretRow>, AppError> {
    let Some(ids) = visible_team_ids else {
        let rows = sqlx::query(
            "SELECT id, name, value, owner_group, team_id, team_shared, kind, updated_at FROM secrets ORDER BY name ASC"
        )
        .fetch_all(pool)
        .await?;
        return Ok(rows.into_iter().map(secret_row).collect());
    };
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let sql = format!(
        "SELECT id, name, value, owner_group, team_id, team_shared, kind, updated_at FROM secrets WHERE 1=1 AND team_id IN ({placeholders}) ORDER BY name ASC"
    );
    let mut q = sqlx::query(&sql);
    for id in ids {
        q = q.bind(id);
    }
    let rows = q.fetch_all(pool).await?;
    Ok(rows.into_iter().map(secret_row).collect())
}

/// Id of the secret with this name — the create-time duplicate check
/// (`name` is globally UNIQUE).
pub async fn find_id_by_name(pool: &SqlitePool, name: &str) -> Result<Option<String>, AppError> {
    let row = sqlx::query("SELECT id FROM secrets WHERE name = ?")
        .bind(name)
        .fetch_optional(pool)
        .await?;
    Ok(row.map(|r| r.get(0)))
}

/// Insert one secret. `value` is the ciphertext; `owner_group` is the team
/// slug; `team_shared_i` is the raw 0/1 column value.
#[allow(clippy::too_many_arguments)]
pub async fn insert(
    pool: &SqlitePool,
    id: &str,
    name: &str,
    value: &str,
    owner_group: &str,
    team_id: &str,
    team_shared_i: i64,
    kind: &str,
) -> Result<(), AppError> {
    sqlx::query(
        "INSERT INTO secrets (id, name, value, owner_group, team_id, team_shared, kind) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(id)
    .bind(name)
    .bind(value)
    .bind(owner_group)
    .bind(team_id)
    .bind(team_shared_i)
    .bind(kind)
    .execute(pool)
    .await?;
    Ok(())
}

/// One secret row by id (`None` = no such secret).
pub async fn find_by_id(pool: &SqlitePool, id: &str) -> Result<Option<SecretRow>, AppError> {
    let row = sqlx::query(
        "SELECT id, name, value, owner_group, team_id, team_shared, kind, updated_at FROM secrets WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(secret_row))
}

/// `owner_group` slug of one secret — the update path's team fallback.
pub async fn owner_group_of(pool: &SqlitePool, id: &str) -> Result<String, AppError> {
    Ok(
        sqlx::query_scalar("SELECT owner_group FROM secrets WHERE id = ?")
            .bind(id)
            .fetch_one(pool)
            .await?,
    )
}

/// Pre-update facts: name, owner_group, team_id, team_shared (raw 0/1),
/// kind and the stored (encrypted) value — the audit before-snapshot and
/// the changed-value detection read these. `None` = no such secret.
pub struct SecretUpdateFacts {
    pub name: String,
    pub owner_group: String,
    pub team_id: String,
    pub team_shared: i64,
    pub kind: String,
    pub value: String,
}

pub async fn update_facts(pool: &SqlitePool, id: &str) -> Result<SecretUpdateFacts, AppError> {
    let row = sqlx::query(
        "SELECT name, owner_group, team_id, team_shared, kind, value FROM secrets WHERE id = ?",
    )
    .bind(id)
    .fetch_one(pool)
    .await?;
    Ok(SecretUpdateFacts {
        name: row.get(0),
        owner_group: row.get(1),
        team_id: row.get(2),
        team_shared: row.get(3),
        kind: row.get(4),
        value: row.get(5),
    })
}

/// Update a secret's metadata only (no value change).
#[allow(clippy::too_many_arguments)]
pub async fn update_metadata(
    pool: &SqlitePool,
    id: &str,
    owner_group: &str,
    team_id: &str,
    team_shared_i: i64,
    kind: &str,
) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE secrets SET owner_group = ?, team_id = ?, team_shared = ?, kind = ?, \
             updated_at = datetime('now') WHERE id = ?",
    )
    .bind(owner_group)
    .bind(team_id)
    .bind(team_shared_i)
    .bind(kind)
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Update a secret's value (new ciphertext) and metadata together.
#[allow(clippy::too_many_arguments)]
pub async fn update_value_and_metadata(
    pool: &SqlitePool,
    id: &str,
    value: &str,
    owner_group: &str,
    team_id: &str,
    team_shared_i: i64,
    kind: &str,
) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE secrets SET value = ?, owner_group = ?, team_id = ?, team_shared = ?, kind = ?, \
             updated_at = datetime('now') WHERE id = ?",
    )
    .bind(value)
    .bind(owner_group)
    .bind(team_id)
    .bind(team_shared_i)
    .bind(kind)
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Pre-delete facts: name, owner_group, team_id, team_shared (raw 0/1), kind
/// — the audit before-snapshot. `None` = no such secret.
pub struct SecretDeleteFacts {
    pub name: String,
    pub owner_group: String,
    pub team_id: String,
    pub team_shared: i64,
    pub kind: String,
}

pub async fn delete_facts(pool: &SqlitePool, id: &str) -> Result<SecretDeleteFacts, AppError> {
    let row = sqlx::query(
        "SELECT name, owner_group, team_id, team_shared, kind FROM secrets WHERE id = ?",
    )
    .bind(id)
    .fetch_one(pool)
    .await?;
    Ok(SecretDeleteFacts {
        name: row.get(0),
        owner_group: row.get(1),
        team_id: row.get(2),
        team_shared: row.get(3),
        kind: row.get(4),
    })
}

/// Delete one secret row.
pub async fn delete(pool: &SqlitePool, id: &str) -> Result<(), AppError> {
    sqlx::query("DELETE FROM secrets WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Display name of one secret (audit + remote-delete calls read this).
pub async fn secret_name(pool: &SqlitePool, id: &str) -> Result<String, AppError> {
    Ok(sqlx::query_scalar("SELECT name FROM secrets WHERE id = ?")
        .bind(id)
        .fetch_one(pool)
        .await?)
}

/// Display name of one secret, tolerated absent (`None` = no such row).
pub async fn secret_name_optional(pool: &SqlitePool, id: &str) -> Result<Option<String>, AppError> {
    Ok(sqlx::query_scalar("SELECT name FROM secrets WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await?)
}

/// `(name, stored value, team_id)` — the GitHub push path's facts.
pub async fn push_facts(pool: &SqlitePool, id: &str) -> Result<(String, String, String), AppError> {
    let row = sqlx::query("SELECT name, value, team_id FROM secrets WHERE id = ?")
        .bind(id)
        .fetch_one(pool)
        .await?;
    Ok((row.get(0), row.get(1), row.get(2)))
}
