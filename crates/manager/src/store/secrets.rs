//! Secret CRUD (platform-level `${secrets.NAME}` credentials).
//!
//! Values are stored AES-encrypted (`crypto::encrypt`, JWT-secret keyed);
//! the store never sees plaintext except as a ciphertext argument. The
//! list/read rows carry the stored value verbatim — decryption, masking and
//! the `reencrypt_required` fallback are API-display concerns.
//!
//! `team_id` is the only stored owner identity; `owner_group` on the row is
//! the team slug derived by joining `teams`, kept for API/audit compatibility.
//! Names are unique per owner team, not globally.
//!
//! On the dual-backend dispatcher (`store::exec`): statements travel as plain
//! SQLite-dialect strings with backend-neutral binds, rows decode by column
//! name. Strict one-row reads keep `fetch_one` semantics via
//! `ok_or(sqlx::Error::RowNotFound)`.

use crate::store::exec::{self, DbPool, SqlValue};

use shared::error::AppError;

/// One secret row as stored. `value` is the ciphertext; `share_with_team` is
/// the raw 0/1 opt-in that exposes the secret to every pipeline in its owner
/// team; `owner_group` is the derived team slug.
#[derive(Debug, Clone)]
pub struct SecretRow {
    pub id: String,
    pub name: String,
    pub value: String,
    pub owner_group: String,
    pub team_id: String,
    pub share_with_team: i64,
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
        share_with_team: r.i64_of("share_with_team"),
        kind: r.text_of("kind"),
        updated_at: r.opt_text_of("updated_at"),
    }
}

const SELECT_SECRET: &str = "SELECT s.id, s.name, s.value, COALESCE(t.slug, '') AS owner_group, \
     s.team_id, s.share_with_team, s.kind, s.updated_at \
     FROM secrets s LEFT JOIN teams t ON t.id = s.team_id";

/// All secrets, name-ordered. `visible_team_ids = None` is unrestricted
/// (super admin); `Some([])` yields nothing (the caller's team filter is
/// unsatisfiable).
pub async fn list(
    pool: &DbPool,
    visible_team_ids: Option<&[String]>,
) -> Result<Vec<SecretRow>, AppError> {
    let Some(ids) = visible_team_ids else {
        let rows =
            exec::fetch_all(pool, &format!("{SELECT_SECRET} ORDER BY s.name ASC"), &[]).await?;
        return Ok(rows.into_iter().map(secret_row).collect());
    };
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let sql = format!("{SELECT_SECRET} WHERE s.team_id IN ({placeholders}) ORDER BY s.name ASC");
    let binds: Vec<SqlValue> = ids.iter().map(|id| id.as_str().into()).collect();
    let rows = exec::fetch_all(pool, &sql, &binds).await?;
    Ok(rows.into_iter().map(secret_row).collect())
}

/// Id of the secret with this name in this team — the create-time duplicate
/// check (`(team_id, name)` is the unique key).
pub async fn find_id_by_name(
    pool: &DbPool,
    team_id: &str,
    name: &str,
) -> Result<Option<String>, AppError> {
    let row = exec::fetch_optional(
        pool,
        "SELECT id FROM secrets WHERE team_id = ? AND name = ?",
        &[team_id.into(), name.into()],
    )
    .await?;
    Ok(row.map(|r| r.text_of("id")))
}

/// Insert one secret. `value` is the ciphertext; `share_with_team_i` is the
/// raw 0/1 column value.
#[allow(clippy::too_many_arguments)]
pub async fn insert(
    pool: &DbPool,
    id: &str,
    name: &str,
    value: &str,
    team_id: &str,
    share_with_team_i: i64,
    kind: &str,
) -> Result<(), AppError> {
    exec::execute(
        pool,
        "INSERT INTO secrets (id, name, value, team_id, share_with_team, kind) \
         VALUES (?, ?, ?, ?, ?, ?)",
        &[
            id.into(),
            name.into(),
            value.into(),
            team_id.into(),
            SqlValue::Int(share_with_team_i),
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
        &format!("{SELECT_SECRET} WHERE s.id = ?"),
        &[id.into()],
    )
    .await?;
    Ok(row.map(secret_row))
}

/// `owner_group` slug of one secret (derived from its team).
pub async fn owner_group_of(pool: &DbPool, id: &str) -> Result<String, AppError> {
    let row = exec::fetch_optional(
        pool,
        "SELECT COALESCE(t.slug, '') AS owner_group
         FROM secrets s LEFT JOIN teams t ON t.id = s.team_id WHERE s.id = ?",
        &[id.into()],
    )
    .await?
    .ok_or(sqlx::Error::RowNotFound)?;
    Ok(row.text_of("owner_group"))
}

/// Pre-update facts: name, derived owner_group, team_id, share_with_team (raw
/// 0/1), kind and the stored (encrypted) value — the audit before-snapshot and
/// the changed-value detection read these. `None` = no such secret.
pub struct SecretUpdateFacts {
    pub name: String,
    pub owner_group: String,
    pub team_id: String,
    pub share_with_team: i64,
    pub kind: String,
    pub value: String,
}

pub async fn update_facts(pool: &DbPool, id: &str) -> Result<SecretUpdateFacts, AppError> {
    let row = exec::fetch_optional(
        pool,
        "SELECT s.name, COALESCE(t.slug, '') AS owner_group, s.team_id,
                s.share_with_team, s.kind, s.value
         FROM secrets s LEFT JOIN teams t ON t.id = s.team_id WHERE s.id = ?",
        &[id.into()],
    )
    .await?
    .ok_or(sqlx::Error::RowNotFound)?;
    Ok(SecretUpdateFacts {
        name: row.text_of("name"),
        owner_group: row.text_of("owner_group"),
        team_id: row.text_of("team_id"),
        share_with_team: row.i64_of("share_with_team"),
        kind: row.text_of("kind"),
        value: row.text_of("value"),
    })
}

/// Update a secret's metadata only (no value change).
#[allow(clippy::too_many_arguments)]
pub async fn update_metadata(
    pool: &DbPool,
    id: &str,
    team_id: &str,
    share_with_team_i: i64,
    kind: &str,
) -> Result<(), AppError> {
    exec::execute(
        pool,
        "UPDATE secrets SET team_id = ?, share_with_team = ?, kind = ?, \
             updated_at = datetime('now') WHERE id = ?",
        &[
            team_id.into(),
            SqlValue::Int(share_with_team_i),
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
    team_id: &str,
    share_with_team_i: i64,
    kind: &str,
) -> Result<(), AppError> {
    exec::execute(
        pool,
        "UPDATE secrets SET value = ?, team_id = ?, share_with_team = ?, kind = ?, \
             updated_at = datetime('now') WHERE id = ?",
        &[
            value.into(),
            team_id.into(),
            SqlValue::Int(share_with_team_i),
            kind.into(),
            id.into(),
        ],
    )
    .await?;
    Ok(())
}

/// Pre-delete facts: name, derived owner_group, team_id, share_with_team (raw
/// 0/1), kind — the audit before-snapshot. `None` = no such secret.
pub struct SecretDeleteFacts {
    pub name: String,
    pub owner_group: String,
    pub team_id: String,
    pub share_with_team: i64,
    pub kind: String,
}

pub async fn delete_facts(pool: &DbPool, id: &str) -> Result<SecretDeleteFacts, AppError> {
    let row = exec::fetch_optional(
        pool,
        "SELECT s.name, COALESCE(t.slug, '') AS owner_group, s.team_id,
                s.share_with_team, s.kind
         FROM secrets s LEFT JOIN teams t ON t.id = s.team_id WHERE s.id = ?",
        &[id.into()],
    )
    .await?
    .ok_or(sqlx::Error::RowNotFound)?;
    Ok(SecretDeleteFacts {
        name: row.text_of("name"),
        owner_group: row.text_of("owner_group"),
        team_id: row.text_of("team_id"),
        share_with_team: row.i64_of("share_with_team"),
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

/// `(name, kind)` of secrets still on a retired kind (deprecated-key scan).
pub async fn deprecated_secret_kinds(pool: &DbPool) -> Result<Vec<(String, String)>, AppError> {
    Ok(exec::fetch_all(
        pool,
        "SELECT name, kind FROM secrets WHERE kind IN ('registry', 'aliyun_oss_auth') ORDER BY name",
        &[],
    )
    .await?
    .into_iter()
    .map(|r| (r.text(0), r.text(1)))
    .collect())
}

/// Which of these names exist in the team — the YAML `secrets:` check.
pub async fn existing_names_in_team(
    pool: &DbPool,
    team_id: &str,
    names: &[String],
) -> Result<std::collections::HashSet<String>, AppError> {
    if names.is_empty() {
        return Ok(std::collections::HashSet::new());
    }
    let placeholders = vec!["?"; names.len()].join(",");
    let sql = format!("SELECT name FROM secrets WHERE team_id = ? AND name IN ({placeholders})");
    let mut binds: Vec<SqlValue> = vec![team_id.into()];
    binds.extend(names.iter().map(|n| SqlValue::from(n.as_str())));
    Ok(exec::fetch_all(pool, &sql, &binds)
        .await?
        .into_iter()
        .map(|r| r.text(0))
        .collect())
}

/// Stored team id of one secret (`None` = no such secret).
pub async fn team_id_of(pool: &DbPool, secret_id: &str) -> Result<Option<String>, AppError> {
    Ok(exec::fetch_optional(
        pool,
        "SELECT team_id FROM secrets WHERE id = ?",
        &[secret_id.into()],
    )
    .await?
    .map(|r| r.text(0)))
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

/// Pipelines currently bound to this secret (name-ordered) — the team-move
/// guard. Any binding means the move would silently break those pipelines at
/// claim time (the runtime join requires the secret and pipeline to share a
/// team), so the caller must unbind first.
pub async fn bound_pipeline_names(pool: &DbPool, secret_id: &str) -> Result<Vec<String>, AppError> {
    Ok(exec::fetch_all(
        pool,
        "SELECT p.name
         FROM pipeline_secrets ps
         JOIN pipelines p ON p.id = ps.pipeline_id
         WHERE ps.secret_id = ?
         ORDER BY p.name ASC",
        &[secret_id.into()],
    )
    .await?
    .into_iter()
    .map(|r| r.text(0))
    .collect())
}
