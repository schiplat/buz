//! Secret CRUD (platform-level `${secrets.NAME}` credentials).
//!
//! Values are stored AES-encrypted (`crypto::encrypt`, JWT-secret keyed);
//! the store never sees plaintext except as a ciphertext argument. The
//! list/read rows carry the stored value verbatim — decryption, masking and
//! the `reencrypt_required` fallback are API-display concerns.
//!
//! On the dual-backend dispatcher (`store::exec`): statements travel as plain
//! SQLite-dialect strings with backend-neutral binds, rows decode by column
//! name. Strict one-row reads keep `fetch_one` semantics via
//! `ok_or(sqlx::Error::RowNotFound)`.

use crate::store::exec::{self, DbPool, SqlValue};

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

fn secret_row(r: crate::store::exec::GenericRow) -> SecretRow {
    SecretRow {
        id: r.text_of("id"),
        name: r.text_of("name"),
        value: r.text_of("value"),
        owner_group: r.text_of("owner_group"),
        team_id: r.text_of("team_id"),
        team_shared: r.i64_of("team_shared"),
        kind: r.text_of("kind"),
        updated_at: r.opt_text_of("updated_at"),
    }
}

/// All secrets, name-ordered. `visible_team_ids = None` is unrestricted
/// (super admin); `Some([])` yields nothing (the caller's team filter is
/// unsatisfiable).
pub async fn list(
    pool: &DbPool,
    visible_team_ids: Option<&[String]>,
) -> Result<Vec<SecretRow>, AppError> {
    let Some(ids) = visible_team_ids else {
        let rows = exec::fetch_all(
            pool,
            "SELECT id, name, value, owner_group, team_id, team_shared, kind, updated_at FROM secrets ORDER BY name ASC",
            &[],
        )
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
    let binds: Vec<SqlValue> = ids.iter().map(|id| id.as_str().into()).collect();
    let rows = exec::fetch_all(pool, &sql, &binds).await?;
    Ok(rows.into_iter().map(secret_row).collect())
}

/// Id of the secret with this name — the create-time duplicate check
/// (`name` is globally UNIQUE).
pub async fn find_id_by_name(pool: &DbPool, name: &str) -> Result<Option<String>, AppError> {
    let row = exec::fetch_optional(
        pool,
        "SELECT id FROM secrets WHERE name = ?",
        &[name.into()],
    )
    .await?;
    Ok(row.map(|r| r.text_of("id")))
}

/// Insert one secret. `value` is the ciphertext; `owner_group` is the team
/// slug; `team_shared_i` is the raw 0/1 column value.
#[allow(clippy::too_many_arguments)]
pub async fn insert(
    pool: &DbPool,
    id: &str,
    name: &str,
    value: &str,
    owner_group: &str,
    team_id: &str,
    team_shared_i: i64,
    kind: &str,
) -> Result<(), AppError> {
    exec::execute(
        pool,
        "INSERT INTO secrets (id, name, value, owner_group, team_id, team_shared, kind) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
        &[
            id.into(),
            name.into(),
            value.into(),
            owner_group.into(),
            team_id.into(),
            SqlValue::Int(team_shared_i),
            kind.into(),
        ],
    )
    .await?;
    Ok(())
}

/// One secret row by id (`None` = no such secret).
pub async fn find_by_id(pool: &DbPool, id: &str) -> Result<Option<SecretRow>, AppError> {
    let row = exec::fetch_optional(
        pool,
        "SELECT id, name, value, owner_group, team_id, team_shared, kind, updated_at FROM secrets WHERE id = ?",
        &[id.into()],
    )
    .await?;
    Ok(row.map(secret_row))
}

/// `owner_group` slug of one secret — the update path's team fallback.
pub async fn owner_group_of(pool: &DbPool, id: &str) -> Result<String, AppError> {
    let row = exec::fetch_optional(
        pool,
        "SELECT owner_group FROM secrets WHERE id = ?",
        &[id.into()],
    )
    .await?
    .ok_or(sqlx::Error::RowNotFound)?;
    Ok(row.text_of("owner_group"))
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

pub async fn update_facts(pool: &DbPool, id: &str) -> Result<SecretUpdateFacts, AppError> {
    let row = exec::fetch_optional(
        pool,
        "SELECT name, owner_group, team_id, team_shared, kind, value FROM secrets WHERE id = ?",
        &[id.into()],
    )
    .await?
    .ok_or(sqlx::Error::RowNotFound)?;
    Ok(SecretUpdateFacts {
        name: row.text_of("name"),
        owner_group: row.text_of("owner_group"),
        team_id: row.text_of("team_id"),
        team_shared: row.i64_of("team_shared"),
        kind: row.text_of("kind"),
        value: row.text_of("value"),
    })
}

/// Update a secret's metadata only (no value change).
#[allow(clippy::too_many_arguments)]
pub async fn update_metadata(
    pool: &DbPool,
    id: &str,
    owner_group: &str,
    team_id: &str,
    team_shared_i: i64,
    kind: &str,
) -> Result<(), AppError> {
    exec::execute(
        pool,
        "UPDATE secrets SET owner_group = ?, team_id = ?, team_shared = ?, kind = ?, \
             updated_at = datetime('now') WHERE id = ?",
        &[
            owner_group.into(),
            team_id.into(),
            SqlValue::Int(team_shared_i),
            kind.into(),
            id.into(),
        ],
    )
    .await?;
    Ok(())
}

/// Update a secret's value (new ciphertext) and metadata together.
#[allow(clippy::too_many_arguments)]
pub async fn update_value_and_metadata(
    pool: &DbPool,
    id: &str,
    value: &str,
    owner_group: &str,
    team_id: &str,
    team_shared_i: i64,
    kind: &str,
) -> Result<(), AppError> {
    exec::execute(
        pool,
        "UPDATE secrets SET value = ?, owner_group = ?, team_id = ?, team_shared = ?, kind = ?, \
             updated_at = datetime('now') WHERE id = ?",
        &[
            value.into(),
            owner_group.into(),
            team_id.into(),
            SqlValue::Int(team_shared_i),
            kind.into(),
            id.into(),
        ],
    )
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

pub async fn delete_facts(pool: &DbPool, id: &str) -> Result<SecretDeleteFacts, AppError> {
    let row = exec::fetch_optional(
        pool,
        "SELECT name, owner_group, team_id, team_shared, kind FROM secrets WHERE id = ?",
        &[id.into()],
    )
    .await?
    .ok_or(sqlx::Error::RowNotFound)?;
    Ok(SecretDeleteFacts {
        name: row.text_of("name"),
        owner_group: row.text_of("owner_group"),
        team_id: row.text_of("team_id"),
        team_shared: row.i64_of("team_shared"),
        kind: row.text_of("kind"),
    })
}

/// Delete one secret row.
pub async fn delete(pool: &DbPool, id: &str) -> Result<(), AppError> {
    exec::execute(pool, "DELETE FROM secrets WHERE id = ?", &[id.into()]).await?;
    Ok(())
}

/// Display name of one secret (audit + remote-delete calls read this).
pub async fn secret_name(pool: &DbPool, id: &str) -> Result<String, AppError> {
    let row = exec::fetch_optional(pool, "SELECT name FROM secrets WHERE id = ?", &[id.into()])
        .await?
        .ok_or(sqlx::Error::RowNotFound)?;
    Ok(row.text_of("name"))
}

/// Display name of one secret, tolerated absent (`None` = no such row).
pub async fn secret_name_optional(pool: &DbPool, id: &str) -> Result<Option<String>, AppError> {
    let row =
        exec::fetch_optional(pool, "SELECT name FROM secrets WHERE id = ?", &[id.into()]).await?;
    Ok(row.and_then(|r| r.opt_text_of("name")))
}

/// `(name, stored value, team_id)` — the GitHub push path's facts.
pub async fn push_facts(pool: &DbPool, id: &str) -> Result<(String, String, String), AppError> {
    let row = exec::fetch_optional(
        pool,
        "SELECT name, value, team_id FROM secrets WHERE id = ?",
        &[id.into()],
    )
    .await?
    .ok_or(sqlx::Error::RowNotFound)?;
    Ok((
        row.text_of("name"),
        row.text_of("value"),
        row.text_of("team_id"),
    ))
}
