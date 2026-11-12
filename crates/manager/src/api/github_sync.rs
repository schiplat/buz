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
    state: &AppState,
    jwt_secret: &str,
    connection_id: &str,
) -> Result<(GitHubClient, String), AppError> {
    let conn = crate::store::github::connection(&state.db(), connection_id)
        .await?
        .ok_or_else(|| AppError::NotFound("github connection not found".into()))?;
    let token = crypto::decrypt(&conn.token_ciphertext, jwt_secret).map_err(|e| {
        AppError::Internal(format!("decrypt connection token '{}': {e}", conn.name))
    })?;
    Ok((GitHubClient::new(&conn.api_base_url, &token), conn.name))
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
    let out: Vec<serde_json::Value> = crate::store::github::list_connections(&state.db())
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

    if crate::store::github::name_taken(&state.db(), &name, None).await? {
        return Err(AppError::Conflict(format!(
            "github connection '{name}' already exists"
        )));
    }

    let cipher = crypto::encrypt(req.token.trim(), &state.config.auth.jwt_secret)
        .map_err(|e| AppError::Internal(format!("token encryption failed: {e}")))?;
    let id = crate::store::github::insert_connection(
        &state.db(),
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
    let existing = crate::store::github::connection(&state.db(), &id)
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
        if n != old_name && crate::store::github::name_taken(&state.db(), n, Some(&id)).await? {
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
        &state.db(),
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
    let name = crate::store::github::connection(&state.db(), &id)
        .await?
        .ok_or_else(|| AppError::NotFound("github connection not found".into()))?
        .name;
    if !crate::store::github::delete_connection(&state.db(), &id).await? {
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
    state: &AppState,
    repo_ids: &[String],
) -> Result<std::collections::HashMap<String, Vec<serde_json::Value>>, AppError> {
    let mut map: std::collections::HashMap<String, Vec<serde_json::Value>> =
        std::collections::HashMap::new();
    let secrets =
        crate::store::github_binds::repo_secret_bind_summaries(&state.pool, repo_ids).await?;
    for s in secrets {
        map.entry(s.repo_id).or_default().push(serde_json::json!({
            "kind": "secret",
            "id": s.id,
            "name": s.name,
            "last_status": s.last_status,
            "last_op": s.last_op,
        }));
    }
    let vars =
        crate::store::github_binds::repo_variable_bind_summaries(&state.pool, repo_ids).await?;
    for v in vars {
        map.entry(v.repo_id).or_default().push(serde_json::json!({
            "kind": "variable",
            "id": v.id,
            "name": v.name,
            "last_status": v.last_status,
            "last_op": v.last_op,
        }));
    }
    Ok(map)
}

pub async fn list_repos(
    _auth: PlatformAdmin,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<Vec<serde_json::Value>>>, AppError> {
    let rows = crate::store::github::list_repos_with_connection(&state.db()).await?;
    let repo_ids: Vec<String> = rows.iter().map(|r| r.id.clone()).collect();
    let binds_map = repo_binds_map(&state, &repo_ids).await?;
    let out: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|r| {
            let id = r.id.clone();
            let binds = binds_map.get(&id).cloned().unwrap_or_default();
            repo_view(&RepoView {
                id: &id,
                connection_id: &r.connection_id,
                connection_name: &r.connection_name,
                owner: &r.owner,
                repo: &r.repo,
                updated_at: r.updated_at,
                api_base_url: &r.api_base_url,
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
        crate::store::github::connection_name_and_base(&state.db(), &req.connection_id)
            .await?
            .ok_or_else(|| AppError::NotFound("github connection not found".into()))?;

    if crate::store::github::repo_exists(&state.db(), &req.connection_id, &owner, &repo, None)
        .await?
    {
        return Err(AppError::Conflict(format!(
            "repo '{owner}/{repo}' already registered on this connection"
        )));
    }

    let id =
        crate::store::github::insert_repo(&state.db(), &req.connection_id, &owner, &repo).await?;

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
    let existing = crate::store::github::repo_by_id(&state.db(), &id)
        .await?
        .ok_or_else(|| AppError::NotFound("github repo not found".into()))?;
    let old_owner = existing.owner;
    let old_repo = existing.repo;
    let conn_id = existing.connection_id;
    let (conn_name, conn_api_base) =
        crate::store::github::connection_name_and_base(&state.db(), &conn_id)
            .await?
            .ok_or_else(|| AppError::NotFound("github connection not found".into()))?;

    let new_owner = req.owner.unwrap_or_else(|| old_owner.clone());
    let new_repo = req.repo.unwrap_or_else(|| old_repo.clone());
    if new_owner.trim().is_empty() || new_repo.trim().is_empty() {
        return Err(AppError::ValidationFailed(
            "owner and repo are required".into(),
        ));
    }

    crate::store::github::update_repo(&state.db(), &id, &new_owner, &new_repo).await?;

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
    let removed = crate::store::github::repo_by_id(&state.db(), &id)
        .await?
        .ok_or_else(|| AppError::NotFound("github repo not found".into()))?;
    let repo_label = format!("{}/{}", removed.owner, removed.repo);
    if !crate::store::github::delete_repo(&state.db(), &id).await? {
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

    let (client, name) =
        connection_client(&state, &state.config.auth.jwt_secret, &req.connection_id).await?;

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
    crate::store::github::mark_fetched(&state.db(), &req.connection_id).await?;

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
    let (client, name) =
        connection_client(&state, &state.config.auth.jwt_secret, &req.connection_id).await?;
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
    let binds = crate::store::github_binds::list_secret_binds(&state.pool, &secret_id).await?;
    let out: Vec<serde_json::Value> = binds
        .into_iter()
        .map(|b| {
            bind_view(&BindView {
                id: &b.id,
                repo_id: &b.repo_id,
                connection_name: &b.connection_name,
                owner: &b.owner,
                repo: &b.repo,
                last_pushed_at: b.last_pushed_at,
                last_status: &b.last_status,
                last_error: &b.last_error,
                last_op: &b.last_op,
                // Never expose secret override plaintext over the API.
                override_value: None,
                has_override: b.has_override,
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

    let repo = crate::store::github_binds::repo_identity(&state.pool, &req.repo_id).await?;
    let (owner, repo_name, conn_name) = match repo {
        Some(r) => r,
        None => {
            // Binding failed — record it so the failure is traceable.
            let secret_name = crate::store::secrets::secret_name_optional(&state.db(), &secret_id)
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

    if crate::store::github_binds::secret_bind_exists(&state.pool, &secret_id, &req.repo_id).await?
    {
        let secret_name = crate::store::secrets::secret_name(&state.db(), &secret_id).await?;
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
    crate::store::github_binds::insert_secret_bind(
        &state.pool,
        &id,
        &secret_id,
        &req.repo_id,
        override_cipher.as_deref(),
    )
    .await?;

    let secret_name = crate::store::secrets::secret_name(&state.db(), &secret_id).await?;
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

    let row =
        crate::store::github_binds::secret_bind_patch_facts(&state.pool, &bind_id, &secret_id)
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

    crate::store::github_binds::set_secret_bind_override(
        &state.pool,
        &bind_id,
        &secret_id,
        override_cipher.as_deref(),
    )
    .await?;

    // Override change changes the effective push value — mark previously-ok binds stale.
    let marked = crate::store::github_binds::mark_secret_bind_stale_if_ok(&state.pool, &bind_id)
        .await
        .unwrap_or(false);

    let secret_name = crate::store::secrets::secret_name(&state.db(), &secret_id).await?;
    let repo_id: String = row.repo_id;
    let conn_name: String = row.connection_name;
    let owner: String = row.owner;
    let repo_name: String = row.repo;
    let last_pushed_at: Option<String> = row.last_pushed_at;
    let last_status: String = if marked {
        "stale".to_string()
    } else {
        row.last_status
    };
    let last_error: String = if marked {
        String::new()
    } else {
        row.last_error
    };
    let last_op: String = row.last_op;

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

    let target = crate::store::github_binds::secret_bind_target(&state.pool, &bind_id, &secret_id)
        .await?
        .ok_or_else(|| AppError::NotFound("secret github bind not found".into()))?;
    let (owner, repo, api_base, token_cipher, conn_name) = (
        target.owner,
        target.repo,
        target.api_base_url,
        target.token_ciphertext,
        target.connection_name,
    );

    let secret_name = crate::store::secrets::secret_name(&state.db(), &secret_id).await?;

    // Remove the value from GitHub Actions first; only unbind locally when the
    // remote cleanup succeeds. 404 is treated as success (never pushed).
    let token = crypto::decrypt(&token_cipher, &state.config.auth.jwt_secret)
        .map_err(|e| AppError::Internal(format!("decrypt connection token '{conn_name}': {e}")))?;
    let client = GitHubClient::new(&api_base, &token);
    if let Err(e) = client.delete_secret(&owner, &repo, &secret_name).await {
        // Record the failed unbind on the bind row so the UI can surface it;
        // the row is kept so the operator can fix the connection and retry.
        crate::store::github_binds::mark_secret_bind_unbind_failed(
            &state.pool,
            &bind_id,
            &secret_id,
            &format!("unbind failed: {e}"),
        )
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

    crate::store::github_binds::delete_secret_bind(&state.pool, &bind_id, &secret_id).await?;

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
    let (secret_name, stored, secret_team) =
        crate::store::secrets::push_facts(&state.db(), &secret_id).await?;
    let plaintext = crypto::decrypt(&stored, &state.config.auth.jwt_secret)
        .map_err(|e| AppError::Internal(format!("decrypt secret '{secret_name}': {e}")))?;
    validate_gh_name(&secret_name)?;

    let bind_ids = crate::store::github_binds::secret_bind_ids(
        &state.pool,
        &secret_id,
        query.bind_id.as_deref(),
    )
    .await?;
    if bind_ids.is_empty() {
        return Ok(Json(ApiResponse::success(serde_json::json!({
            "results": [],
        }))));
    }

    let mut results = Vec::new();
    for bind_id in &bind_ids {
        let target = match crate::store::github_binds::secret_bind_target(
            &state.pool,
            bind_id,
            &secret_id,
        )
        .await
        {
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
        let owner: String = target.owner;
        let repo: String = target.repo;
        let api_base: String = target.api_base_url;
        let token_cipher: String = target.token_ciphertext;
        let conn_name: String = target.connection_name;
        let override_cipher: Option<String> = target.override_ciphertext;

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
                crate::store::github_binds::mark_secret_bind_pushed_ok(&state.pool, bind_id)
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
                crate::store::github_binds::mark_secret_bind_push_failed(
                    &state.pool,
                    bind_id,
                    &e.to_string(),
                )
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

async fn variable_exists(state: &AppState, var_id: &str) -> Result<(), AppError> {
    if !crate::store::variables::exists(&state.db(), var_id).await? {
        return Err(AppError::NotFound("variable not found".into()));
    }
    Ok(())
}

pub async fn list_variable_binds(
    _auth: PlatformAdmin,
    Path(var_id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<Vec<serde_json::Value>>>, AppError> {
    variable_exists(&state, &var_id).await?;
    let binds = crate::store::github_binds::list_variable_binds(&state.pool, &var_id).await?;
    let out: Vec<serde_json::Value> = binds
        .into_iter()
        .map(|b| {
            bind_view(&BindView {
                id: &b.id,
                repo_id: &b.repo_id,
                connection_name: &b.connection_name,
                owner: &b.owner,
                repo: &b.repo,
                last_pushed_at: b.last_pushed_at,
                last_status: &b.last_status,
                last_error: &b.last_error,
                last_op: &b.last_op,
                override_value: b.override_value.as_deref(),
                has_override: b.has_override,
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
    variable_exists(&state, &var_id).await?;

    let repo = crate::store::github_binds::repo_identity(&state.pool, &req.repo_id).await?;
    let (owner, repo_name, conn_name) = match repo {
        Some(r) => r,
        None => {
            // Binding failed — record it so the failure is traceable.
            let var_name = crate::store::variables::variable_name_optional(&state.db(), &var_id)
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

    if crate::store::github_binds::variable_bind_exists(&state.pool, &var_id, &req.repo_id).await? {
        let var_name = crate::store::variables::variable_name(&state.db(), &var_id).await?;
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
    crate::store::github_binds::insert_variable_bind(
        &state.pool,
        &id,
        &var_id,
        &req.repo_id,
        req.override_value.as_deref(),
    )
    .await?;

    let var_name = crate::store::variables::variable_name(&state.db(), &var_id).await?;
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
    variable_exists(&state, &var_id).await?;

    let row = crate::store::github_binds::variable_bind_patch_facts(&state.pool, &bind_id, &var_id)
        .await?
        .ok_or_else(|| AppError::NotFound("variable github bind not found".into()))?;

    crate::store::github_binds::set_variable_bind_override(
        &state.pool,
        &bind_id,
        &var_id,
        req.override_value.as_deref(),
    )
    .await?;

    let marked = crate::store::github_binds::mark_variable_bind_stale_if_ok(&state.pool, &bind_id)
        .await
        .unwrap_or(false);

    let var_name = crate::store::variables::variable_name(&state.db(), &var_id).await?;
    let repo_id: String = row.repo_id;
    let conn_name: String = row.connection_name;
    let owner: String = row.owner;
    let repo_name: String = row.repo;
    let last_pushed_at: Option<String> = row.last_pushed_at;
    let last_status: String = if marked {
        "stale".to_string()
    } else {
        row.last_status
    };
    let last_error: String = if marked {
        String::new()
    } else {
        row.last_error
    };
    let last_op: String = row.last_op;
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
    variable_exists(&state, &var_id).await?;

    let target = crate::store::github_binds::variable_bind_target(&state.pool, &bind_id, &var_id)
        .await?
        .ok_or_else(|| AppError::NotFound("variable github bind not found".into()))?;
    let (owner, repo, api_base) = (target.owner, target.repo, target.api_base_url);
    let token_cipher = target.token_ciphertext;
    let conn_name = target.connection_name;

    let var_name = crate::store::variables::variable_name(&state.db(), &var_id).await?;

    // Remove the value from GitHub Actions first; only unbind locally when the
    // remote cleanup succeeds. 404 is treated as success (never pushed).
    let token = crypto::decrypt(&token_cipher, &state.config.auth.jwt_secret)
        .map_err(|e| AppError::Internal(format!("decrypt connection token '{conn_name}': {e}")))?;
    let client = GitHubClient::new(&api_base, &token);
    if let Err(e) = client.delete_variable(&owner, &repo, &var_name).await {
        // Record the failed unbind on the bind row so the UI can surface it;
        // the row is kept so the operator can fix the connection and retry.
        crate::store::github_binds::mark_variable_bind_unbind_failed(
            &state.pool,
            &bind_id,
            &var_id,
            &format!("unbind failed: {e}"),
        )
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

    crate::store::github_binds::delete_variable_bind(&state.pool, &bind_id, &var_id).await?;

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
    variable_exists(&state, &var_id).await?;

    let (var_name, value) = crate::store::variables::push_facts(&state.db(), &var_id).await?;
    validate_gh_name(&var_name)?;

    let bind_ids = crate::store::github_binds::variable_bind_ids(
        &state.pool,
        &var_id,
        query.bind_id.as_deref(),
    )
    .await?;
    if bind_ids.is_empty() {
        return Ok(Json(ApiResponse::success(serde_json::json!({
            "results": [],
        }))));
    }

    let mut results = Vec::new();
    for bind_id in &bind_ids {
        let target = match crate::store::github_binds::variable_bind_target(
            &state.pool,
            bind_id,
            &var_id,
        )
        .await
        {
            Ok(Some(target)) => target,
            Ok(None) => {
                results.push(serde_json::json!({ "bind_id": bind_id, "status": "failed", "error": "bind not found" }));
                continue;
            }
            Err(error) => {
                results.push(serde_json::json!({ "bind_id": bind_id, "status": "failed", "error": error.to_string() }));
                continue;
            }
        };
        let owner = target.owner;
        let repo = target.repo;
        let api_base = target.api_base_url;
        let token_cipher = target.token_ciphertext;
        let conn_name = target.connection_name;
        let push_value =
            effective_variable_push_value(&value, target.override_value.as_deref()).to_string();

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
                crate::store::github_binds::mark_variable_bind_pushed_ok(&state.pool, bind_id)
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
                crate::store::github_binds::mark_variable_bind_push_failed(
                    &state.pool,
                    bind_id,
                    &e.to_string(),
                )
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

/// Batch-load secret → binds map for list enrichment (single query, no N+1).
pub(crate) async fn secret_binds_map(
    state: &AppState,
    secret_ids: &[String],
) -> Result<std::collections::HashMap<String, Vec<serde_json::Value>>, AppError> {
    let mut map: std::collections::HashMap<String, Vec<serde_json::Value>> =
        std::collections::HashMap::new();
    for b in crate::store::github_binds::secret_bind_summaries(&state.pool, secret_ids).await? {
        map.entry(b.resource_id)
            .or_default()
            .push(bind_row_json(&BindView {
                id: &b.id,
                repo_id: &b.repo_id,
                connection_name: &b.connection_name,
                owner: &b.owner,
                repo: &b.repo,
                last_pushed_at: b.last_pushed_at,
                last_status: &b.last_status,
                last_error: &b.last_error,
                last_op: &b.last_op,
                // Never expose secret override plaintext over the API.
                override_value: None,
                has_override: b.override_value.is_some(),
            }));
    }
    Ok(map)
}

/// Batch-load variable → binds map for list enrichment.
pub(crate) async fn variable_binds_map(
    state: &AppState,
    var_ids: &[String],
) -> Result<std::collections::HashMap<String, Vec<serde_json::Value>>, AppError> {
    let mut map: std::collections::HashMap<String, Vec<serde_json::Value>> =
        std::collections::HashMap::new();
    for b in crate::store::github_binds::variable_bind_summaries(&state.pool, var_ids).await? {
        map.entry(b.resource_id)
            .or_default()
            .push(bind_row_json(&BindView {
                id: &b.id,
                repo_id: &b.repo_id,
                connection_name: &b.connection_name,
                owner: &b.owner,
                repo: &b.repo,
                last_pushed_at: b.last_pushed_at,
                last_status: &b.last_status,
                last_error: &b.last_error,
                last_op: &b.last_op,
                override_value: b.override_value.as_deref(),
                has_override: b.override_value.is_some(),
            }));
    }
    Ok(map)
}
