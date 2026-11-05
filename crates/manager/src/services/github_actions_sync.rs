//! GitHub Actions sync — REST client for repo-level Actions secrets/variables.
//!
//! Secrets are write-only on GitHub: values are sealed with a libsodium
//! compatible `crypto_box_seal` using the repo's public key and pushed via
//! `PUT`. Variables are plaintext via `POST`/`PATCH`. The Manager decrypts Buz
//! secrets server-side before sealing; plaintext never crosses the frontend.

use base64::Engine;
use serde::Deserialize;
use shared::error::AppError;
use uuid::Uuid;

/// Base64 decode helper (GitHub returns base64 public keys / we return base64
/// ciphertext).
pub fn base64_decode(raw: &str) -> Result<Vec<u8>, String> {
    base64::engine::general_purpose::STANDARD
        .decode(raw)
        .map_err(|e| format!("invalid base64: {e}"))
}

pub fn base64_encode(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// Seal `plaintext` with the GitHub repo's base64 X25519 public key using a
/// libsodium-compatible sealed box (`crypto_box_seal`). Returns base64
/// ciphertext for the `encrypted_value` field.
pub fn seal_secret_value(public_key_b64: &str, plaintext: &str) -> Result<String, String> {
    use crypto_box::{aead::OsRng, PublicKey};
    let key_bytes = base64_decode(public_key_b64)?;
    let key_len = key_bytes.len();
    let key_arr: [u8; 32] = key_bytes
        .try_into()
        .map_err(|_| format!("GitHub public key must be 32 bytes, got {key_len}"))?;
    let public_key = PublicKey::from(key_arr);
    let sealed = public_key
        .seal(&mut OsRng, plaintext.as_bytes())
        .map_err(|e| format!("sealed-box encrypt failed: {e}"))?;
    Ok(base64_encode(&sealed))
}

#[derive(Debug, Deserialize)]
pub struct RepoPublicKey {
    #[serde(rename = "key_id")]
    pub key_id: String,
    pub key: String,
}

#[derive(Debug, Deserialize)]
pub struct RepoRef {
    pub name: String,
    pub full_name: String,
}

#[derive(Debug, Deserialize)]
pub struct VariableItem {
    #[allow(dead_code)]
    pub name: String,
    #[allow(dead_code)]
    pub value: String,
}

/// Thin wrapper over the GitHub REST API (repo-level Actions endpoints).
#[derive(Debug, Clone)]
pub struct GitHubClient {
    api_base_url: String,
    /// Host of `api_base_url`, used to reject `Link: rel="next"` pagination
    /// URLs that would forward the PAT to a different host.
    base_host: String,
    token: String,
    http: reqwest::Client,
}

impl GitHubClient {
    pub fn new(api_base_url: &str, token: &str) -> Self {
        let api_base_url = api_base_url.trim().trim_end_matches('/').to_string();
        let base_host = url::Url::parse(&api_base_url)
            .ok()
            .and_then(|u| u.host_str().map(str::to_string))
            .unwrap_or_default();
        // GitHub rejects requests without a `User-Agent` header with a 403 —
        // reqwest's default client does NOT send one, so set it explicitly.
        // A request timeout prevents a hung endpoint from stalling the fetch
        // scheduler / handler tasks indefinitely.
        let http = crate::http_util::client_builder()
            .user_agent(concat!("buz-manager/", env!("CARGO_PKG_VERSION")))
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("reqwest client build cannot fail");
        Self {
            api_base_url,
            base_host,
            token: token.to_string(),
            http,
        }
    }

    fn auth_headers(&self) -> reqwest::header::HeaderMap {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::AUTHORIZATION,
            reqwest::header::HeaderValue::from_str(&format!("Bearer {}", self.token))
                .unwrap_or_else(|_| reqwest::header::HeaderValue::from_static("Bearer ")),
        );
        headers.insert(
            reqwest::header::ACCEPT,
            reqwest::header::HeaderValue::from_static("application/vnd.github+json"),
        );
        headers
    }

    /// Map a non-2xx GitHub response to a helpful AppError.
    async fn map_error(&self, resp: reqwest::Response) -> AppError {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        let detail = extract_gh_error_message(&body).unwrap_or(body);
        match status.as_u16() {
            401 => AppError::Forbidden(format!(
                "GitHub token rejected (401): {detail} — check the connection token"
            )),
            403 => AppError::Forbidden(format!(
                "GitHub permission denied (403): {detail} — token scope or fine-grained \
                 'Actions' write permission missing"
            )),
            404 => AppError::NotFound(format!("GitHub resource not found (404): {detail}")),
            409 => AppError::Conflict(format!("GitHub conflict (409): {detail}")),
            422 => {
                AppError::ValidationFailed(format!("GitHub rejected the request (422): {detail}"))
            }
            _ => AppError::Internal(format!("GitHub API error ({}): {detail}", status.as_u16())),
        }
    }

    /// `GET /repos/{owner}/{repo}/actions/secrets/public-key`
    pub async fn get_repo_public_key(
        &self,
        owner: &str,
        repo: &str,
    ) -> Result<RepoPublicKey, AppError> {
        let url = format!(
            "{}/repos/{owner}/{repo}/actions/secrets/public-key",
            self.api_base_url
        );
        let resp = self
            .http
            .get(&url)
            .headers(self.auth_headers())
            .send()
            .await
            .map_err(|e| AppError::Internal(format!("GitHub request failed: {e}")))?;
        if !resp.status().is_success() {
            return Err(self.map_error(resp).await);
        }
        resp.json().await.map_err(|e| {
            AppError::Internal(format!("GitHub public-key response parse failed: {e}"))
        })
    }

    /// `PUT /repos/{owner}/{repo}/actions/secrets/{name}` — create or overwrite.
    pub async fn put_secret(
        &self,
        owner: &str,
        repo: &str,
        name: &str,
        encrypted_value: &str,
        key_id: &str,
    ) -> Result<(), AppError> {
        let url = format!(
            "{}/repos/{owner}/{repo}/actions/secrets/{name}",
            self.api_base_url
        );
        let body = serde_json::json!({
            "encrypted_value": encrypted_value,
            "key_id": key_id,
        });
        let resp = self
            .http
            .put(&url)
            .headers(self.auth_headers())
            .json(&body)
            .send()
            .await
            .map_err(|e| AppError::Internal(format!("GitHub request failed: {e}")))?;
        if !resp.status().is_success() {
            return Err(self.map_error(resp).await);
        }
        Ok(())
    }

    /// `DELETE /repos/{owner}/{repo}/actions/secrets/{name}` — remove a secret
    /// from GitHub Actions. Idempotent: a 404 (already gone) is success, so
    /// unbinding a never-pushed bind still completes.
    pub async fn delete_secret(&self, owner: &str, repo: &str, name: &str) -> Result<(), AppError> {
        let url = format!(
            "{}/repos/{owner}/{repo}/actions/secrets/{name}",
            self.api_base_url
        );
        let resp = self
            .http
            .delete(&url)
            .headers(self.auth_headers())
            .send()
            .await
            .map_err(|e| AppError::Internal(format!("GitHub request failed: {e}")))?;
        match resp.status().as_u16() {
            204 => {
                tracing::info!(
                    owner = %owner,
                    repo = %repo,
                    secret = %name,
                    "github-sync: remote secret deleted"
                );
                Ok(())
            }
            404 => {
                tracing::info!(
                    owner = %owner,
                    repo = %repo,
                    secret = %name,
                    already_gone = true,
                    "github-sync: remote secret already absent, delete skipped"
                );
                Ok(())
            }
            _ => Err(self.map_error(resp).await),
        }
    }

    /// `DELETE /repos/{owner}/{repo}/actions/variables/{name}` — remove a
    /// variable from GitHub Actions. Idempotent: a 404 (already gone) is
    /// success, so unbinding a never-pushed bind still completes.
    pub async fn delete_variable(
        &self,
        owner: &str,
        repo: &str,
        name: &str,
    ) -> Result<(), AppError> {
        let url = format!(
            "{}/repos/{owner}/{repo}/actions/variables/{name}",
            self.api_base_url
        );
        let resp = self
            .http
            .delete(&url)
            .headers(self.auth_headers())
            .send()
            .await
            .map_err(|e| AppError::Internal(format!("GitHub request failed: {e}")))?;
        match resp.status().as_u16() {
            204 => {
                tracing::info!(
                    owner = %owner,
                    repo = %repo,
                    variable = %name,
                    "github-sync: remote variable deleted"
                );
                Ok(())
            }
            404 => {
                tracing::info!(
                    owner = %owner,
                    repo = %repo,
                    variable = %name,
                    already_gone = true,
                    "github-sync: remote variable already absent, delete skipped"
                );
                Ok(())
            }
            _ => Err(self.map_error(resp).await),
        }
    }

    /// Get a single repo variable, or `None` when it does not exist yet.
    pub async fn get_variable(
        &self,
        owner: &str,
        repo: &str,
        name: &str,
    ) -> Result<Option<VariableItem>, AppError> {
        let url = format!(
            "{}/repos/{owner}/{repo}/actions/variables/{name}",
            self.api_base_url
        );
        let resp = self
            .http
            .get(&url)
            .headers(self.auth_headers())
            .send()
            .await
            .map_err(|e| AppError::Internal(format!("GitHub request failed: {e}")))?;
        match resp.status().as_u16() {
            200 => resp
                .json()
                .await
                .map(Some)
                .map_err(|e| AppError::Internal(format!("GitHub variable parse failed: {e}"))),
            404 => Ok(None),
            _ => Err(self.map_error(resp).await),
        }
    }

    /// `POST /repos/{owner}/{repo}/actions/variables` (create).
    pub async fn create_variable(
        &self,
        owner: &str,
        repo: &str,
        name: &str,
        value: &str,
    ) -> Result<(), AppError> {
        let url = format!(
            "{}/repos/{owner}/{repo}/actions/variables",
            self.api_base_url
        );
        let resp = self
            .http
            .post(&url)
            .headers(self.auth_headers())
            .json(&serde_json::json!({ "name": name, "value": value }))
            .send()
            .await
            .map_err(|e| AppError::Internal(format!("GitHub request failed: {e}")))?;
        if !resp.status().is_success() {
            return Err(self.map_error(resp).await);
        }
        Ok(())
    }

    /// `PATCH /repos/{owner}/{repo}/actions/variables/{name}` (update value).
    pub async fn update_variable(
        &self,
        owner: &str,
        repo: &str,
        name: &str,
        value: &str,
    ) -> Result<(), AppError> {
        let url = format!(
            "{}/repos/{owner}/{repo}/actions/variables/{name}",
            self.api_base_url
        );
        let resp = self
            .http
            .patch(&url)
            .headers(self.auth_headers())
            .json(&serde_json::json!({ "value": value }))
            .send()
            .await
            .map_err(|e| AppError::Internal(format!("GitHub request failed: {e}")))?;
        if !resp.status().is_success() {
            return Err(self.map_error(resp).await);
        }
        Ok(())
    }

    /// Upsert a repo variable: `POST` when missing, `PATCH` when present.
    pub async fn upsert_variable(
        &self,
        owner: &str,
        repo: &str,
        name: &str,
        value: &str,
    ) -> Result<(), AppError> {
        if self.get_variable(owner, repo, name).await?.is_some() {
            self.update_variable(owner, repo, name, value).await
        } else {
            self.create_variable(owner, repo, name, value).await
        }
    }

    /// List the owner's repositories (org first, user fallback on 404).
    /// Returns repo full names (`owner/name`). GitHub paginates repo listings
    /// (default 30/page, max 100), so this follows `Link: rel="next"` until
    /// every page is drained — otherwise orgs with >30 repos silently lose
    /// the tail.
    pub async fn list_owner_repos(&self, owner: &str) -> Result<Vec<RepoRef>, AppError> {
        let org_url = format!(
            "{}/orgs/{owner}/repos?per_page=100&sort=full_name&direction=asc",
            self.api_base_url
        );
        let resp = self
            .http
            .get(&org_url)
            .headers(self.auth_headers())
            .send()
            .await
            .map_err(|e| AppError::Internal(format!("GitHub request failed: {e}")))?;
        if resp.status().is_success() {
            let page = self.parse_repo_pages(resp).await?;
            tracing::info!(
                owner = %owner,
                total = page.len(),
                "github-sync: listed owner repos via org endpoint"
            );
            return Ok(page);
        }

        let user_url = format!(
            "{}/users/{owner}/repos?per_page=100&sort=full_name&direction=asc",
            self.api_base_url
        );
        let resp = self
            .http
            .get(&user_url)
            .headers(self.auth_headers())
            .send()
            .await
            .map_err(|e| AppError::Internal(format!("GitHub request failed: {e}")))?;
        if resp.status().is_success() {
            let page = self.parse_repo_pages(resp).await?;
            tracing::info!(
                owner = %owner,
                total = page.len(),
                "github-sync: listed owner repos via user endpoint"
            );
            return Ok(page);
        }
        Err(self.map_error(resp).await)
    }

    /// Parse one repo-list response then keep following `Link: rel="next"`
    /// until the full listing is drained.
    async fn parse_repo_pages(&self, resp: reqwest::Response) -> Result<Vec<RepoRef>, AppError> {
        let mut all = Vec::new();
        let mut url = parse_next_link(
            resp.headers()
                .get(reqwest::header::LINK)
                .and_then(|v| v.to_str().ok()),
        )
        .map(|s| s.to_string());
        let mut page = resp
            .json::<Vec<RepoRef>>()
            .await
            .map_err(|e| AppError::Internal(format!("GitHub repo list parse failed: {e}")))?;
        tracing::info!(
            page_len = page.len(),
            has_next = url.is_some(),
            "github-sync: repo list page received"
        );
        all.append(&mut page);

        while let Some(next_url) = url {
            if !self.next_url_is_safe(&next_url) {
                tracing::warn!(
                    url = %next_url,
                    base_host = %self.base_host,
                    "github-sync: ignoring Link: rel=\"next\" pointing outside the configured base host"
                );
                break;
            }
            let resp = self
                .http
                .get(&next_url)
                .headers(self.auth_headers())
                .send()
                .await
                .map_err(|e| AppError::Internal(format!("GitHub request failed: {e}")))?;
            url = parse_next_link(
                resp.headers()
                    .get(reqwest::header::LINK)
                    .and_then(|v| v.to_str().ok()),
            )
            .map(|s| s.to_string());
            let mut page = resp
                .json::<Vec<RepoRef>>()
                .await
                .map_err(|e| AppError::Internal(format!("GitHub repo list parse failed: {e}")))?;
            tracing::info!(
                page_len = page.len(),
                has_next = url.is_some(),
                "github-sync: repo list page received (next)"
            );
            all.append(&mut page);
        }
        Ok(all)
    }

    /// Only follow pagination links that stay on the configured GitHub base
    /// host — a compromised or misbehaving endpoint must not be able to point
    /// the PAT at an arbitrary host.
    fn next_url_is_safe(&self, next: &str) -> bool {
        let Ok(u) = url::Url::parse(next) else {
            return false;
        };
        let scheme_ok = u.scheme() == "https"
            || (u.scheme() == "http"
                && matches!(u.host_str(), Some("localhost" | "127.0.0.1" | "[::1]")));
        scheme_ok && u.host_str() == Some(self.base_host.as_str())
    }

    /// Connectivity + permission check. `repo` optional: without it, verify the
    /// token by listing the owner's repos.
    pub async fn test_connection(&self, owner: &str, repo: Option<&str>) -> Result<(), AppError> {
        if let Some(repo) = repo {
            // `GET /repos/{owner}/{repo}` — 200 means token can read the repo.
            let url = format!("{}/repos/{owner}/{repo}", self.api_base_url);
            let resp = self
                .http
                .get(&url)
                .headers(self.auth_headers())
                .send()
                .await
                .map_err(|e| AppError::Internal(format!("GitHub request failed: {e}")))?;
            if !resp.status().is_success() {
                return Err(self.map_error(resp).await);
            }
            Ok(())
        } else {
            self.list_owner_repos(owner).await.map(|_| ())
        }
    }

    /// Token-only check: `GET /user` returns 200 with the authenticated
    /// account's `login` when the token is valid (and the account is active).
    pub async fn test_token(&self) -> Result<String, AppError> {
        let url = format!("{}/user", self.api_base_url);
        let resp = self
            .http
            .get(&url)
            .headers(self.auth_headers())
            .send()
            .await
            .map_err(|e| AppError::Internal(format!("GitHub request failed: {e}")))?;
        if !resp.status().is_success() {
            return Err(self.map_error(resp).await);
        }
        let login: String = resp
            .json::<serde_json::Value>()
            .await
            .map_err(|e| AppError::Internal(format!("GitHub user parse failed: {e}")))?
            .get("login")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        tracing::info!(login = %login, "github-sync: token validated via /user");
        Ok(login)
    }
}

/// Extract GitHub's `{"message": "..."}` error detail, if present.
fn extract_gh_error_message(body: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    v.get("message").and_then(|m| m.as_str()).map(String::from)
}

/// Fetch an owner's repos from GitHub and upsert them under a connection.
///
/// Shared by the manual `POST /github-sync/repos/fetch` handler and the
/// auto-fetch scheduler so both produce identical logs. Returns
/// `(added, existing)`.
pub async fn fetch_and_upsert_repos(
    pool: &sqlx::SqlitePool,
    connection_id: &str,
    connection_name: &str,
    owner: &str,
    client: &GitHubClient,
) -> Result<(u32, u32), AppError> {
    let repos = client.list_owner_repos(owner).await?;

    tracing::info!(
        connection = %connection_name,
        connection_id,
        owner = %owner,
        fetched = repos.len(),
        "github-sync: owner repo list fetched"
    );
    for r in &repos {
        tracing::debug!(repo = %r.full_name, "github-sync: fetched repo entry");
    }

    let mut added = 0u32;
    let mut existing = 0u32;
    for r in &repos {
        let (owner_part, name) = r.full_name.split_once('/').unwrap_or((owner, &r.name));
        let owner_part = owner_part.to_string();
        let name = name.to_string();
        let dup = sqlx::query(
            "SELECT id FROM github_repos WHERE connection_id = ? AND owner = ? AND repo = ?",
        )
        .bind(connection_id)
        .bind(&owner_part)
        .bind(&name)
        .fetch_optional(pool)
        .await?;
        if dup.is_some() {
            existing += 1;
            tracing::debug!(
                connection = %connection_name,
                repo = %format!("{owner_part}/{name}"),
                "github-sync: repo already registered, skipping"
            );
            continue;
        }
        let id = Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO github_repos (id, connection_id, owner, repo) VALUES (?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(connection_id)
        .bind(&owner_part)
        .bind(&name)
        .execute(pool)
        .await?;
        added += 1;
        tracing::info!(
            connection = %connection_name,
            repo = %format!("{owner_part}/{name}"),
            "github-sync: repo registered"
        );
    }

    tracing::info!(
        connection = %connection_name,
        owner = %owner,
        added,
        existing,
        "github-sync: fetch complete"
    );
    Ok((added, existing))
}

/// Parse the `rel="next"` URL from a GitHub `Link` response header.
fn parse_next_link(link: Option<&str>) -> Option<&str> {
    link?.split(',').find_map(|part| {
        let mut it = part.split(';');
        let url = it
            .next()?
            .trim()
            .trim_start_matches('<')
            .trim_end_matches('>');
        let rel = it.next()?.trim();
        (rel == "rel=\"next\"").then_some(url)
    })
}
