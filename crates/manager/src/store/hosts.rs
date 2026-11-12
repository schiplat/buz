//! SSH hosts: CRUD, the shared/team visibility tiers, and the stored
//! credentials the connection prober decrypts.
//!
//! Visibility: `team_id` NULL/'' is the "shared" tier (everyone sees,
//! admins manage); team-scoped rows are member-only. The list OR-filter
//! can't be expressed by `team_in_clause` (pure AND), so it is built here.

use sqlx::{Row, SqlitePool};

use shared::error::AppError;

/// One host row. `credential_data` / labels are stored verbatim (the value
/// is ciphertext; labels are a JSON array string).
#[derive(Debug, Clone)]
pub struct HostRow {
    pub id: String,
    pub name: String,
    pub host: String,
    pub port: i64,
    pub username: String,
    pub credential_type: String,
    pub credential_data: String,
    pub labels: String,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    pub team_id: Option<String>,
}

fn host_row(r: sqlx::sqlite::SqliteRow) -> HostRow {
    HostRow {
        id: r.get(0),
        name: r.get(1),
        host: r.get(2),
        port: r.get(3),
        username: r.get(4),
        credential_type: r.get(5),
        credential_data: r.get(6),
        labels: r.get(7),
        created_at: r.get(8),
        updated_at: r.get(9),
        team_id: r.get(10),
    }
}

/// Hosts visible to the caller: `None` = platform admin (all rows);
/// `Some([])` = shared tier only; `Some(ids)` = shared tier plus the given
/// teams. Creation-ordered (newest first).
pub async fn list(
    pool: &SqlitePool,
    visible_team_ids: Option<&[String]>,
) -> Result<Vec<HostRow>, AppError> {
    let rows =
        match visible_team_ids {
            None => sqlx::query(
                "SELECT id, name, host, port, username, credential_type, credential_data, labels,
                    created_at, updated_at, team_id
             FROM hosts ORDER BY created_at DESC",
            )
            .fetch_all(pool)
            .await?,
            Some([]) => sqlx::query(
                "SELECT id, name, host, port, username, credential_type, credential_data, labels,
                        created_at, updated_at, team_id
                 FROM hosts WHERE team_id IS NULL OR team_id = ''
                 ORDER BY created_at DESC",
            )
            .fetch_all(pool)
            .await?,
            Some(ids) => {
                let placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
                let sql = format!(
                "SELECT id, name, host, port, username, credential_type, credential_data, labels,
                        created_at, updated_at, team_id
                 FROM hosts
                 WHERE team_id IS NULL OR team_id = '' OR team_id IN ({placeholders})
                 ORDER BY created_at DESC"
            );
                let mut q = sqlx::query(&sql);
                for id in ids {
                    q = q.bind(id);
                }
                q.fetch_all(pool).await?
            }
        };
    Ok(rows.into_iter().map(host_row).collect())
}

/// Everything needed to insert one host. `credential_data` /
/// `credential_passphrase` arrive already encrypted (empty = none stored).
#[allow(clippy::too_many_arguments)]
pub async fn insert(
    pool: &SqlitePool,
    id: &str,
    name: &str,
    host: &str,
    port: i64,
    username: &str,
    credential_type: &str,
    credential_data: &str,
    credential_passphrase: &str,
    labels: &str,
    team_id: Option<&str>,
) -> Result<(), AppError> {
    sqlx::query(
        "INSERT INTO hosts (id, name, host, port, username, credential_type, credential_data,
         credential_passphrase, labels, team_id)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(id)
    .bind(name)
    .bind(host)
    .bind(port)
    .bind(username)
    .bind(credential_type)
    .bind(credential_data)
    .bind(credential_passphrase)
    .bind(labels)
    .bind(team_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Full host row by id (`None` = no such host).
pub async fn find_full_by_id(pool: &SqlitePool, id: &str) -> Result<Option<HostRow>, AppError> {
    let row = sqlx::query(
        "SELECT id, name, host, port, username, credential_type, credential_data, labels,
                created_at, updated_at, team_id
         FROM hosts WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(host_row))
}

/// `(name, host, port, username, credential_type, team_id)` — the audit
/// before/after snapshot and display-name facts. `None` = no such host.
#[derive(Debug, Clone)]
pub struct HostFacts {
    pub name: String,
    pub host: String,
    pub port: i64,
    pub username: String,
    pub credential_type: String,
    pub team_id: Option<String>,
}

pub async fn facts(pool: &SqlitePool, id: &str) -> Result<Option<HostFacts>, AppError> {
    let row = sqlx::query(
        "SELECT name, host, port, username, credential_type, team_id FROM hosts WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| HostFacts {
        name: r.get(0),
        host: r.get(1),
        port: r.get(2),
        username: r.get(3),
        credential_type: r.get(4),
        team_id: r.get(5),
    }))
}

/// Set one host's team (`None` = move to the shared tier).
pub async fn update_team_id(
    pool: &SqlitePool,
    id: &str,
    team_id: Option<&str>,
) -> Result<(), AppError> {
    sqlx::query("UPDATE hosts SET team_id = ?, updated_at = datetime('now') WHERE id = ?")
        .bind(team_id)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Replace one host's labels (JSON array string).
pub async fn update_labels(pool: &SqlitePool, id: &str, labels: &str) -> Result<(), AppError> {
    sqlx::query("UPDATE hosts SET labels = ?, updated_at = datetime('now') WHERE id = ?")
        .bind(labels)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Replace one host's address.
pub async fn update_address(pool: &SqlitePool, id: &str, host: &str) -> Result<(), AppError> {
    sqlx::query("UPDATE hosts SET host = ?, updated_at = datetime('now') WHERE id = ?")
        .bind(host)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Replace one host's SSH port.
pub async fn update_port(pool: &SqlitePool, id: &str, port: i64) -> Result<(), AppError> {
    sqlx::query("UPDATE hosts SET port = ?, updated_at = datetime('now') WHERE id = ?")
        .bind(port)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Replace one host's login username.
pub async fn update_username(pool: &SqlitePool, id: &str, username: &str) -> Result<(), AppError> {
    sqlx::query("UPDATE hosts SET username = ?, updated_at = datetime('now') WHERE id = ?")
        .bind(username)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Replace one host's credential type.
pub async fn update_credential_type(
    pool: &SqlitePool,
    id: &str,
    credential_type: &str,
) -> Result<(), AppError> {
    sqlx::query("UPDATE hosts SET credential_type = ?, updated_at = datetime('now') WHERE id = ?")
        .bind(credential_type)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Replace one host's credential (ciphertext; empty clears it).
pub async fn update_credential_data(
    pool: &SqlitePool,
    id: &str,
    credential_data: &str,
) -> Result<(), AppError> {
    sqlx::query("UPDATE hosts SET credential_data = ?, updated_at = datetime('now') WHERE id = ?")
        .bind(credential_data)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Replace one host's credential passphrase (ciphertext; empty clears it).
pub async fn update_credential_passphrase(
    pool: &SqlitePool,
    id: &str,
    credential_passphrase: &str,
) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE hosts SET credential_passphrase = ?, updated_at = datetime('now') WHERE id = ?",
    )
    .bind(credential_passphrase)
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Replace one host's display name.
pub async fn update_name(pool: &SqlitePool, id: &str, name: &str) -> Result<(), AppError> {
    sqlx::query("UPDATE hosts SET name = ?, updated_at = datetime('now') WHERE id = ?")
        .bind(name)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Delete one host; returns `false` when nothing matched (caller surfaces
/// `NotFound`).
pub async fn delete(pool: &SqlitePool, id: &str) -> Result<bool, AppError> {
    let result = sqlx::query("DELETE FROM hosts WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

/// Stored credentials of one host — the connection prober's decrypt input.
/// `None` = no such host.
#[derive(Debug, Clone)]
pub struct HostCredentials {
    pub credential_type: String,
    pub credential_data: String,
    pub credential_passphrase: String,
}

pub async fn credentials_of(
    pool: &SqlitePool,
    id: &str,
) -> Result<Option<HostCredentials>, AppError> {
    let row = sqlx::query(
        "SELECT credential_type, credential_data, credential_passphrase FROM hosts WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| HostCredentials {
        credential_type: r.get(0),
        credential_data: r.get(1),
        credential_passphrase: r.get(2),
    }))
}

// ───────────────────────── claim-time resolution (team-scoped) ─────────────────────────
//
// Every read here is scoped to the runner's team: a shared (NULL) host row or
// one owned by the same team. Cross-team rows must stay invisible — the claim
// path treats "no row" as "host unavailable", never as a permission error.

/// Id of the host with this name when it is visible to `team_id` (`None` =
/// unknown, or owned by another team).
pub async fn visible_id_by_name(
    pool: &SqlitePool,
    name: &str,
    team_id: Option<&str>,
) -> Result<Option<String>, AppError> {
    let row =
        sqlx::query("SELECT id FROM hosts WHERE name = ? AND (team_id IS NULL OR team_id = ?)")
            .bind(name)
            .bind(team_id)
            .fetch_optional(pool)
            .await?;
    Ok(row.map(|r| r.get(0)))
}

/// Ids of hosts carrying `label` that are visible to `team_id`.
pub async fn visible_ids_by_label(
    pool: &SqlitePool,
    label: &str,
    team_id: Option<&str>,
) -> Result<Vec<String>, AppError> {
    Ok(sqlx::query(
        "SELECT hosts.id FROM hosts, json_each(hosts.labels) \
         WHERE json_each.value = ? AND (hosts.team_id IS NULL OR hosts.team_id = ?)",
    )
    .bind(label)
    .bind(team_id)
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|r| r.get(0))
    .collect())
}

/// One host's push credentials, scoped to `team_id` (shared rows or the same
/// team). `None` = unknown id, or owned by another team.
pub struct HostCredentialsRow {
    pub host: String,
    pub port: i64,
    pub username: String,
    pub credential_type: String,
    pub credential_data: String,
    pub credential_passphrase: String,
}

pub async fn visible_credentials_by_id(
    pool: &SqlitePool,
    id: &str,
    team_id: Option<&str>,
) -> Result<Option<HostCredentialsRow>, AppError> {
    Ok(sqlx::query(
        "SELECT host, port, username, credential_type, credential_data, credential_passphrase
         FROM hosts WHERE id = ? AND (team_id IS NULL OR team_id = ?)",
    )
    .bind(id)
    .bind(team_id)
    .fetch_optional(pool)
    .await?
    .map(|r| HostCredentialsRow {
        host: r.get(0),
        port: r.get(1),
        username: r.get(2),
        credential_type: r.get(3),
        credential_data: r.get(4),
        credential_passphrase: r.get(5),
    }))
}
