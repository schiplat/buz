//! GitHub sync connections: the admin-managed rows that hold a repo host's API
//! base URLsame and its encrypted access token.
//!
//! Token encryption stays in the handler: the store takes ciphertext and never
//! sees a plaintext token.

use sqlx::{Row, SqlitePool};
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

const CONNECTION_COLUMNS: &str =
    "id, name, api_base_url, token_ciphertext, fetch_owners, fetch_interval_minutes, last_fetch_at, updated_at";

/// Every connection, ordered by display name.
pub async fn list_connections(pool: &SqlitePool) -> Result<Vec<ConnectionRow>, AppError> {
    let rows = sqlx::query(&format!(
        "SELECT {CONNECTION_COLUMNS} FROM github_connections ORDER BY name ASC"
    ))
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| ConnectionRow {
            id: r.get(0),
            name: r.get(1),
            api_base_url: r.get(2),
            token_ciphertext: r.get(3),
            fetch_owners: r.get(4),
            fetch_interval_minutes: r.get(5),
            last_fetch_at: r.get(6),
            updated_at: r.get(7),
        })
        .collect())
}

/// One connection by id (`None` when absent).
pub async fn connection(pool: &SqlitePool, id: &str) -> Result<Option<ConnectionRow>, AppError> {
    let row = sqlx::query(&format!(
        "SELECT {CONNECTION_COLUMNS} FROM github_connections WHERE id = ?"
    ))
    .bind(id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| ConnectionRow {
        id: r.get(0),
        name: r.get(1),
        api_base_url: r.get(2),
        token_ciphertext: r.get(3),
        fetch_owners: r.get(4),
        fetch_interval_minutes: r.get(5),
        last_fetch_at: r.get(6),
        updated_at: r.get(7),
    }))
}

/// True when another connection already uses `name` (`except_id` = the row
/// being updated, which may keep its own name).
pub async fn name_taken(
    pool: &SqlitePool,
    name: &str,
    except_id: Option<&str>,
) -> Result<bool, AppError> {
    let found: Option<String> = match except_id {
        Some(id) => {
            sqlx::query_scalar("SELECT id FROM github_connections WHERE name = ? AND id != ?")
                .bind(name)
                .bind(id)
                .fetch_optional(pool)
                .await?
        }
        None => {
            sqlx::query_scalar("SELECT id FROM github_connections WHERE name = ?")
                .bind(name)
                .fetch_optional(pool)
                .await?
        }
    };
    Ok(found.is_some())
}

/// Insert a connection. `token_ciphertext` is already encrypted.
pub async fn insert_connection(
    pool: &SqlitePool,
    name: &str,
    api_base_url: &str,
    token_ciphertext: &str,
    fetch_owners: &str,
    fetch_interval_minutes: Option<i64>,
) -> Result<String, AppError> {
    let id = Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO github_connections
            (id, name, api_base_url, token_ciphertext, fetch_owners, fetch_interval_minutes)
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(name)
    .bind(api_base_url)
    .bind(token_ciphertext)
    .bind(fetch_owners)
    .bind(fetch_interval_minutes)
    .execute(pool)
    .await?;
    Ok(id)
}

/// Overwrite every editable field. `token_ciphertext = None` keeps the stored
/// token (the API treats an absent token identically to an unchanged one).
#[allow(clippy::too_many_arguments)] // one row, every editable column
pub async fn update_connection(
    pool: &SqlitePool,
    id: &str,
    name: &str,
    api_base_url: &str,
    token_ciphertext: Option<&str>,
    fetch_owners: &str,
    fetch_interval_minutes: Option<i64>,
) -> Result<(), AppError> {
    match token_ciphertext {
        Some(cipher) => {
            sqlx::query(
                "UPDATE github_connections SET name = ?, api_base_url = ?, token_ciphertext = ?, fetch_owners = ?, fetch_interval_minutes = ?, updated_at = datetime('now') WHERE id = ?",
            )
            .bind(name)
            .bind(api_base_url)
            .bind(cipher)
            .bind(fetch_owners)
            .bind(fetch_interval_minutes)
            .bind(id)
            .execute(pool)
            .await?;
        }
        None => {
            sqlx::query(
                "UPDATE github_connections SET name = ?, api_base_url = ?, fetch_owners = ?, fetch_interval_minutes = ?, updated_at = datetime('now') WHERE id = ?",
            )
            .bind(name)
            .bind(api_base_url)
            .bind(fetch_owners)
            .bind(fetch_interval_minutes)
            .bind(id)
            .execute(pool)
            .await?;
        }
    }
    Ok(())
}

/// Delete one connection; `false` when it did not exist.
pub async fn delete_connection(pool: &SqlitePool, id: &str) -> Result<bool, AppError> {
    let result = sqlx::query("DELETE FROM github_connections WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() != 0)
}

/// Connections that have a fetch owner and an interval — the scheduler's work
/// list.
pub async fn fetchable_connections(
    pool: &SqlitePool,
) -> Result<Vec<(String, String, Option<i64>)>, AppError> {
    let rows = sqlx::query(
        "SELECT id, name, fetch_interval_minutes FROM github_connections
         WHERE fetch_owners != '' AND fetch_interval_minutes IS NOT NULL
         ORDER BY name ASC",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| (r.get(0), r.get(1), r.get(2)))
        .collect())
}

/// Stamp `last_fetch_at` after a fetch round.
pub async fn mark_fetched(pool: &SqlitePool, id: &str) -> Result<(), AppError> {
    sqlx::query("UPDATE github_connections SET last_fetch_at = datetime('now') WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/// A synced repository row (the `github_repos` projection).
pub struct RepoRow {
    pub id: String,
    pub connection_id: String,
    pub owner: String,
    pub repo: String,
    pub pipeline_id: Option<String>,
    pub updated_at: Option<String>,
    pub created_at: Option<String>,
}

pub const REPO_COLUMNS: &str =
    "id, connection_id, owner, repo, pipeline_id, updated_at, created_at";

pub fn repo_from_row(r: &sqlx::sqlite::SqliteRow) -> RepoRow {
    RepoRow {
        id: r.get(0),
        connection_id: r.get(1),
        owner: r.get(2),
        repo: r.get(3),
        pipeline_id: r.get(4),
        updated_at: r.get(5),
        created_at: r.get(6),
    }
}

pub async fn list_repos(pool: &SqlitePool) -> Result<Vec<RepoRow>, AppError> {
    let rows = sqlx::query(&format!(
        "SELECT {REPO_COLUMNS} FROM github_repos ORDER BY owner ASC, repo ASC"
    ))
    .fetch_all(pool)
    .await?;
    Ok(rows.iter().map(repo_from_row).collect())
}

pub async fn repo_by_id(pool: &SqlitePool, id: &str) -> Result<Option<RepoRow>, AppError> {
    let row = sqlx::query(&format!(
        "SELECT {REPO_COLUMNS} FROM github_repos WHERE id = ?"
    ))
    .bind(id)
    .fetch_optional(pool)
    .await?;
    Ok(row.as_ref().map(repo_from_row))
}

pub async fn repo_exists(
    pool: &SqlitePool,
    connection_id: &str,
    owner: &str,
    repo: &str,
    except_id: Option<&str>,
) -> Result<bool, AppError> {
    let found: Option<String> = match except_id {
        Some(id) => sqlx::query_scalar(
            "SELECT id FROM github_repos WHERE connection_id = ? AND owner = ? AND repo = ? AND id != ?",
        )
        .bind(connection_id)
        .bind(owner)
        .bind(repo)
        .bind(id)
        .fetch_optional(pool)
        .await?,
        None => sqlx::query_scalar(
            "SELECT id FROM github_repos WHERE connection_id = ? AND owner = ? AND repo = ?",
        )
        .bind(connection_id)
        .bind(owner)
        .bind(repo)
        .fetch_optional(pool)
        .await?,
    };
    Ok(found.is_some())
}

pub async fn insert_repo(
    pool: &SqlitePool,
    connection_id: &str,
    owner: &str,
    repo: &str,
) -> Result<String, AppError> {
    let id = Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO github_repos (id, connection_id, owner, repo) VALUES (?, ?, ?, ?)")
        .bind(&id)
        .bind(connection_id)
        .bind(owner)
        .bind(repo)
        .execute(pool)
        .await?;
    Ok(id)
}

pub async fn update_repo(
    pool: &SqlitePool,
    id: &str,
    owner: &str,
    repo: &str,
) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE github_repos SET owner = ?, repo = ?, updated_at = datetime('now') WHERE id = ?",
    )
    .bind(owner)
    .bind(repo)
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn delete_repo(pool: &SqlitePool, id: &str) -> Result<bool, AppError> {
    let result = sqlx::query("DELETE FROM github_repos WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() != 0)
}

/// `(name, api_base_url)` of one connection — the join columns a repo view needs.
pub async fn connection_name_and_base(
    pool: &SqlitePool,
    id: &str,
) -> Result<Option<(String, String)>, AppError> {
    let row = sqlx::query("SELECT name, api_base_url FROM github_connections WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await?;
    Ok(row.map(|r| (r.get(0), r.get(1))))
}
