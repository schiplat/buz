//! GitHub Actions sync API — connections, repo registration, bindings, push.
//!
//! Connections and repos are platform-admin only (the PAT is the blast radius).
//! Secret binds use the secret's own team `SecretManage` permission; variable
//! binds are platform-admin (platform_vars is platform-level).

use axum::{
    extract::{Path, Query, State},
    Json,
};
use serde::{Deserialize, Deserializer};
use shared::error::AppError;
use shared::protocol::response::ApiResponse;
use shared::rbac::Perm;
use sqlx::Row;
use uuid::Uuid;

use crate::audit::{self, Action, AuditEvent};
use crate::crypto;
use crate::middleware::{AuthUser, PlatformAdmin};
use crate::rbac::secret_team_id;
use crate::services::github_actions_sync::{
    fetch_and_upsert_repos, seal_secret_value, GitHubClient,
};
use crate::state::AppState;

// ────────────────────────────── helpers ──────────────────────────────

/// Load a connection's token (decrypted) for outbound calls.
async fn connection_client(
    pool: &sqlx::SqlitePool,
    jwt_secret: &str,
    connection_id: &str,
) -> Result<(GitHubClient, String), AppError> {
    let row = sqlx::query(
        "SELECT api_base_url, token_ciphertext, name FROM github_connections WHERE id = ?",
    )
    .bind(connection_id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| AppError::NotFound("github connection not found".into()))?;
    let api_base_url: String = row.get(0);
    let token_cipher: String = row.get(1);
    let name: String = row.get(2);
    let token = crypto::decrypt(&token_cipher, jwt_secret)
        .map_err(|e| AppError::Internal(format!("decrypt connection token '{name}': {e}")))?;
    Ok((GitHubClient::new(&api_base_url, &token), name))
}

/// Validate a GitHub Actions secret/variable name for pushability.
/// Buz names are already UPPER_SNAKE_CASE; GitHub forbids `GITHUB_` prefix and
/// leading digit.
fn validate_gh_name(name: &str) -> Result<(), AppError> {
    if name.starts_with("GITHUB_") {
        return Err(AppError::ValidationFailed(format!(
            "'{name}' is reserved on GitHub (GITHUB_ prefix) — skip it"
        )));
    }
    if name.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        return Err(AppError::ValidationFailed(format!(
            "'{name}' must not start with a digit on GitHub"
        )));
    }
    Ok(())
}

/// Validate a GitHub API base URL. Requires `https` (plain `http` is accepted
/// only for localhost, e.g. a local GHE or tests); the normalized URL has no
/// trailing slash. Rejects other schemes and malformed URLs so the stored PAT
/// is never sent to an arbitrary scheme/host by construction.
fn validate_api_base_url(raw: &str) -> Result<String, AppError> {
    let s = raw.trim().trim_end_matches('/');
    let url = url::Url::parse(s)
        .map_err(|_| AppError::ValidationFailed("api_base_url must be a valid URL".into()))?;
    let scheme_ok = url.scheme() == "https"
        || (url.scheme() == "http"
            && matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]")));
    if !scheme_ok {
        return Err(AppError::ValidationFailed(
            "api_base_url must use https (plain http is only allowed for localhost)".into(),
        ));
    }
    Ok(s.to_string())
}

/// Display data for a `github_connections` row (token never included).
struct ConnectionView {
    pub id: String,
    pub name: String,
    pub api_base_url: String,
    pub has_token: bool,
    pub updated_at: Option<String>,
    pub fetch_owners: String,
    pub fetch_interval_minutes: Option<i64>,
    pub last_fetch_at: Option<String>,
}

fn connection_view(v: &ConnectionView) -> serde_json::Value {
    serde_json::json!({
        "id": v.id,
        "name": v.name,
        "api_base_url": v.api_base_url,
        "has_token": v.has_token,
        "fetch_owners": v.fetch_owners,
        "fetch_interval_minutes": v.fetch_interval_minutes,
        "last_fetch_at": v.last_fetch_at,
        "updated_at": v.updated_at,
    })
}

// ────────────────────────── connections (admin) ──────────────────────────

#[derive(Debug, Deserialize)]
pub struct CreateConnectionRequest {
    pub name: String,
    #[serde(default)]
    pub api_base_url: Option<String>,
    pub token: String,
    #[serde(default)]
    pub fetch_owners: Option<String>,
    #[serde(default)]
    pub fetch_interval_minutes: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateConnectionRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub api_base_url: Option<String>,
    #[serde(default)]
    pub token: Option<String>,
    /// `Some(Some(s))` update; `Some(None)` clear; `None` keep current.
    #[serde(default, deserialize_with = "deserialize_some")]
    pub fetch_owners: Option<Option<String>>,
    /// `Some(Some(n))` update; `Some(None)` disable; `None` keep current.
    #[serde(default, deserialize_with = "deserialize_some")]
    pub fetch_interval_minutes: Option<Option<i64>>,
}

/// Wrap a `null` JSON value as `Some(None)` so update requests can distinguish
/// "field absent" (`None`) from "explicitly cleared" (`Some(None)`).
fn deserialize_some<'de, T, D>(deserializer: D) -> Result<Option<T>, D::Error>
where
    T: Deserialize<'de>,
    D: Deserializer<'de>,
{
    Deserialize::deserialize(deserializer).map(Some)
}

/// Normalize a fetch-owners list (comma or newline separated) into a canonical
/// comma-separated string. Empty input → empty string (no auto-fetch).
fn normalize_fetch_owners(raw: Option<String>) -> String {
    let Some(raw) = raw else {
        return String::new();
    };
    raw.split([',', '\n'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

/// Validate a fetch interval: positive minutes, or None to disable auto-fetch.
fn validate_fetch_interval(raw: Option<i64>) -> Result<Option<i64>, AppError> {
    match raw {
        Some(n) if n <= 0 => Err(AppError::ValidationFailed(
            "fetch_interval_minutes must be a positive integer, or omit/null to disable".into(),
        )),
        other => Ok(other),
    }
}

pub async fn list_connections(
    _auth: PlatformAdmin,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<Vec<serde_json::Value>>>, AppError> {
    let out: Vec<serde_json::Value> = crate::store::github::list_connections(&state.pool)
        .await?
        .iter()
        .map(|c| {
            connection_view(&ConnectionView {
                id: c.id.clone(),
                name: c.name.clone(),
                api_base_url: c.api_base_url.clone(),
                has_token: c.has_token(),
                updated_at: c.updated_at.clone(),
                fetch_owners: c.fetch_owners.clone(),
                fetch_interval_minutes: c.fetch_interval_minutes,
                last_fetch_at: c.last_fetch_at.clone(),
            })
        })
        .collect();
    Ok(Json(ApiResponse::success(out)))
}

pub async fn create_connection(
    auth: PlatformAdmin,
    State(state): State<AppState>,
    Json(req): Json<CreateConnectionRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let name = req.name.trim().to_string();
    if name.is_empty() {
        return Err(AppError::ValidationFailed("name is required".into()));
    }
    if req.token.trim().is_empty() {
        return Err(AppError::ValidationFailed("token is required".into()));
    }
    let api_base_url = match req
        .api_base_url
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    {
        Some(raw) => validate_api_base_url(&raw)?,
        None => "https://api.github.com".to_string(),
    };
    let fetch_owners = normalize_fetch_owners(req.fetch_owners);
    let fetch_interval_minutes = validate_fetch_interval(req.fetch_interval_minutes)?;

    if crate::store::github::name_taken(&state.pool, &name, None).await? {
        return Err(AppError::Conflict(format!(
            "github connection '{name}' already exists"
        )));
    }

    let cipher = crypto::encrypt(req.token.trim(), &state.config.auth.jwt_secret)
        .map_err(|e| AppError::Internal(format!("token encryption failed: {e}")))?;
    let id = crate::store::github::insert_connection(
        &state.pool,
        &name,
        &api_base_url,
        &cipher,
        &fetch_owners,
        fetch_interval_minutes,
    )
    .await?;

    audit::record(
        &state.pool,
        AuditEvent::from_auth(&auth, Action::GithubConnectionCreate)
            .resource("github_connection", &id, &name)
            .after(Some(serde_json::json!({
                "name": name,
                "api_base_url": api_base_url,
                "has_token": true,
                "fetch_owners": fetch_owners,
                "fetch_interval_minutes": fetch_interval_minutes,
            }))),
    )
    .await;

    Ok(Json(ApiResponse::success(connection_view(
        &ConnectionView {
            id,
            name,
            api_base_url,
            has_token: true,
            updated_at: None,
            fetch_owners,
            fetch_interval_minutes,
            last_fetch_at: None,
        },
    ))))
}

pub async fn update_connection(
    auth: PlatformAdmin,
    Path(id): Path<String>,
    State(state): State<AppState>,
    Json(req): Json<UpdateConnectionRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let existing = crate::store::github::connection(&state.pool, &id)
        .await?
        .ok_or_else(|| AppError::NotFound("github connection not found".into()))?;
    let old_name = existing.name;
    let old_base = existing.api_base_url;
    let old_cipher = existing.token_ciphertext;
    let old_fetch_owners = existing.fetch_owners;
    let old_fetch_interval = existing.fetch_interval_minutes;

    let requested_name = req.name.as_deref().map(str::trim);
    let requested_token = req
        .token
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty());

    let new_name = requested_name
        .filter(|n| !n.is_empty())
        .map(str::to_string)
        .unwrap_or(old_name.clone());
    let new_base = match req
        .api_base_url
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(raw) => validate_api_base_url(raw)?,
        None => old_base,
    };
    // Fetch config: keep current when the field is absent; explicit null clears
    // owners / disables the interval.
    let new_fetch_owners = match req.fetch_owners {
        Some(Some(raw)) => normalize_fetch_owners(Some(raw)),
        Some(None) => String::new(),
        None => old_fetch_owners,
    };
    let new_fetch_interval = match req.fetch_interval_minutes {
        Some(Some(n)) => Some(validate_fetch_interval(Some(n))?.expect("validated")),
        Some(None) => None,
        None => old_fetch_interval,
    };

    if let Some(n) = requested_name.filter(|n| !n.is_empty()) {
        if n != old_name && crate::store::github::name_taken(&state.pool, n, Some(&id)).await? {
            return Err(AppError::Conflict(format!(
                "github connection '{n}' already exists"
            )));
        }
    }

    // An absent token keeps the stored one; the API treats it as unchanged.
    let new_cipher = match requested_token {
        Some(token) => Some(
            crypto::encrypt(token, &state.config.auth.jwt_secret)
                .map_err(|e| AppError::Internal(format!("token encryption failed: {e}")))?,
        ),
        None => None,
    };
    crate::store::github::update_connection(
        &state.pool,
        &id,
        &new_name,
        &new_base,
        new_cipher.as_deref(),
        &new_fetch_owners,
        new_fetch_interval,
    )
    .await?;
    audit::record(
        &state.pool,
        AuditEvent::from_auth(&auth, Action::GithubConnectionUpdate)
            .resource("github_connection", &id, &new_name)
            .before(Some(serde_json::json!({ "name": old_name })))
            .after(Some(serde_json::json!({
                "name": new_name,
                "api_base_url": new_base,
                "fetch_owners": new_fetch_owners,
                "fetch_interval_minutes": new_fetch_interval,
            }))),
    )
    .await;

    Ok(Json(ApiResponse::success(connection_view(
        &ConnectionView {
            id,
            name: new_name,
            api_base_url: new_base,
            has_token: !old_cipher.is_empty() || requested_token.is_some(),
            updated_at: None,
            fetch_owners: new_fetch_owners,
            fetch_interval_minutes: new_fetch_interval,
            last_fetch_at: None,
        },
    ))))
}

pub async fn delete_connection(
    auth: PlatformAdmin,
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    let name = crate::store::github::connection(&state.pool, &id)
        .await?
        .ok_or_else(|| AppError::NotFound("github connection not found".into()))?
        .name;
    if !crate::store::github::delete_connection(&state.pool, &id).await? {
        return Err(AppError::NotFound("github connection not found".into()));
    }
    audit::record(
        &state.pool,
        AuditEvent::from_auth(&auth, Action::GithubConnectionDelete)
            .resource("github_connection", &id, &name)
            .before(Some(serde_json::json!({ "name": name }))),
    )
    .await;
    Ok(Json(ApiResponse::success(())))
}

// ─────────────────────────── repos (admin) ───────────────────────────

#[derive(Debug, Deserialize)]
pub struct CreateRepoRequest {
    pub connection_id: String,
    pub owner: String,
    pub repo: String,
}

#[derive(Debug, Deserialize)]
pub struct UpdateRepoRequest {
    #[serde(default)]
    pub owner: Option<String>,
    #[serde(default)]
    pub repo: Option<String>,
}

/// Derive the repo's web UI base from an API base URL.
/// `https://api.github.com` → `https://github.com`; GHE `https://ghe.example.com/api/v3`
/// → `https://ghe.example.com`; a bare `https://api.ghe.example.com` → `https://ghe.example.com`.
fn gh_web_base(api_base_url: &str) -> String {
    let base = api_base_url.trim().trim_end_matches('/');
    if base == "https://api.github.com" {
        return "https://github.com".to_string();
    }
    if let Some(rest) = base.strip_prefix("https://api.") {
        return format!("https://{rest}");
    }
    if let Some(rest) = base.strip_prefix("http://api.") {
        return format!("http://{rest}");
    }
    // GHE: take scheme://host only.
    base.split('/').take(3).collect::<Vec<_>>().join("/")
}

/// Display data for a registered GitHub repo (repos list / create / update).
struct RepoView<'a> {
    id: &'a str,
    connection_id: &'a str,
    connection_name: &'a str,
    owner: &'a str,
    repo: &'a str,
    updated_at: Option<String>,
    api_base_url: &'a str,
    binds: &'a [serde_json::Value],
}

fn repo_view(r: &RepoView<'_>) -> serde_json::Value {
    serde_json::json!({
        "id": r.id,
        "connection_id": r.connection_id,
        "connection_name": r.connection_name,
        "owner": r.owner,
        "repo": r.repo,
        "url": format!("{}/{}/{}", gh_web_base(r.api_base_url), r.owner, r.repo),
        "updated_at": r.updated_at,
        "binds": r.binds,
    })
}

/// Batch-load repo → bound secret/var summaries for the repos list.
/// Only metadata (bind id, resource name, push state) — never values.
pub(crate) async fn repo_binds_map(
    pool: &sqlx::SqlitePool,
    repo_ids: &[String],
) -> Result<std::collections::HashMap<String, Vec<serde_json::Value>>, AppError> {
    let mut map: std::collections::HashMap<String, Vec<serde_json::Value>> =
        std::collections::HashMap::new();
    if repo_ids.is_empty() {
        return Ok(map);
    }
    let placeholders = repo_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");

    let secret_sql = format!(
        "SELECT b.repo_id, b.id, s.name, b.last_status, b.last_op
         FROM secret_github_binds b
         JOIN secrets s ON s.id = b.secret_id
         WHERE b.repo_id IN ({placeholders})
         ORDER BY s.name ASC"
    );
    let mut q = sqlx::query(&secret_sql);
    for id in repo_ids {
        q = q.bind(id);
    }
    for r in q.fetch_all(pool).await? {
        map.entry(r.get::<String, _>(0))
            .or_default()
            .push(serde_json::json!({
                "kind": "secret",
                "id": r.get::<String, _>(1),
                "name": r.get::<String, _>(2),
                "last_status": r.get::<String, _>(3),
                "last_op": r.get::<String, _>(4),
            }));
    }

    let var_sql = format!(
        "SELECT b.repo_id, b.id, v.name, b.last_status, b.last_op
         FROM variable_github_binds b
         JOIN platform_vars v ON v.id = b.var_id
         WHERE b.repo_id IN ({placeholders})
         ORDER BY v.name ASC"
    );
    let mut q = sqlx::query(&var_sql);
    for id in repo_ids {
        q = q.bind(id);
    }
    for r in q.fetch_all(pool).await? {
        map.entry(r.get::<String, _>(0))
            .or_default()
            .push(serde_json::json!({
                "kind": "variable",
                "id": r.get::<String, _>(1),
                "name": r.get::<String, _>(2),
                "last_status": r.get::<String, _>(3),
                "last_op": r.get::<String, _>(4),
            }));
    }

    Ok(map)
}

pub async fn list_repos(
    _auth: PlatformAdmin,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<Vec<serde_json::Value>>>, AppError> {
    let rows = sqlx::query(
        "SELECT r.id, r.connection_id, c.name, r.owner, r.repo, r.updated_at, c.api_base_url
         FROM github_repos r
         JOIN github_connections c ON c.id = r.connection_id
         ORDER BY r.owner ASC, r.repo ASC",
    )
    .fetch_all(&state.pool)
    .await?;
    let repo_ids: Vec<String> = rows.iter().map(|r| r.get::<String, _>(0)).collect();
    let binds_map = repo_binds_map(&state.pool, &repo_ids).await?;
    let out: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|r| {
            let id = r.get::<String, _>(0);
            let binds = binds_map.get(&id).cloned().unwrap_or_default();
            repo_view(&RepoView {
                id: &id,
                connection_id: &r.get::<String, _>(1),
                connection_name: &r.get::<String, _>(2),
                owner: &r.get::<String, _>(3),
                repo: &r.get::<String, _>(4),
                updated_at: r.get(5),
                api_base_url: &r.get::<String, _>(6),
                binds: &binds,
            })
        })
        .collect();
    Ok(Json(ApiResponse::success(out)))
}

pub async fn create_repo(
    auth: PlatformAdmin,
    State(state): State<AppState>,
    Json(req): Json<CreateRepoRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let owner = req.owner.trim().to_string();
    let repo = req.repo.trim().to_string();
    if owner.is_empty() || repo.is_empty() {
        return Err(AppError::ValidationFailed(
            "owner and repo are required".into(),
        ));
    }
    let (conn_name, conn_api_base) =
        crate::store::github::connection_name_and_base(&state.pool, &req.connection_id)
            .await?
            .ok_or_else(|| AppError::NotFound("github connection not found".into()))?;

    if crate::store::github::repo_exists(&state.pool, &req.connection_id, &owner, &repo, None)
        .await?
    {
        return Err(AppError::Conflict(format!(
            "repo '{owner}/{repo}' already registered on this connection"
        )));
    }

    let id =
        crate::store::github::insert_repo(&state.pool, &req.connection_id, &owner, &repo).await?;

    audit::record(
        &state.pool,
        AuditEvent::from_auth(&auth, Action::GithubRepoCreate)
            .resource("github_repo", &id, format!("{owner}/{repo}"))
            .after(Some(serde_json::json!({ "connection_id": &req.connection_id, "owner": owner, "repo": repo }))),
    )
    .await;

    Ok(Json(ApiResponse::success(repo_view(&RepoView {
        id: &id,
        connection_id: &req.connection_id,
        connection_name: &conn_name,
        owner: &owner,
        repo: &repo,
        updated_at: None,
        api_base_url: &conn_api_base,
        binds: &[],
    }))))
}

pub async fn update_repo(
    auth: PlatformAdmin,
    Path(id): Path<String>,
    State(state): State<AppState>,
    Json(req): Json<UpdateRepoRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let existing = crate::store::github::repo_by_id(&state.pool, &id)
        .await?
        .ok_or_else(|| AppError::NotFound("github repo not found".into()))?;
    let old_owner = existing.owner;
    let old_repo = existing.repo;
    let conn_id = existing.connection_id;
    let (conn_name, conn_api_base) =
        crate::store::github::connection_name_and_base(&state.pool, &conn_id)
            .await?
            .ok_or_else(|| AppError::NotFound("github connection not found".into()))?;

    let new_owner = req.owner.unwrap_or_else(|| old_owner.clone());
    let new_repo = req.repo.unwrap_or_else(|| old_repo.clone());
    if new_owner.trim().is_empty() || new_repo.trim().is_empty() {
        return Err(AppError::ValidationFailed(
            "owner and repo are required".into(),
        ));
    }

    crate::store::github::update_repo(&state.pool, &id, &new_owner, &new_repo).await?;

    audit::record(
        &state.pool,
        AuditEvent::from_auth(&auth, Action::GithubRepoUpdate)
            .resource("github_repo", &id, format!("{old_owner}/{old_repo}"))
            .after(Some(
                serde_json::json!({ "owner": new_owner, "repo": new_repo }),
            )),
    )
    .await;

    Ok(Json(ApiResponse::success(repo_view(&RepoView {
        id: &id,
        connection_id: &conn_id,
        connection_name: &conn_name,
        owner: &new_owner,
        repo: &new_repo,
        updated_at: None,
        api_base_url: &conn_api_base,
        binds: &[],
    }))))
}

pub async fn delete_repo(
    auth: PlatformAdmin,
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    let removed = crate::store::github::repo_by_id(&state.pool, &id)
        .await?
        .ok_or_else(|| AppError::NotFound("github repo not found".into()))?;
    let repo_label = format!("{}/{}", removed.owner, removed.repo);
    if !crate::store::github::delete_repo(&state.pool, &id).await? {
        return Err(AppError::NotFound("github repo not found".into()));
    }
    audit::record(
        &state.pool,
        AuditEvent::from_auth(&auth, Action::GithubRepoDelete)
            .resource("github_repo", &id, &repo_label)
            .before(Some(
                serde_json::json!({ "owner": removed.owner, "repo": removed.repo }),
            )),
    )
    .await;
    Ok(Json(ApiResponse::success(())))
}

// ────────────────────────── fetch + test (admin) ──────────────────────────

#[derive(Debug, Deserialize)]
pub struct FetchReposRequest {
    pub connection_id: String,
    #[serde(default)]
    pub owner: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct TestRequest {
    pub connection_id: String,
    #[serde(default)]
    pub owner: Option<String>,
    #[serde(default)]
    pub repo: Option<String>,
}

/// POST /github-sync/repos/fetch — upsert an owner's repos (org, user fallback).
pub async fn fetch_repos(
    auth: PlatformAdmin,
    State(state): State<AppState>,
    Json(req): Json<FetchReposRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let owner = req
        .owner
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let owner = match owner {
        Some(o) => o,
        None => return Err(AppError::ValidationFailed("owner is required".into())),
    };

    let (client, name) = connection_client(
        &state.pool,
        &state.config.auth.jwt_secret,
        &req.connection_id,
    )
    .await?;

    tracing::info!(
        connection = %name,
        connection_id = %req.connection_id,
        owner = %owner,
        source = "manual",
        "github-sync: fetch repos from owner"
    );

    let (added, existing) =
        fetch_and_upsert_repos(&state.pool, &req.connection_id, &name, &owner, &client).await?;

    // A manual fetch suppresses the next scheduled run.
    sqlx::query("UPDATE github_connections SET last_fetch_at = datetime('now') WHERE id = ?")
        .bind(&req.connection_id)
        .execute(&state.pool)
        .await?;

    audit::record(
        &state.pool,
        AuditEvent::from_auth(&auth, Action::GithubRepoFetch)
            .resource("github_connection", &req.connection_id, &owner)
            .after(Some(
                serde_json::json!({ "owner": owner, "added": added, "existing": existing }),
            )),
    )
    .await;

    Ok(Json(ApiResponse::success(serde_json::json!({
        "added": added,
        "existing": existing,
    }))))
}

/// POST /github-sync/test — verify token validity and repo access.
pub async fn test_connection(
    auth: PlatformAdmin,
    State(state): State<AppState>,
    Json(req): Json<TestRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let (client, name) = connection_client(
        &state.pool,
        &state.config.auth.jwt_secret,
        &req.connection_id,
    )
    .await?;
    let owner = req
        .owner
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let repo = req
        .repo
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    let result = match (&owner, &repo) {
        (Some(o), Some(r)) => client.test_connection(o, Some(r)).await,
        (Some(o), None) => client.test_connection(o, None).await,
        // No owner/repo: validate the token alone via GET /user.
        (None, None) => match client.test_token().await {
            Ok(login) => {
                audit::record(
                    &state.pool,
                    AuditEvent::from_auth(&auth, Action::GithubConnectionTest)
                        .resource("github_connection", &req.connection_id, &name)
                        .after(Some(serde_json::json!({ "ok": true, "login": login }))),
                )
                .await;
                return Ok(Json(ApiResponse::success(serde_json::json!({
                    "ok": true,
                    "login": login,
                }))));
            }
            Err(e) => Err(e),
        },
        (None, Some(_)) => {
            return Err(AppError::ValidationFailed(
                "repo is set but owner is missing — provide owner, or leave both empty to just validate the token"
                    .into(),
            ))
        }
    };
    match result {
        Ok(()) => {
            audit::record(
                &state.pool,
                AuditEvent::from_auth(&auth, Action::GithubConnectionTest)
                    .resource("github_connection", &req.connection_id, &name)
                    .after(Some(serde_json::json!({ "ok": true }))),
            )
            .await;
            Ok(Json(ApiResponse::success(
                serde_json::json!({ "ok": true }),
            )))
        }
        Err(e) => Err(e),
    }
}

// ────────────────────────── secret binds ──────────────────────────

#[derive(Debug, Deserialize)]
pub struct AddBindRequest {
    pub repo_id: String,
    /// Optional per-repo push value. Variables: stored plaintext. Secrets: encrypted at rest.
    /// Omit/null = inherit the resource default.
    #[serde(default)]
    pub override_value: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct PatchBindOverrideRequest {
    /// `null` clears to inherit; a string sets a per-repo push value.
    /// Secrets: empty string is rejected; plaintext is encrypted before storage.
    pub override_value: Option<String>,
}

/// Resolve the value pushed to GitHub for a variable/secret bind.
/// `None` override → platform default; `Some(s)` → `s` (including empty for variables).
pub(crate) fn effective_variable_push_value<'a>(
    default_value: &'a str,
    override_value: Option<&'a str>,
) -> &'a str {
    match override_value {
        Some(v) => v,
        None => default_value,
    }
}

/// Display data for a secret/var ↔ GitHub repo bind.
pub(crate) struct BindView<'a> {
    id: &'a str,
    repo_id: &'a str,
    connection_name: &'a str,
    owner: &'a str,
    repo: &'a str,
    last_pushed_at: Option<String>,
    last_status: &'a str,
    last_error: &'a str,
    last_op: &'a str,
    /// Variable binds only (plaintext). Secrets never expose override plaintext here.
    override_value: Option<&'a str>,
    /// True when a per-repo override is stored (secrets: ciphertext present; vars: value set).
    has_override: bool,
}

fn bind_view(b: &BindView<'_>) -> serde_json::Value {
    serde_json::json!({
        "id": b.id,
        "repo_id": b.repo_id,
        "connection_name": b.connection_name,
        "owner": b.owner,
        "repo": b.repo,
        "last_pushed_at": b.last_pushed_at,
        "last_status": b.last_status,
        "last_error": b.last_error,
        "last_op": b.last_op,
        "override_value": b.override_value,
        "has_override": b.has_override,
    })
}

pub async fn list_secret_binds(
    auth: AuthUser,
    Path(secret_id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<Vec<serde_json::Value>>>, AppError> {
    let team_id = secret_team_id(&state.pool, &secret_id).await?;
    auth.check_team(&team_id, Perm::TeamView)?;
    let rows = sqlx::query(
        "SELECT b.id, b.repo_id, c.name, r.owner, r.repo, b.last_pushed_at, b.last_status, b.last_error, b.last_op, b.override_value
         FROM secret_github_binds b
         JOIN github_repos r ON r.id = b.repo_id
         JOIN github_connections c ON c.id = r.connection_id
         WHERE b.secret_id = ?
         ORDER BY r.owner ASC, r.repo ASC",
    )
    .bind(&secret_id)
    .fetch_all(&state.pool)
    .await?;
    let out: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|r| {
            let ov_cipher: Option<String> = r.get(9);
            let has_override = ov_cipher.is_some();
            bind_view(&BindView {
                id: &r.get::<String, _>(0),
                repo_id: &r.get::<String, _>(1),
                connection_name: &r.get::<String, _>(2),
                owner: &r.get::<String, _>(3),
                repo: &r.get::<String, _>(4),
                last_pushed_at: r.get(5),
                last_status: &r.get::<String, _>(6),
                last_error: &r.get::<String, _>(7),
                last_op: &r.get::<String, _>(8),
                // Never expose secret override plaintext over the API.
                override_value: None,
                has_override,
            })
        })
        .collect();
    Ok(Json(ApiResponse::success(out)))
}

pub async fn add_secret_bind(
    auth: PlatformAdmin,
    Path(secret_id): Path<String>,
    State(state): State<AppState>,
    Json(req): Json<AddBindRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    // GitHub binds push the secret to a repo using the connection's PAT, so
    // binding is restricted to platform admins — team maintainers can read the
    // bind state but not change it.
    let team_id = secret_team_id(&state.pool, &secret_id).await?;

    let repo = sqlx::query(
        "SELECT r.owner, r.repo, c.name FROM github_repos r
         JOIN github_connections c ON c.id = r.connection_id
         WHERE r.id = ?",
    )
    .bind(&req.repo_id)
    .fetch_optional(&state.pool)
    .await?;
    let (owner, repo_name, conn_name) = match repo {
        Some(r) => (
            r.get::<String, _>(0),
            r.get::<String, _>(1),
            r.get::<String, _>(2),
        ),
        None => {
            // Binding failed — record it so the failure is traceable.
            let secret_name: String = sqlx::query_scalar("SELECT name FROM secrets WHERE id = ?")
                .bind(&secret_id)
                .fetch_optional(&state.pool)
                .await?
                .unwrap_or_default();
            audit::record(
                &state.pool,
                AuditEvent::from_auth(&auth, Action::SecretGithubBindAdd)
                    .resource("secret", &secret_id, &secret_name)
                    .team(Some(team_id.clone()))
                    .meta(Some(serde_json::json!({
                        "repo_id": req.repo_id,
                        "status": "failed",
                        "error": "github repo not found",
                    }))),
            )
            .await;
            return Err(AppError::NotFound("github repo not found".into()));
        }
    };

    let dup = sqlx::query("SELECT id FROM secret_github_binds WHERE secret_id = ? AND repo_id = ?")
        .bind(&secret_id)
        .bind(&req.repo_id)
        .fetch_optional(&state.pool)
        .await?;
    if dup.is_some() {
        let secret_name: String = sqlx::query_scalar("SELECT name FROM secrets WHERE id = ?")
            .bind(&secret_id)
            .fetch_one(&state.pool)
            .await?;
        audit::record(
            &state.pool,
            AuditEvent::from_auth(&auth, Action::SecretGithubBindAdd)
                .resource("secret", &secret_id, &secret_name)
                .team(Some(team_id))
                .meta(Some(serde_json::json!({
                    "repo": format!("{owner}/{repo_name}"),
                    "connection": conn_name,
                    "status": "failed",
                    "error": "already bound",
                }))),
        )
        .await;
        return Err(AppError::Conflict(format!(
            "secret already bound to '{owner}/{repo_name}'"
        )));
    }

    let id = Uuid::new_v4().to_string();
    let override_cipher = match &req.override_value {
        None => None,
        Some(v) if v.is_empty() => {
            return Err(AppError::ValidationFailed(
                "override_value must be non-empty for secret binds (use null to inherit)".into(),
            ));
        }
        Some(v) => Some(
            crypto::encrypt(v, &state.config.auth.jwt_secret)
                .map_err(|e| AppError::Internal(format!("encrypt secret override: {e}")))?,
        ),
    };
    sqlx::query(
        "INSERT INTO secret_github_binds (id, secret_id, repo_id, last_op, override_value) VALUES (?, ?, ?, 'bind', ?)",
    )
    .bind(&id)
    .bind(&secret_id)
    .bind(&req.repo_id)
    .bind(&override_cipher)
    .execute(&state.pool)
    .await?;

    let secret_name: String = sqlx::query_scalar("SELECT name FROM secrets WHERE id = ?")
        .bind(&secret_id)
        .fetch_one(&state.pool)
        .await?;
    audit::record(
        &state.pool,
        AuditEvent::from_auth(&auth, Action::SecretGithubBindAdd)
            .resource("secret", &secret_id, &secret_name)
            .team(Some(team_id))
            .after(Some(serde_json::json!({
                "repo": format!("{owner}/{repo_name}"),
                "connection": conn_name,
                "status": "ok",
                "has_override": override_cipher.is_some(),
            }))),
    )
    .await;

    Ok(Json(ApiResponse::success(bind_view(&BindView {
        id: &id,
        repo_id: &req.repo_id,
        connection_name: &conn_name,
        owner: &owner,
        repo: &repo_name,
        last_pushed_at: None,
        last_status: "pending",
        last_error: "",
        last_op: "bind",
        override_value: None,
        has_override: override_cipher.is_some(),
    }))))
}

pub async fn patch_secret_bind(
    auth: PlatformAdmin,
    Path((secret_id, bind_id)): Path<(String, String)>,
    State(state): State<AppState>,
    Json(req): Json<PatchBindOverrideRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let team_id = secret_team_id(&state.pool, &secret_id).await?;

    let row = sqlx::query(
        "SELECT b.id, b.repo_id, c.name, r.owner, r.repo, b.last_pushed_at, b.last_status, b.last_error, b.last_op
         FROM secret_github_binds b
         JOIN github_repos r ON r.id = b.repo_id
         JOIN github_connections c ON c.id = r.connection_id
         WHERE b.id = ? AND b.secret_id = ?",
    )
    .bind(&bind_id)
    .bind(&secret_id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound("secret github bind not found".into()))?;

    let override_cipher = match &req.override_value {
        None => None,
        Some(v) if v.is_empty() => {
            return Err(AppError::ValidationFailed(
                "override_value must be non-empty for secret binds (use null to inherit)".into(),
            ));
        }
        Some(v) => Some(
            crypto::encrypt(v, &state.config.auth.jwt_secret)
                .map_err(|e| AppError::Internal(format!("encrypt secret override: {e}")))?,
        ),
    };

    sqlx::query(
        "UPDATE secret_github_binds SET override_value = ?, updated_at = datetime('now') WHERE id = ? AND secret_id = ?",
    )
    .bind(&override_cipher)
    .bind(&bind_id)
    .bind(&secret_id)
    .execute(&state.pool)
    .await?;

    // Override change changes the effective push value — mark previously-ok binds stale.
    let marked = mark_secret_bind_stale_if_ok(&state.pool, &bind_id)
        .await
        .unwrap_or(false);

    let secret_name: String = sqlx::query_scalar("SELECT name FROM secrets WHERE id = ?")
        .bind(&secret_id)
        .fetch_one(&state.pool)
        .await?;
    let repo_id: String = row.get(1);
    let conn_name: String = row.get(2);
    let owner: String = row.get(3);
    let repo_name: String = row.get(4);
    let last_pushed_at: Option<String> = row.get(5);
    let last_status: String = if marked {
        "stale".to_string()
    } else {
        row.get(6)
    };
    let last_error: String = if marked { String::new() } else { row.get(7) };
    let last_op: String = row.get(8);

    audit::record(
        &state.pool,
        AuditEvent::from_auth(&auth, Action::SecretGithubBindPatch)
            .resource("secret", &secret_id, &secret_name)
            .team(Some(team_id))
            .after(Some(serde_json::json!({
                "repo": format!("{owner}/{repo_name}"),
                "connection": &conn_name,
                "has_override": override_cipher.is_some(),
                "last_status": &last_status,
            }))),
    )
    .await;

    tracing::info!(
        secret = %secret_name,
        repo = %format!("{owner}/{repo_name}"),
        has_override = override_cipher.is_some(),
        last_status = %last_status,
        "github-sync: secret bind override updated"
    );

    Ok(Json(ApiResponse::success(bind_view(&BindView {
        id: &bind_id,
        repo_id: &repo_id,
        connection_name: &conn_name,
        owner: &owner,
        repo: &repo_name,
        last_pushed_at,
        last_status: &last_status,
        last_error: &last_error,
        last_op: &last_op,
        override_value: None,
        has_override: override_cipher.is_some(),
    }))))
}

pub async fn delete_secret_bind(
    auth: PlatformAdmin,
    Path((secret_id, bind_id)): Path<(String, String)>,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    let team_id = secret_team_id(&state.pool, &secret_id).await?;

    let row = sqlx::query(
        "SELECT r.owner, r.repo, c.api_base_url, c.token_ciphertext, c.name
         FROM secret_github_binds b
         JOIN github_repos r ON r.id = b.repo_id
         JOIN github_connections c ON c.id = r.connection_id
         WHERE b.id = ? AND b.secret_id = ?",
    )
    .bind(&bind_id)
    .bind(&secret_id)
    .fetch_optional(&state.pool)
    .await?;
    let (owner, repo, api_base, token_cipher, conn_name) = match row {
        Some(r) => (
            r.get::<String, _>(0),
            r.get::<String, _>(1),
            r.get::<String, _>(2),
            r.get::<String, _>(3),
            r.get::<String, _>(4),
        ),
        None => return Err(AppError::NotFound("secret github bind not found".into())),
    };

    let secret_name: String = sqlx::query_scalar("SELECT name FROM secrets WHERE id = ?")
        .bind(&secret_id)
        .fetch_one(&state.pool)
        .await?;

    // Remove the value from GitHub Actions first; only unbind locally when the
    // remote cleanup succeeds. 404 is treated as success (never pushed).
    let token = crypto::decrypt(&token_cipher, &state.config.auth.jwt_secret)
        .map_err(|e| AppError::Internal(format!("decrypt connection token '{conn_name}': {e}")))?;
    let client = GitHubClient::new(&api_base, &token);
    if let Err(e) = client.delete_secret(&owner, &repo, &secret_name).await {
        // Record the failed unbind on the bind row so the UI can surface it;
        // the row is kept so the operator can fix the connection and retry.
        sqlx::query(
            "UPDATE secret_github_binds SET last_op = 'unbind', last_status = 'failed', last_error = ?, updated_at = datetime('now') WHERE id = ? AND secret_id = ?",
        )
        .bind(format!("unbind failed: {e}"))
        .bind(&bind_id)
        .bind(&secret_id)
        .execute(&state.pool)
        .await?;
        tracing::warn!(
            connection = %conn_name,
            secret = %secret_name,
            repo = %format!("{owner}/{repo}"),
            error = %e,
            "github-sync: secret unbind failed at remote cleanup, keeping local bind"
        );
        return Err(e);
    }

    sqlx::query("DELETE FROM secret_github_binds WHERE id = ? AND secret_id = ?")
        .bind(&bind_id)
        .bind(&secret_id)
        .execute(&state.pool)
        .await?;

    tracing::info!(
        connection = %conn_name,
        secret = %secret_name,
        repo = %format!("{owner}/{repo}"),
        "github-sync: secret unbound and removed from repo"
    );

    audit::record(
        &state.pool,
        AuditEvent::from_auth(&auth, Action::SecretGithubBindRemove)
            .resource("secret", &secret_id, &secret_name)
            .team(Some(team_id))
            .before(Some(
                serde_json::json!({ "repo": format!("{owner}/{repo}") }),
            )),
    )
    .await;
    Ok(Json(ApiResponse::success(())))
}

#[derive(Debug, Deserialize)]
pub struct PushBindsQuery {
    #[serde(default)]
    pub bind_id: Option<String>,
}

/// POST /secrets/{id}/github-binds/push — push secret value to bound repos.
pub async fn push_secret_binds(
    auth: PlatformAdmin,
    Path(secret_id): Path<String>,
    Query(query): Query<PushBindsQuery>,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let row = sqlx::query("SELECT name, value, team_id FROM secrets WHERE id = ?")
        .bind(&secret_id)
        .fetch_one(&state.pool)
        .await?;
    let secret_name: String = row.get(0);
    let stored: String = row.get(1);
    let secret_team: String = row.get(2);
    let plaintext = crypto::decrypt(&stored, &state.config.auth.jwt_secret)
        .map_err(|e| AppError::Internal(format!("decrypt secret '{secret_name}': {e}")))?;
    validate_gh_name(&secret_name)?;

    let bind_ids: Vec<String> = match &query.bind_id {
        Some(bid) if !bid.is_empty() => vec![bid.clone()],
        _ => {
            sqlx::query_scalar("SELECT id FROM secret_github_binds WHERE secret_id = ?")
                .bind(&secret_id)
                .fetch_all(&state.pool)
                .await?
        }
    };
    if bind_ids.is_empty() {
        return Ok(Json(ApiResponse::success(serde_json::json!({
            "results": [],
        }))));
    }

    let mut results = Vec::new();
    for bind_id in &bind_ids {
        let target = sqlx::query(
            "SELECT b.id, r.owner, r.repo, c.api_base_url, c.token_ciphertext, c.name, b.override_value
             FROM secret_github_binds b
             JOIN github_repos r ON r.id = b.repo_id
             JOIN github_connections c ON c.id = r.connection_id
             WHERE b.id = ? AND b.secret_id = ?",
        )
        .bind(bind_id)
        .bind(&secret_id)
        .fetch_optional(&state.pool)
        .await;
        let target = match target {
            Ok(Some(t)) => t,
            Ok(None) => {
                results.push(serde_json::json!({ "bind_id": bind_id, "status": "failed", "error": "bind not found" }));
                continue;
            }
            Err(e) => {
                results.push(serde_json::json!({ "bind_id": bind_id, "status": "failed", "error": e.to_string() }));
                continue;
            }
        };
        let owner: String = target.get(1);
        let repo: String = target.get(2);
        let api_base: String = target.get(3);
        let token_cipher: String = target.get(4);
        let conn_name: String = target.get(5);
        let override_cipher: Option<String> = target.get(6);

        let push_plaintext = match &override_cipher {
            Some(cipher) => match crypto::decrypt(cipher, &state.config.auth.jwt_secret) {
                Ok(v) => v,
                Err(e) => {
                    results.push(serde_json::json!({
                        "bind_id": bind_id,
                        "owner": owner,
                        "repo": repo,
                        "status": "failed",
                        "error": format!("decrypt secret override: {e}"),
                    }));
                    continue;
                }
            },
            None => plaintext.clone(),
        };

        let outcome = async {
            let token = crypto::decrypt(&token_cipher, &state.config.auth.jwt_secret)
                .map_err(|e| AppError::Internal(format!("decrypt connection token: {e}")))?;
            let client = GitHubClient::new(&api_base, &token);
            let pk = client.get_repo_public_key(&owner, &repo).await?;
            let encrypted =
                seal_secret_value(&pk.key, &push_plaintext).map_err(AppError::Internal)?;
            client
                .put_secret(&owner, &repo, &secret_name, &encrypted, &pk.key_id)
                .await?;
            Ok::<(), AppError>(())
        }
        .await;

        let outcome_success = match outcome {
            Ok(()) => {
                sqlx::query(
                    "UPDATE secret_github_binds SET last_status = 'ok', last_error = '', last_pushed_at = datetime('now'), last_op = 'push', updated_at = datetime('now') WHERE id = ?",
                )
                .bind(bind_id)
                .execute(&state.pool)
                .await?;
                results.push(serde_json::json!({
                    "bind_id": bind_id, "owner": owner, "repo": repo, "status": "ok",
                }));
                tracing::info!(
                    connection = %conn_name,
                    secret = %secret_name,
                    repo = %format!("{owner}/{repo}"),
                    "github-sync: secret pushed to repo"
                );
                true
            }
            Err(e) => {
                sqlx::query(
                    "UPDATE secret_github_binds SET last_status = 'failed', last_error = ?, last_op = 'push', updated_at = datetime('now') WHERE id = ?",
                )
                .bind(e.to_string())
                .bind(bind_id)
                .execute(&state.pool)
                .await?;
                results.push(serde_json::json!({
                    "bind_id": bind_id, "owner": owner, "repo": repo, "status": "failed", "error": e.to_string(),
                }));
                tracing::warn!(
                    connection = %conn_name,
                    secret = %secret_name,
                    repo = %format!("{owner}/{repo}"),
                    error = %e,
                    "github-sync: secret push failed"
                );
                false
            }
        };

        audit::record(
            &state.pool,
            AuditEvent::from_auth(&auth, Action::SecretGithubPush)
                .resource("secret", &secret_id, &secret_name)
                .team(Some(secret_team.clone()))
                .meta(Some(serde_json::json!({
                    "repo": format!("{owner}/{repo}"),
                    "connection": conn_name,
                    "status": if outcome_success { "ok" } else { "failed" },
                }))),
        )
        .await;
    }

    Ok(Json(ApiResponse::success(
        serde_json::json!({ "results": results }),
    )))
}

// ────────────────────────── variable binds (admin) ──────────────────────────

async fn variable_exists(pool: &sqlx::SqlitePool, var_id: &str) -> Result<(), AppError> {
    sqlx::query("SELECT id FROM platform_vars WHERE id = ?")
        .bind(var_id)
        .fetch_optional(pool)
        .await?
        .map(|_| ())
        .ok_or_else(|| AppError::NotFound("variable not found".into()))
}

pub async fn list_variable_binds(
    _auth: PlatformAdmin,
    Path(var_id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<Vec<serde_json::Value>>>, AppError> {
    variable_exists(&state.pool, &var_id).await?;
    let rows = sqlx::query(
        "SELECT b.id, b.repo_id, c.name, r.owner, r.repo, b.last_pushed_at, b.last_status, b.last_error, b.last_op, b.override_value
         FROM variable_github_binds b
         JOIN github_repos r ON r.id = b.repo_id
         JOIN github_connections c ON c.id = r.connection_id
         WHERE b.var_id = ?
         ORDER BY r.owner ASC, r.repo ASC",
    )
    .bind(&var_id)
    .fetch_all(&state.pool)
    .await?;
    let out: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|r| {
            let ov: Option<String> = r.get(9);
            let has_override = ov.is_some();
            bind_view(&BindView {
                id: &r.get::<String, _>(0),
                repo_id: &r.get::<String, _>(1),
                connection_name: &r.get::<String, _>(2),
                owner: &r.get::<String, _>(3),
                repo: &r.get::<String, _>(4),
                last_pushed_at: r.get(5),
                last_status: &r.get::<String, _>(6),
                last_error: &r.get::<String, _>(7),
                last_op: &r.get::<String, _>(8),
                override_value: ov.as_deref(),
                has_override,
            })
        })
        .collect();
    Ok(Json(ApiResponse::success(out)))
}

pub async fn add_variable_bind(
    auth: PlatformAdmin,
    Path(var_id): Path<String>,
    State(state): State<AppState>,
    Json(req): Json<AddBindRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    variable_exists(&state.pool, &var_id).await?;

    let repo = sqlx::query(
        "SELECT r.owner, r.repo, c.name FROM github_repos r
         JOIN github_connections c ON c.id = r.connection_id
         WHERE r.id = ?",
    )
    .bind(&req.repo_id)
    .fetch_optional(&state.pool)
    .await?;
    let (owner, repo_name, conn_name) = match repo {
        Some(r) => (
            r.get::<String, _>(0),
            r.get::<String, _>(1),
            r.get::<String, _>(2),
        ),
        None => {
            // Binding failed — record it so the failure is traceable.
            let var_name: String =
                sqlx::query_scalar("SELECT name FROM platform_vars WHERE id = ?")
                    .bind(&var_id)
                    .fetch_optional(&state.pool)
                    .await?
                    .unwrap_or_default();
            audit::record(
                &state.pool,
                AuditEvent::from_auth(&auth, Action::VariableGithubBindAdd)
                    .resource("variable", &var_id, &var_name)
                    .meta(Some(serde_json::json!({
                        "repo_id": req.repo_id,
                        "status": "failed",
                        "error": "github repo not found",
                    }))),
            )
            .await;
            return Err(AppError::NotFound("github repo not found".into()));
        }
    };

    let dup = sqlx::query("SELECT id FROM variable_github_binds WHERE var_id = ? AND repo_id = ?")
        .bind(&var_id)
        .bind(&req.repo_id)
        .fetch_optional(&state.pool)
        .await?;
    if dup.is_some() {
        let var_name: String = sqlx::query_scalar("SELECT name FROM platform_vars WHERE id = ?")
            .bind(&var_id)
            .fetch_one(&state.pool)
            .await?;
        audit::record(
            &state.pool,
            AuditEvent::from_auth(&auth, Action::VariableGithubBindAdd)
                .resource("variable", &var_id, &var_name)
                .meta(Some(serde_json::json!({
                    "repo": format!("{owner}/{repo_name}"),
                    "connection": conn_name,
                    "status": "failed",
                    "error": "already bound",
                }))),
        )
        .await;
        return Err(AppError::Conflict(format!(
            "variable already bound to '{owner}/{repo_name}'"
        )));
    }

    let id = Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO variable_github_binds (id, var_id, repo_id, last_op, override_value) VALUES (?, ?, ?, 'bind', ?)",
    )
    .bind(&id)
    .bind(&var_id)
    .bind(&req.repo_id)
    .bind(&req.override_value)
    .execute(&state.pool)
    .await?;

    let var_name: String = sqlx::query_scalar("SELECT name FROM platform_vars WHERE id = ?")
        .bind(&var_id)
        .fetch_one(&state.pool)
        .await?;
    audit::record(
        &state.pool,
        AuditEvent::from_auth(&auth, Action::VariableGithubBindAdd)
            .resource("variable", &var_id, &var_name)
            .after(Some(serde_json::json!({
                "repo": format!("{owner}/{repo_name}"),
                "connection": conn_name,
                "status": "ok",
                "has_override": req.override_value.is_some(),
            }))),
    )
    .await;

    Ok(Json(ApiResponse::success(bind_view(&BindView {
        id: &id,
        repo_id: &req.repo_id,
        connection_name: &conn_name,
        owner: &owner,
        repo: &repo_name,
        last_pushed_at: None,
        last_status: "pending",
        last_error: "",
        last_op: "bind",
        override_value: req.override_value.as_deref(),
        has_override: req.override_value.is_some(),
    }))))
}

pub async fn patch_variable_bind(
    auth: PlatformAdmin,
    Path((var_id, bind_id)): Path<(String, String)>,
    State(state): State<AppState>,
    Json(req): Json<PatchBindOverrideRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    variable_exists(&state.pool, &var_id).await?;

    let row = sqlx::query(
        "SELECT b.id, b.repo_id, c.name, r.owner, r.repo, b.last_pushed_at, b.last_status, b.last_error, b.last_op
         FROM variable_github_binds b
         JOIN github_repos r ON r.id = b.repo_id
         JOIN github_connections c ON c.id = r.connection_id
         WHERE b.id = ? AND b.var_id = ?",
    )
    .bind(&bind_id)
    .bind(&var_id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound("variable github bind not found".into()))?;

    sqlx::query(
        "UPDATE variable_github_binds SET override_value = ?, updated_at = datetime('now') WHERE id = ? AND var_id = ?",
    )
    .bind(&req.override_value)
    .bind(&bind_id)
    .bind(&var_id)
    .execute(&state.pool)
    .await?;

    let marked = mark_variable_bind_stale_if_ok(&state.pool, &bind_id)
        .await
        .unwrap_or(false);

    let var_name: String = sqlx::query_scalar("SELECT name FROM platform_vars WHERE id = ?")
        .bind(&var_id)
        .fetch_one(&state.pool)
        .await?;
    let repo_id: String = row.get(1);
    let conn_name: String = row.get(2);
    let owner: String = row.get(3);
    let repo_name: String = row.get(4);
    let last_pushed_at: Option<String> = row.get(5);
    let last_status: String = if marked {
        "stale".to_string()
    } else {
        row.get(6)
    };
    let last_error: String = if marked { String::new() } else { row.get(7) };
    let last_op: String = row.get(8);
    audit::record(
        &state.pool,
        AuditEvent::from_auth(&auth, Action::VariableGithubBindPatch)
            .resource("variable", &var_id, &var_name)
            .after(Some(serde_json::json!({
                "repo": format!("{owner}/{repo_name}"),
                "connection": &conn_name,
                "has_override": req.override_value.is_some(),
                "last_status": &last_status,
            }))),
    )
    .await;

    tracing::info!(
        variable = %var_name,
        repo = %format!("{owner}/{repo_name}"),
        has_override = req.override_value.is_some(),
        last_status = %last_status,
        "github-sync: variable bind override updated"
    );

    Ok(Json(ApiResponse::success(bind_view(&BindView {
        id: &bind_id,
        repo_id: &repo_id,
        connection_name: &conn_name,
        owner: &owner,
        repo: &repo_name,
        last_pushed_at,
        last_status: &last_status,
        last_error: &last_error,
        last_op: &last_op,
        override_value: req.override_value.as_deref(),
        has_override: req.override_value.is_some(),
    }))))
}

pub async fn delete_variable_bind(
    auth: PlatformAdmin,
    Path((var_id, bind_id)): Path<(String, String)>,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    variable_exists(&state.pool, &var_id).await?;

    let row = sqlx::query(
        "SELECT r.owner, r.repo, c.api_base_url, c.token_ciphertext, c.name
         FROM variable_github_binds b
         JOIN github_repos r ON r.id = b.repo_id
         JOIN github_connections c ON c.id = r.connection_id
         WHERE b.id = ? AND b.var_id = ?",
    )
    .bind(&bind_id)
    .bind(&var_id)
    .fetch_optional(&state.pool)
    .await?;
    let (owner, repo, api_base, token_cipher, conn_name) = match row {
        Some(r) => (
            r.get::<String, _>(0),
            r.get::<String, _>(1),
            r.get::<String, _>(2),
            r.get::<String, _>(3),
            r.get::<String, _>(4),
        ),
        None => return Err(AppError::NotFound("variable github bind not found".into())),
    };

    let var_name: String = sqlx::query_scalar("SELECT name FROM platform_vars WHERE id = ?")
        .bind(&var_id)
        .fetch_one(&state.pool)
        .await?;

    // Remove the value from GitHub Actions first; only unbind locally when the
    // remote cleanup succeeds. 404 is treated as success (never pushed).
    let token = crypto::decrypt(&token_cipher, &state.config.auth.jwt_secret)
        .map_err(|e| AppError::Internal(format!("decrypt connection token '{conn_name}': {e}")))?;
    let client = GitHubClient::new(&api_base, &token);
    if let Err(e) = client.delete_variable(&owner, &repo, &var_name).await {
        // Record the failed unbind on the bind row so the UI can surface it;
        // the row is kept so the operator can fix the connection and retry.
        sqlx::query(
            "UPDATE variable_github_binds SET last_op = 'unbind', last_status = 'failed', last_error = ?, updated_at = datetime('now') WHERE id = ? AND var_id = ?",
        )
        .bind(format!("unbind failed: {e}"))
        .bind(&bind_id)
        .bind(&var_id)
        .execute(&state.pool)
        .await?;
        tracing::warn!(
            connection = %conn_name,
            variable = %var_name,
            repo = %format!("{owner}/{repo}"),
            error = %e,
            "github-sync: variable unbind failed at remote cleanup, keeping local bind"
        );
        return Err(e);
    }

    sqlx::query("DELETE FROM variable_github_binds WHERE id = ? AND var_id = ?")
        .bind(&bind_id)
        .bind(&var_id)
        .execute(&state.pool)
        .await?;

    tracing::info!(
        connection = %conn_name,
        variable = %var_name,
        repo = %format!("{owner}/{repo}"),
        "github-sync: variable unbound and removed from repo"
    );

    audit::record(
        &state.pool,
        AuditEvent::from_auth(&auth, Action::VariableGithubBindRemove)
            .resource("variable", &var_id, &var_name)
            .before(Some(
                serde_json::json!({ "repo": format!("{owner}/{repo}") }),
            )),
    )
    .await;
    Ok(Json(ApiResponse::success(())))
}

/// POST /variables/{id}/github-binds/push — push variable value to bound repos.
pub async fn push_variable_binds(
    auth: PlatformAdmin,
    Path(var_id): Path<String>,
    Query(query): Query<PushBindsQuery>,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    variable_exists(&state.pool, &var_id).await?;

    let row = sqlx::query("SELECT name, value FROM platform_vars WHERE id = ?")
        .bind(&var_id)
        .fetch_one(&state.pool)
        .await?;
    let var_name: String = row.get(0);
    let value: String = row.get(1);
    validate_gh_name(&var_name)?;

    let bind_ids: Vec<String> = match &query.bind_id {
        Some(bid) if !bid.is_empty() => vec![bid.clone()],
        _ => {
            sqlx::query_scalar("SELECT id FROM variable_github_binds WHERE var_id = ?")
                .bind(&var_id)
                .fetch_all(&state.pool)
                .await?
        }
    };
    if bind_ids.is_empty() {
        return Ok(Json(ApiResponse::success(serde_json::json!({
            "results": [],
        }))));
    }

    let mut results = Vec::new();
    for bind_id in &bind_ids {
        let target = sqlx::query(
            "SELECT b.id, r.owner, r.repo, c.api_base_url, c.token_ciphertext, c.name, b.override_value
             FROM variable_github_binds b
             JOIN github_repos r ON r.id = b.repo_id
             JOIN github_connections c ON c.id = r.connection_id
             WHERE b.id = ? AND b.var_id = ?",
        )
        .bind(bind_id)
        .bind(&var_id)
        .fetch_optional(&state.pool)
        .await;
        let target = match target {
            Ok(Some(t)) => t,
            Ok(None) => {
                results.push(serde_json::json!({ "bind_id": bind_id, "status": "failed", "error": "bind not found" }));
                continue;
            }
            Err(e) => {
                results.push(serde_json::json!({ "bind_id": bind_id, "status": "failed", "error": e.to_string() }));
                continue;
            }
        };
        let owner: String = target.get(1);
        let repo: String = target.get(2);
        let api_base: String = target.get(3);
        let token_cipher: String = target.get(4);
        let conn_name: String = target.get(5);
        let override_value: Option<String> = target.get(6);
        let push_value =
            effective_variable_push_value(&value, override_value.as_deref()).to_string();

        let outcome = async {
            let token = crypto::decrypt(&token_cipher, &state.config.auth.jwt_secret)
                .map_err(|e| AppError::Internal(format!("decrypt connection token: {e}")))?;
            let client = GitHubClient::new(&api_base, &token);
            client
                .upsert_variable(&owner, &repo, &var_name, &push_value)
                .await?;
            Ok::<(), AppError>(())
        }
        .await;

        let outcome_success = match outcome {
            Ok(()) => {
                sqlx::query(
                    "UPDATE variable_github_binds SET last_status = 'ok', last_error = '', last_pushed_at = datetime('now'), last_op = 'push', updated_at = datetime('now') WHERE id = ?",
                )
                .bind(bind_id)
                .execute(&state.pool)
                .await?;
                results.push(serde_json::json!({
                    "bind_id": bind_id, "owner": owner, "repo": repo, "status": "ok",
                }));
                tracing::info!(
                    connection = %conn_name,
                    variable = %var_name,
                    repo = %format!("{owner}/{repo}"),
                    "github-sync: variable pushed to repo"
                );
                true
            }
            Err(e) => {
                sqlx::query(
                    "UPDATE variable_github_binds SET last_status = 'failed', last_error = ?, last_op = 'push', updated_at = datetime('now') WHERE id = ?",
                )
                .bind(e.to_string())
                .bind(bind_id)
                .execute(&state.pool)
                .await?;
                results.push(serde_json::json!({
                    "bind_id": bind_id, "owner": owner, "repo": repo, "status": "failed", "error": e.to_string(),
                }));
                tracing::warn!(
                    connection = %conn_name,
                    variable = %var_name,
                    repo = %format!("{owner}/{repo}"),
                    error = %e,
                    "github-sync: variable push failed"
                );
                false
            }
        };

        audit::record(
            &state.pool,
            AuditEvent::from_auth(&auth, Action::VariableGithubPush)
                .resource("variable", &var_id, &var_name)
                .meta(Some(serde_json::json!({
                    "repo": format!("{owner}/{repo}"),
                    "connection": conn_name,
                    "status": if outcome_success { "ok" } else { "failed" },
                }))),
        )
        .await;
    }

    Ok(Json(ApiResponse::success(
        serde_json::json!({ "results": results }),
    )))
}

// Re-export helper for the resource list enrichment in secrets.rs / variables.rs.
pub(crate) fn bind_row_json(b: &BindView<'_>) -> serde_json::Value {
    bind_view(b)
}

/// After the platform default secret value changes, mark inherit binds (`override_value IS NULL`)
/// that were successfully pushed as `stale`. Custom-override binds are left alone.
/// Returns how many rows were marked.
pub(crate) async fn mark_secret_binds_stale_on_default_change(
    pool: &sqlx::SqlitePool,
    secret_id: &str,
) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        "UPDATE secret_github_binds
         SET last_status = 'stale', last_error = '', updated_at = datetime('now')
         WHERE secret_id = ? AND override_value IS NULL AND last_status = 'ok'",
    )
    .bind(secret_id)
    .execute(pool)
    .await?;
    let n = result.rows_affected();
    if n > 0 {
        tracing::info!(
            secret_id = %secret_id,
            marked = n,
            "github-sync: marked inherit secret binds stale after default value change"
        );
    }
    Ok(n)
}

/// After the platform default variable value changes, mark inherit binds as `stale`.
pub(crate) async fn mark_variable_binds_stale_on_default_change(
    pool: &sqlx::SqlitePool,
    var_id: &str,
) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        "UPDATE variable_github_binds
         SET last_status = 'stale', last_error = '', updated_at = datetime('now')
         WHERE var_id = ? AND override_value IS NULL AND last_status = 'ok'",
    )
    .bind(var_id)
    .execute(pool)
    .await?;
    let n = result.rows_affected();
    if n > 0 {
        tracing::info!(
            var_id = %var_id,
            marked = n,
            "github-sync: marked inherit variable binds stale after default value change"
        );
    }
    Ok(n)
}

/// After a bind override is patched, mark that bind `stale` if it was previously `ok`
/// (effective push value changed). Already-pending/failed/stale rows are left alone.
pub(crate) async fn mark_secret_bind_stale_if_ok(
    pool: &sqlx::SqlitePool,
    bind_id: &str,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query(
        "UPDATE secret_github_binds
         SET last_status = 'stale', last_error = '', updated_at = datetime('now')
         WHERE id = ? AND last_status = 'ok'",
    )
    .bind(bind_id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}

pub(crate) async fn mark_variable_bind_stale_if_ok(
    pool: &sqlx::SqlitePool,
    bind_id: &str,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query(
        "UPDATE variable_github_binds
         SET last_status = 'stale', last_error = '', updated_at = datetime('now')
         WHERE id = ? AND last_status = 'ok'",
    )
    .bind(bind_id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}

/// Batch-load secret → binds map for list enrichment (single query, no N+1).
pub(crate) async fn secret_binds_map(
    pool: &sqlx::SqlitePool,
    secret_ids: &[String],
) -> Result<std::collections::HashMap<String, Vec<serde_json::Value>>, AppError> {
    if secret_ids.is_empty() {
        return Ok(std::collections::HashMap::new());
    }
    let placeholders = secret_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let sql = format!(
        "SELECT b.secret_id, b.id, b.repo_id, c.name, r.owner, r.repo, b.last_status, b.last_pushed_at, b.last_error, b.last_op, b.override_value
         FROM secret_github_binds b
         JOIN github_repos r ON r.id = b.repo_id
         JOIN github_connections c ON c.id = r.connection_id
         WHERE b.secret_id IN ({placeholders})
         ORDER BY r.owner ASC, r.repo ASC"
    );
    let mut q = sqlx::query(&sql);
    for id in secret_ids {
        q = q.bind(id);
    }
    let rows = q.fetch_all(pool).await?;
    let mut map: std::collections::HashMap<String, Vec<serde_json::Value>> =
        std::collections::HashMap::new();
    for r in rows {
        let secret_id: String = r.get(0);
        let ov: Option<String> = r.get(10);
        map.entry(secret_id)
            .or_default()
            .push(bind_row_json(&BindView {
                id: &r.get::<String, _>(1),
                repo_id: &r.get::<String, _>(2),
                connection_name: &r.get::<String, _>(3),
                owner: &r.get::<String, _>(4),
                repo: &r.get::<String, _>(5),
                last_pushed_at: r.get(7),
                last_status: &r.get::<String, _>(6),
                last_error: &r.get::<String, _>(8),
                last_op: &r.get::<String, _>(9),
                override_value: None,
                has_override: ov.is_some(),
            }));
    }
    Ok(map)
}

/// Batch-load variable → binds map for list enrichment.
pub(crate) async fn variable_binds_map(
    pool: &sqlx::SqlitePool,
    var_ids: &[String],
) -> Result<std::collections::HashMap<String, Vec<serde_json::Value>>, AppError> {
    if var_ids.is_empty() {
        return Ok(std::collections::HashMap::new());
    }
    let placeholders = var_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let sql = format!(
        "SELECT b.var_id, b.id, b.repo_id, c.name, r.owner, r.repo, b.last_status, b.last_pushed_at, b.last_error, b.last_op, b.override_value
         FROM variable_github_binds b
         JOIN github_repos r ON r.id = b.repo_id
         JOIN github_connections c ON c.id = r.connection_id
         WHERE b.var_id IN ({placeholders})
         ORDER BY r.owner ASC, r.repo ASC"
    );
    let mut q = sqlx::query(&sql);
    for id in var_ids {
        q = q.bind(id);
    }
    let rows = q.fetch_all(pool).await?;
    let mut map: std::collections::HashMap<String, Vec<serde_json::Value>> =
        std::collections::HashMap::new();
    for r in rows {
        let var_id: String = r.get(0);
        let ov: Option<String> = r.get(10);
        map.entry(var_id)
            .or_default()
            .push(bind_row_json(&BindView {
                id: &r.get::<String, _>(1),
                repo_id: &r.get::<String, _>(2),
                connection_name: &r.get::<String, _>(3),
                owner: &r.get::<String, _>(4),
                repo: &r.get::<String, _>(5),
                last_pushed_at: r.get(7),
                last_status: &r.get::<String, _>(6),
                last_error: &r.get::<String, _>(8),
                last_op: &r.get::<String, _>(9),
                override_value: ov.as_deref(),
                has_override: ov.is_some(),
            }));
    }
    Ok(map)
}
