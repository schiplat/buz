//! SSH hosts: CRUD, the shared/team visibility tiers, and the stored
//! credentials the connection prober decrypts.
//!
//! Visibility: `team_id` NULL/'' is the "shared" tier (everyone sees,
//! admins manage); team-scoped rows are member-only. The list OR-filter
//! can't be expressed by `team_in_clause` (pure AND), so it is built here.
//!
//! Migration status: on the dual-backend dispatcher (`store::exec`); the
//! label walk carries an explicit per-backend body (`Sql::Dual`).

use crate::store::exec::{self, DbPool, SqlValue};
use crate::store::sql::Sql;

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

fn host_row(r: exec::GenericRow) -> HostRow {
    HostRow {
        id: r.text_of("id"),
        name: r.text_of("name"),
        host: r.text_of("host"),
        port: r.i64_of("port"),
        username: r.text_of("username"),
        credential_type: r.text_of("credential_type"),
        credential_data: r.text_of("credential_data"),
        labels: r.text_of("labels"),
        created_at: r.opt_text_of("created_at"),
        updated_at: r.opt_text_of("updated_at"),
        team_id: r.opt_text_of("team_id"),
    }
}

/// Hosts visible to the caller: `None` = platform admin (all rows);
/// `Some([])` = shared tier only; `Some(ids)` = shared tier plus the given
/// teams. Creation-ordered (newest first).
pub async fn list(
    pool: &DbPool,
    visible_team_ids: Option<&[String]>,
) -> Result<Vec<HostRow>, AppError> {
    let rows =
        match visible_team_ids {
            None => exec::fetch_all(
                pool,
                "SELECT id, name, host, port, username, credential_type, credential_data, labels,
                    created_at, updated_at, team_id
             FROM hosts ORDER BY created_at DESC",
                &[],
            )
            .await?,
            Some([]) => exec::fetch_all(
                pool,
                "SELECT id, name, host, port, username, credential_type, credential_data, labels,
                    created_at, updated_at, team_id
             FROM hosts WHERE team_id IS NULL OR team_id = ''
             ORDER BY created_at DESC",
                &[],
            )
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
                let binds: Vec<SqlValue> = ids.iter().map(|id| id.as_str().into()).collect();
                exec::fetch_all(pool, &sql, &binds).await?
            }
        };
    Ok(rows.into_iter().map(host_row).collect())
}

/// Everything needed to insert one host. `credential_data` /
/// `credential_passphrase` arrive already encrypted (empty = none stored).
#[allow(clippy::too_many_arguments)]
pub async fn insert(
    pool: &DbPool,
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
    exec::execute(
        pool,
        "INSERT INTO hosts (id, name, host, port, username, credential_type, credential_data,
         credential_passphrase, labels, team_id)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        &[
            id.into(),
            name.into(),
            host.into(),
            SqlValue::Int(port),
            username.into(),
            credential_type.into(),
            credential_data.into(),
            credential_passphrase.into(),
            labels.into(),
            team_id.into(),
        ],
    )
    .await?;
    Ok(())
}

/// Full host row by id (`None` = no such host).
pub async fn find_full_by_id(pool: &DbPool, id: &str) -> Result<Option<HostRow>, AppError> {
    Ok(exec::fetch_optional(
        pool,
        "SELECT id, name, host, port, username, credential_type, credential_data, labels,
                created_at, updated_at, team_id
         FROM hosts WHERE id = ?",
        &[id.into()],
    )
    .await?
    .map(host_row))
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

pub async fn facts(pool: &DbPool, id: &str) -> Result<Option<HostFacts>, AppError> {
    Ok(exec::fetch_optional(
        pool,
        "SELECT name, host, port, username, credential_type, team_id FROM hosts WHERE id = ?",
        &[id.into()],
    )
    .await?
    .map(|r| HostFacts {
        name: r.text_of("name"),
        host: r.text_of("host"),
        port: r.i64_of("port"),
        username: r.text_of("username"),
        credential_type: r.text_of("credential_type"),
        team_id: r.opt_text_of("team_id"),
    }))
}

/// Set one host's team (`None` = move to the shared tier).
pub async fn update_team_id(
    pool: &DbPool,
    id: &str,
    team_id: Option<&str>,
) -> Result<(), AppError> {
    exec::execute(
        pool,
        "UPDATE hosts SET team_id = ?, updated_at = datetime('now') WHERE id = ?",
        &[team_id.into(), id.into()],
    )
    .await?;
    Ok(())
}

/// Replace one host's labels (JSON array string).
pub async fn update_labels(pool: &DbPool, id: &str, labels: &str) -> Result<(), AppError> {
    exec::execute(
        pool,
        "UPDATE hosts SET labels = ?, updated_at = datetime('now') WHERE id = ?",
        &[labels.into(), id.into()],
    )
    .await?;
    Ok(())
}

/// Replace one host's address.
pub async fn update_address(pool: &DbPool, id: &str, host: &str) -> Result<(), AppError> {
    exec::execute(
        pool,
        "UPDATE hosts SET host = ?, updated_at = datetime('now') WHERE id = ?",
        &[host.into(), id.into()],
    )
    .await?;
    Ok(())
}

/// Replace one host's SSH port.
pub async fn update_port(pool: &DbPool, id: &str, port: i64) -> Result<(), AppError> {
    exec::execute(
        pool,
        "UPDATE hosts SET port = ?, updated_at = datetime('now') WHERE id = ?",
        &[SqlValue::Int(port), id.into()],
    )
    .await?;
    Ok(())
}

/// Replace one host's login username.
pub async fn update_username(pool: &DbPool, id: &str, username: &str) -> Result<(), AppError> {
    exec::execute(
        pool,
        "UPDATE hosts SET username = ?, updated_at = datetime('now') WHERE id = ?",
        &[username.into(), id.into()],
    )
    .await?;
    Ok(())
}

/// Replace one host's credential type.
pub async fn update_credential_type(
    pool: &DbPool,
    id: &str,
    credential_type: &str,
) -> Result<(), AppError> {
    exec::execute(
        pool,
        "UPDATE hosts SET credential_type = ?, updated_at = datetime('now') WHERE id = ?",
        &[credential_type.into(), id.into()],
    )
    .await?;
    Ok(())
}

/// Replace one host's credential (ciphertext; empty clears it).
pub async fn update_credential_data(
    pool: &DbPool,
    id: &str,
    credential_data: &str,
) -> Result<(), AppError> {
    exec::execute(
        pool,
        "UPDATE hosts SET credential_data = ?, updated_at = datetime('now') WHERE id = ?",
        &[credential_data.into(), id.into()],
    )
    .await?;
    Ok(())
}

/// Replace one host's credential passphrase (ciphertext; empty clears it).
pub async fn update_credential_passphrase(
    pool: &DbPool,
    id: &str,
    credential_passphrase: &str,
) -> Result<(), AppError> {
    exec::execute(
        pool,
        "UPDATE hosts SET credential_passphrase = ?, updated_at = datetime('now') WHERE id = ?",
        &[credential_passphrase.into(), id.into()],
    )
    .await?;
    Ok(())
}

/// Replace one host's display name.
pub async fn update_name(pool: &DbPool, id: &str, name: &str) -> Result<(), AppError> {
    exec::execute(
        pool,
        "UPDATE hosts SET name = ?, updated_at = datetime('now') WHERE id = ?",
        &[name.into(), id.into()],
    )
    .await?;
    Ok(())
}

/// Delete one host; returns `false` when nothing matched (caller surfaces
/// `NotFound`).
pub async fn delete(pool: &DbPool, id: &str) -> Result<bool, AppError> {
    let affected = exec::execute(pool, "DELETE FROM hosts WHERE id = ?", &[id.into()]).await?;
    Ok(affected > 0)
}

/// Stored team id of one host. `None` = no such host; the inner `None` = a
/// NULL team_id (unassigned, distinct from the empty string).
pub async fn team_id_of(pool: &DbPool, host_id: &str) -> Result<Option<Option<String>>, AppError> {
    Ok(exec::fetch_optional(
        pool,
        "SELECT team_id FROM hosts WHERE id = ?",
        &[host_id.into()],
    )
    .await?
    .map(|r| r.opt_text_of("team_id")))
}

/// Stored credentials of one host — the connection prober's decrypt input.
/// `None` = no such host.
#[derive(Debug, Clone)]
pub struct HostCredentials {
    pub credential_type: String,
    pub credential_data: String,
    pub credential_passphrase: String,
}

pub async fn credentials_of(pool: &DbPool, id: &str) -> Result<Option<HostCredentials>, AppError> {
    Ok(exec::fetch_optional(
        pool,
        "SELECT credential_type, credential_data, credential_passphrase FROM hosts WHERE id = ?",
        &[id.into()],
    )
    .await?
    .map(|r| HostCredentials {
        credential_type: r.text_of("credential_type"),
        credential_data: r.text_of("credential_data"),
        credential_passphrase: r.text_of("credential_passphrase"),
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
    pool: &DbPool,
    name: &str,
    team_id: Option<&str>,
) -> Result<Option<String>, AppError> {
    Ok(exec::fetch_optional(
        pool,
        "SELECT id FROM hosts WHERE name = ? AND (team_id IS NULL OR team_id = ?)",
        &[name.into(), team_id.into()],
    )
    .await?
    .map(|r| r.text_of("id")))
}

/// Ids of hosts carrying `label` that are visible to `team_id`.
///
/// The label walk is an explicit two-dialect statement: `json_each` on
/// SQLite, `jsonb_array_elements_text` on Postgres (see `store::sql`).
pub async fn visible_ids_by_label(
    pool: &DbPool,
    label: &str,
    team_id: Option<&str>,
) -> Result<Vec<String>, AppError> {
    const SQL: Sql<'static> = Sql::Dual {
        sqlite: "SELECT hosts.id FROM hosts, json_each(hosts.labels) \
                 WHERE json_each.value = ? AND (hosts.team_id IS NULL OR hosts.team_id = ?)",
        pg: "SELECT h.id FROM hosts h, jsonb_array_elements_text(h.labels::jsonb) AS label \
             WHERE label = $1 AND (h.team_id IS NULL OR h.team_id = $2)",
    };
    Ok(
        exec::fetch_all_sql(pool, SQL, &[label.into(), team_id.into()])
            .await?
            .into_iter()
            .map(|r| r.text(0))
            .collect(),
    )
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
    pool: &DbPool,
    id: &str,
    team_id: Option<&str>,
) -> Result<Option<HostCredentialsRow>, AppError> {
    Ok(exec::fetch_optional(
        pool,
        "SELECT host, port, username, credential_type, credential_data, credential_passphrase
         FROM hosts WHERE id = ? AND (team_id IS NULL OR team_id = ?)",
        &[id.into(), team_id.into()],
    )
    .await?
    .map(|r| HostCredentialsRow {
        host: r.text_of("host"),
        port: r.i64_of("port"),
        username: r.text_of("username"),
        credential_type: r.text_of("credential_type"),
        credential_data: r.text_of("credential_data"),
        credential_passphrase: r.text_of("credential_passphrase"),
    }))
}
