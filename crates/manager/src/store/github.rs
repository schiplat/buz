//! GitHub sync connections: the admin-managed rows that hold a repo host's API
//! base URLsame and its encrypted access token.
//!
//! Token encryption stays in the handler: the store takes ciphertext and never
//! sees a plaintext token.
//!
//! On the dual-backend dispatcher (`store::exec`): statements travel as plain
//! SQLite-dialect strings with backend-neutral binds, rows decode by column
//! name.

use crate::store::exec::{self, DbPool};
use uuid::Uuid;

use shared::error::AppError;

/// One connection row, shaped for the admin API.
pub struct ConnectionRow {
    pub id: String,
    pub name: String,
    pub api_base_url: String,
    pub token_ciphertext: String,
    pub fetch_owners: String,
    pub fetch_interval_minutes: Option<i64>,
    pub last_fetch_at: Option<String>,
    pub updated_at: Option<String>,
}

impl ConnectionRow {
    pub fn has_token(&self) -> bool {
        !self.token_ciphertext.is_empty()
    }
}

fn connection_row(r: crate::store::exec::GenericRow) -> ConnectionRow {
    ConnectionRow {
        id: r.text_of("id"),
        name: r.text_of("name"),
        api_base_url: r.text_of("api_base_url"),
        token_ciphertext: r.text_of("token_ciphertext"),
        fetch_owners: r.text_of("fetch_owners"),
        fetch_interval_minutes: r.opt_i64_of("fetch_interval_minutes"),
        last_fetch_at: r.opt_text_of("last_fetch_at"),
        updated_at: r.opt_text_of("updated_at"),
    }
}

const CONNECTION_COLUMNS: &str =
    "id, name, api_base_url, token_ciphertext, fetch_owners, fetch_interval_minutes, last_fetch_at, updated_at";

/// Every connection, ordered by display name.
pub async fn list_connections(pool: &DbPool) -> Result<Vec<ConnectionRow>, AppError> {
    let rows = exec::fetch_all(
        pool,
        &format!("SELECT {CONNECTION_COLUMNS} FROM github_connections ORDER BY name ASC"),
        &[],
    )
    .await?;
    Ok(rows.into_iter().map(connection_row).collect())
}

/// One connection by id (`None` when absent).
pub async fn connection(pool: &DbPool, id: &str) -> Result<Option<ConnectionRow>, AppError> {
    let row = exec::fetch_optional(
        pool,
        &format!("SELECT {CONNECTION_COLUMNS} FROM github_connections WHERE id = ?"),
        &[id.into()],
    )
    .await?;
    Ok(row.map(connection_row))
}

/// True when another connection already uses `name` (`except_id` = the row
/// being updated, which may keep its own name).
pub async fn name_taken(
    pool: &DbPool,
    name: &str,
    except_id: Option<&str>,
) -> Result<bool, AppError> {
    let found = match except_id {
        Some(id) => {
            exec::fetch_optional(
                pool,
                "SELECT id FROM github_connections WHERE name = ? AND id != ?",
                &[name.into(), id.into()],
            )
            .await?
        }
        None => {
            exec::fetch_optional(
                pool,
                "SELECT id FROM github_connections WHERE name = ?",
                &[name.into()],
            )
            .await?
        }
    };
    Ok(found.is_some())
}

/// Insert a connection. `token_ciphertext` is already encrypted.
pub async fn insert_connection(
    pool: &DbPool,
    name: &str,
    api_base_url: &str,
    token_ciphertext: &str,
    fetch_owners: &str,
    fetch_interval_minutes: Option<i64>,
) -> Result<String, AppError> {
    let id = Uuid::new_v4().to_string();
    exec::execute(
        pool,
        "INSERT INTO github_connections
            (id, name, api_base_url, token_ciphertext, fetch_owners, fetch_interval_minutes)
         VALUES (?, ?, ?, ?, ?, ?)",
        &[
            id.as_str().into(),
            name.into(),
            api_base_url.into(),
            token_ciphertext.into(),
            fetch_owners.into(),
            fetch_interval_minutes.into(),
        ],
    )
    .await?;
    Ok(id)
}

/// Overwrite every editable field. `token_ciphertext = None` keeps the stored
/// token (the API treats an absent token identically to an unchanged one).
#[allow(clippy::too_many_arguments)] // one row, every editable column
pub async fn update_connection(
    pool: &DbPool,
    id: &str,
    name: &str,
    api_base_url: &str,
    token_ciphertext: Option<&str>,
    fetch_owners: &str,
    fetch_interval_minutes: Option<i64>,
) -> Result<(), AppError> {
    match token_ciphertext {
        Some(cipher) => {
            exec::execute(
                pool,
                "UPDATE github_connections SET name = ?, api_base_url = ?, token_ciphertext = ?, fetch_owners = ?, fetch_interval_minutes = ?, updated_at = datetime('now') WHERE id = ?",
                &[
                    name.into(),
                    api_base_url.into(),
                    cipher.into(),
                    fetch_owners.into(),
                    fetch_interval_minutes.into(),
                    id.into(),
                ],
            )
            .await?;
        }
        None => {
            exec::execute(
                pool,
                "UPDATE github_connections SET name = ?, api_base_url = ?, fetch_owners = ?, fetch_interval_minutes = ?, updated_at = datetime('now') WHERE id = ?",
                &[
                    name.into(),
                    api_base_url.into(),
                    fetch_owners.into(),
                    fetch_interval_minutes.into(),
                    id.into(),
                ],
            )
            .await?;
        }
    }
    Ok(())
}

/// Delete one connection; `false` when it did not exist.
pub async fn delete_connection(pool: &DbPool, id: &str) -> Result<bool, AppError> {
    let n = exec::execute(
        pool,
        "DELETE FROM github_connections WHERE id = ?",
        &[id.into()],
    )
    .await?;
    Ok(n != 0)
}

/// Connections that have a fetch owner and an interval — the scheduler's work
/// list.
pub async fn fetchable_connections(
    pool: &DbPool,
) -> Result<Vec<(String, String, Option<i64>)>, AppError> {
    let rows = exec::fetch_all(
        pool,
        "SELECT id, name, fetch_interval_minutes FROM github_connections
         WHERE fetch_owners != '' AND fetch_interval_minutes IS NOT NULL
         ORDER BY name ASC",
        &[],
    )
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| (r.text(0), r.text(1), r.opt_i64(2)))
        .collect())
}

/// Stamp `last_fetch_at` after a fetch round.
pub async fn mark_fetched(pool: &DbPool, id: &str) -> Result<(), AppError> {
    exec::execute(
        pool,
        "UPDATE github_connections SET last_fetch_at = datetime('now') WHERE id = ?",
        &[id.into()],
    )
    .await?;
    Ok(())
}

/// A synced repository row (the `github_repos` projection).
pub struct RepoRow {
    pub id: String,
    pub connection_id: String,
    pub owner: String,
    pub repo: String,
    pub updated_at: Option<String>,
    pub created_at: Option<String>,
}

pub const REPO_COLUMNS: &str = "id, connection_id, owner, repo, updated_at, created_at";

pub fn repo_from_row(r: crate::store::exec::GenericRow) -> RepoRow {
    RepoRow {
        id: r.text_of("id"),
        connection_id: r.text_of("connection_id"),
        owner: r.text_of("owner"),
        repo: r.text_of("repo"),
        updated_at: r.opt_text_of("updated_at"),
        created_at: r.opt_text_of("created_at"),
    }
}

pub async fn list_repos(pool: &DbPool) -> Result<Vec<RepoRow>, AppError> {
    let rows = exec::fetch_all(
        pool,
        &format!("SELECT {REPO_COLUMNS} FROM github_repos ORDER BY owner ASC, repo ASC"),
        &[],
    )
    .await?;
    Ok(rows.into_iter().map(repo_from_row).collect())
}

pub async fn repo_by_id(pool: &DbPool, id: &str) -> Result<Option<RepoRow>, AppError> {
    let row = exec::fetch_optional(
        pool,
        &format!("SELECT {REPO_COLUMNS} FROM github_repos WHERE id = ?"),
        &[id.into()],
    )
    .await?;
    Ok(row.map(repo_from_row))
}

pub async fn repo_exists(
    pool: &DbPool,
    connection_id: &str,
    owner: &str,
    repo: &str,
    except_id: Option<&str>,
) -> Result<bool, AppError> {
    let found = match except_id {
        Some(id) => {
            exec::fetch_optional(
                pool,
                "SELECT id FROM github_repos WHERE connection_id = ? AND owner = ? AND repo = ? AND id != ?",
                &[connection_id.into(), owner.into(), repo.into(), id.into()],
            )
            .await?
        }
        None => {
            exec::fetch_optional(
                pool,
                "SELECT id FROM github_repos WHERE connection_id = ? AND owner = ? AND repo = ?",
                &[connection_id.into(), owner.into(), repo.into()],
            )
            .await?
        }
    };
    Ok(found.is_some())
}

pub async fn insert_repo(
    pool: &DbPool,
    connection_id: &str,
    owner: &str,
    repo: &str,
) -> Result<String, AppError> {
    let id = Uuid::new_v4().to_string();
    exec::execute(
        pool,
        "INSERT INTO github_repos (id, connection_id, owner, repo) VALUES (?, ?, ?, ?)",
        &[
            id.as_str().into(),
            connection_id.into(),
            owner.into(),
            repo.into(),
        ],
    )
    .await?;
    Ok(id)
}

pub async fn update_repo(pool: &DbPool, id: &str, owner: &str, repo: &str) -> Result<(), AppError> {
    exec::execute(
        pool,
        "UPDATE github_repos SET owner = ?, repo = ?, updated_at = datetime('now') WHERE id = ?",
        &[owner.into(), repo.into(), id.into()],
    )
    .await?;
    Ok(())
}

pub async fn delete_repo(pool: &DbPool, id: &str) -> Result<bool, AppError> {
    let n = exec::execute(pool, "DELETE FROM github_repos WHERE id = ?", &[id.into()]).await?;
    Ok(n != 0)
}

/// `(name, api_base_url)` of one connection — the join columns a repo view needs.
pub async fn connection_name_and_base(
    pool: &DbPool,
    id: &str,
) -> Result<Option<(String, String)>, AppError> {
    let row = exec::fetch_optional(
        pool,
        "SELECT name, api_base_url FROM github_connections WHERE id = ?",
        &[id.into()],
    )
    .await?;
    Ok(row.map(|r| (r.text(0), r.text(1))))
}

/// Repos with their connection's display name and API base — the repos-list
/// join (`github_repos` × `github_connections`), owner/repo-ordered.
pub struct RepoListRow {
    pub id: String,
    pub connection_id: String,
    pub connection_name: String,
    pub owner: String,
    pub repo: String,
    pub updated_at: Option<String>,
    pub api_base_url: String,
}

pub async fn list_repos_with_connection(pool: &DbPool) -> Result<Vec<RepoListRow>, AppError> {
    let rows = exec::fetch_all(
        pool,
        "SELECT r.id, r.connection_id, c.name, r.owner, r.repo, r.updated_at, c.api_base_url
         FROM github_repos r
         JOIN github_connections c ON c.id = r.connection_id
         ORDER BY r.owner ASC, r.repo ASC",
        &[],
    )
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| RepoListRow {
            id: r.text_of("id"),
            connection_id: r.text_of("connection_id"),
            connection_name: r.text_of("name"),
            owner: r.text_of("owner"),
            repo: r.text_of("repo"),
            updated_at: r.opt_text_of("updated_at"),
            api_base_url: r.text_of("api_base_url"),
        })
        .collect())
}
