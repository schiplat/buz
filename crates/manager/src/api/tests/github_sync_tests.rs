//! Integration tests for GitHub Actions sync (connections, repos, binds, push).
//! Included via `api/mod.rs` under `#[cfg(test)]`.
//!
//! A local mock GitHub HTTP server verifies the wire protocol: the Manager must
//! call `public-key`, seal the plaintext with a libsodium compatible box, and
//! PUT the encrypted value. The mock owns the recipient keypair, so the test
//! proves the encrypted value actually decrypts to the stored secret.

use std::sync::{Arc, Mutex};

use axum::{
    body::Body,
    http::{header::AUTHORIZATION, Request, StatusCode},
    Router,
};
use base64::Engine;
use crypto_box::aead::OsRng;
use http_body_util::BodyExt;
use shared::auth::{Role, TokenManager};
use sqlx::sqlite::SqlitePoolOptions;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tower::ServiceExt;

use crate::api;
use crate::config::{AuthConfig, Config, PipelineConfig, RunnerConfig, ServerConfig};
use crate::db::run_migrations;
use crate::services::log_broadcast::LogBroadcaster;
use crate::state::AppState;

const JWT_SECRET: &str = "test-secret-0123456789-abcdefghijklmnop";
const SUPER_ADMIN_ID: &str = "admin-1";
const REGULAR_USER_ID: &str = "user-1";

fn test_config() -> Config {
    Config {
        server: ServerConfig::default(),
        database: Default::default(),
        auth: AuthConfig {
            jwt_secret: JWT_SECRET.into(),
            jwt_ttl_hours: 24,
            runner_register_token_ttl_hours: 24,
        },
        runner: RunnerConfig::default(),
        pipeline: PipelineConfig {
            yaml_scan_dir: "/tmp/empty-buz-scan".into(),
            yaml_scan_interval_seconds: 3600,
        },
        standalone: Default::default(),
        logging: Default::default(),
    }
}

async fn test_app() -> Router {
    let pool = SqlitePoolOptions::new()
        .max_connections(4)
        .connect("sqlite::memory:")
        .await
        .expect("open in-memory db");
    run_migrations(&pool, JWT_SECRET).await.expect("migrate db");

    sqlx::query(
        "INSERT INTO users (id, username, password_hash, display_name, is_platform_admin, is_super_admin, is_active)
         VALUES (?, ?, 'hash', ?, 1, 1, 1)",
    )
    .bind(SUPER_ADMIN_ID)
    .bind("boss")
    .bind("boss")
    .execute(&pool)
    .await
    .expect("seed admin");

    sqlx::query(
        "INSERT INTO users (id, username, password_hash, display_name, is_platform_admin, is_super_admin, is_active)
         VALUES (?, ?, 'hash', ?, 0, 0, 1)",
    )
    .bind(REGULAR_USER_ID)
    .bind("alice")
    .bind("alice")
    .execute(&pool)
    .await
    .expect("seed user");

    sqlx::query("INSERT INTO teams (id, name, slug) VALUES ('team-1', 'Default', 'default')")
        .execute(&pool)
        .await
        .expect("seed team");

    let state = AppState {
        pool,
        config: test_config(),
        log_broadcast: LogBroadcaster::new(),
    };
    Router::new().nest("/api/v1", api::router(state))
}

fn admin_jwt() -> String {
    TokenManager::new(JWT_SECRET)
        .create_token(SUPER_ADMIN_ID, Role::Admin, 1)
        .expect("mint jwt")
}

fn regular_jwt() -> String {
    TokenManager::new(JWT_SECRET)
        .create_token(REGULAR_USER_ID, Role::User, 1)
        .expect("mint jwt")
}

async fn read_body(response: axum::response::Response) -> serde_json::Value {
    let body = response
        .into_body()
        .collect()
        .await
        .expect("read body")
        .to_bytes();
    serde_json::from_slice(&body).expect("json body")
}

async fn post_json(
    app: &Router,
    token: &str,
    uri: &str,
    payload: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header(AUTHORIZATION, format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(payload.to_string()))
                .expect("build request"),
        )
        .await
        .expect("send request");
    let status = resp.status();
    let body = read_body(resp).await;
    (status, body)
}

async fn get_json(app: &Router, token: &str, uri: &str) -> (StatusCode, serde_json::Value) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(uri)
                .header(AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .expect("build request"),
        )
        .await
        .expect("send request");
    let status = resp.status();
    let body = read_body(resp).await;
    (status, body)
}

async fn delete_json(app: &Router, token: &str, uri: &str) -> (StatusCode, serde_json::Value) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(uri)
                .header(AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .expect("build request"),
        )
        .await
        .expect("send request");
    let status = resp.status();
    let body = read_body(resp).await;
    (status, body)
}

async fn put_json(
    app: &Router,
    token: &str,
    uri: &str,
    payload: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(uri)
                .header(AUTHORIZATION, format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(payload.to_string()))
                .expect("build request"),
        )
        .await
        .expect("send request");
    let status = resp.status();
    let body = read_body(resp).await;
    (status, body)
}

async fn patch_json(
    app: &Router,
    token: &str,
    uri: &str,
    payload: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(uri)
                .header(AUTHORIZATION, format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(payload.to_string()))
                .expect("build request"),
        )
        .await
        .expect("send request");
    let status = resp.status();
    let body = read_body(resp).await;
    (status, body)
}

// ─────────────────────────── mock GitHub server ───────────────────────────

#[derive(Debug, Clone)]
struct MockRequest {
    method: String,
    path: String,
    body: String,
    user_agent: String,
}

struct MockGitHub {
    addr: std::net::SocketAddr,
    requests: Arc<Mutex<Vec<MockRequest>>>,
    public_key_b64: String,
    secret_key: crypto_box::SecretKey,
}

impl MockGitHub {
    async fn start() -> Self {
        let secret_key = crypto_box::SecretKey::generate(&mut OsRng);
        let public_key_b64 =
            base64::engine::general_purpose::STANDARD.encode(secret_key.public_key().as_bytes());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock");
        let addr = listener.local_addr().expect("addr");
        let requests: Arc<Mutex<Vec<MockRequest>>> = Arc::new(Mutex::new(Vec::new()));
        let owned_repos: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(vec![(
            "myorg".to_string(),
            "alpha-repo".to_string(),
        )]));

        let requests_clone = requests.clone();
        let owned_clone = owned_repos.clone();
        let pubkey = public_key_b64.clone();
        let base_url = format!("http://{addr}");
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                let requests = requests_clone.clone();
                let owned = owned_clone.clone();
                let pubkey = pubkey.clone();
                let base_url = base_url.clone();
                tokio::spawn(async move {
                    let _ =
                        handle_mock_conn(&mut stream, &pubkey, &owned, &requests, &base_url).await;
                });
            }
        });

        Self {
            addr,
            requests,
            public_key_b64,
            secret_key,
        }
    }

    fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    fn snapshot(&self) -> Vec<MockRequest> {
        self.requests.lock().expect("lock").clone()
    }

    fn clear_requests(&self) {
        self.requests.lock().expect("lock").clear();
    }

    fn unseal_b64(&self, b64: &str) -> String {
        let raw = base64::engine::general_purpose::STANDARD
            .decode(b64)
            .expect("decode ciphertext");
        let plain = self.secret_key.unseal(&raw).expect("unseal ciphertext");
        String::from_utf8(plain).expect("utf8 plaintext")
    }
}

async fn handle_mock_conn(
    stream: &mut tokio::net::TcpStream,
    public_key_b64: &str,
    owned: &Arc<Mutex<Vec<(String, String)>>>,
    requests: &Arc<Mutex<Vec<MockRequest>>>,
    base_url: &str,
) -> std::io::Result<()> {
    // Read request head (until \r\n\r\n) then body by Content-Length.
    let mut buf = Vec::new();
    let mut tmp = [0u8; 1024];
    let head_end;
    loop {
        let n = stream.read(&mut tmp).await?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(pos) = find_subslice(&buf, b"\r\n\r\n") {
            head_end = pos + 4;
            break;
        }
        if buf.len() > 64 * 1024 {
            return Ok(());
        }
    }

    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or_default().to_string();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let path = parts.next().unwrap_or_default().to_string();

    let mut content_length = 0usize;
    let mut user_agent = String::new();
    for line in lines {
        if line.to_ascii_lowercase().starts_with("content-length:") {
            content_length = line
                .split_once(':')
                .and_then(|(_, v)| v.trim().parse().ok())
                .unwrap_or(0);
        } else if line.to_ascii_lowercase().starts_with("user-agent:") {
            user_agent = line
                .split_once(':')
                .map(|(_, v)| v.trim().to_string())
                .unwrap_or_default();
        }
    }
    while buf.len() < head_end + content_length {
        let n = stream.read(&mut tmp).await?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
    }
    let body = String::from_utf8_lossy(&buf[head_end..head_end + content_length]).to_string();

    requests.lock().expect("lock").push(MockRequest {
        method: method.clone(),
        path: path.clone(),
        body: body.clone(),
        user_agent,
    });

    // Route: secret public-key, secret PUT/DELETE, variable GET/POST/PATCH/DELETE, repo lists.
    // Returns (status line, optional Link header value, body).
    let (status, link, resp_body) = if path.ends_with("/actions/secrets/public-key") {
        (
            "200 OK",
            String::new(),
            format!(r#"{{"key_id":"k1","key":"{public_key_b64}"}}"#),
        )
    } else if method == "PUT" && path.contains("/actions/secrets/") {
        ("201 Created", String::new(), String::new())
    } else if method == "DELETE" && path.contains("/actions/secrets/") {
        // `MISSING_`-prefixed names simulate an already-absent remote secret
        // (404 is treated as success by the client — idempotent unbind).
        // `FAIL_DELETE_`-prefixed names simulate a hard remote failure (500),
        // which must leave the local bind in place with a recorded error.
        if path.contains("FAIL_DELETE_") {
            (
                "500 Internal Server Error",
                String::new(),
                r#"{"message":"Internal Server Error"}"#.to_string(),
            )
        } else if path.contains("MISSING_") {
            (
                "404 Not Found",
                String::new(),
                r#"{"message":"Not Found"}"#.to_string(),
            )
        } else {
            ("204 No Content", String::new(), String::new())
        }
    } else if method == "GET" && path.contains("/actions/variables/") {
        // Variable GET by name → not found (forces POST path).
        (
            "404 Not Found",
            String::new(),
            r#"{"message":"Not Found"}"#.to_string(),
        )
    } else if method == "POST" && path.ends_with("/actions/variables") {
        ("201 Created", String::new(), String::new())
    } else if method == "PATCH" && path.contains("/actions/variables/") {
        ("204 No Content", String::new(), String::new())
    } else if method == "DELETE" && path.contains("/actions/variables/") {
        ("204 No Content", String::new(), String::new())
    } else if path.starts_with("/orgs/myorg/repos") {
        let list = owned
            .lock()
            .expect("lock")
            .iter()
            .map(|(o, r)| serde_json::json!({ "name": r, "full_name": format!("{o}/{r}") }))
            .collect::<Vec<_>>();
        (
            "200 OK",
            String::new(),
            serde_json::to_string(&list).expect("json"),
        )
    } else if path.starts_with("/orgs/pagedorg/repos") {
        // Two pages: page 1 returns two repos + a `next` link, page 2 returns
        // the tail — verifies the client drains paginated listings.
        let page_two = path.contains("page=2");
        let list = if page_two {
            vec![serde_json::json!({ "name": "gamma", "full_name": "pagedorg/gamma" })]
        } else {
            vec![
                serde_json::json!({ "name": "alpha", "full_name": "pagedorg/alpha" }),
                serde_json::json!({ "name": "beta", "full_name": "pagedorg/beta" }),
            ]
        };
        let link = if page_two {
            String::new()
        } else {
            format!("<{}/orgs/pagedorg/repos?page=2>; rel=\"next\"", base_url)
        };
        ("200 OK", link, serde_json::to_string(&list).expect("json"))
    } else if path.starts_with("/orgs/badnext/repos") {
        // `Link: rel="next"` points at a foreign host — the client must refuse
        // to follow it (PAT must never be forwarded off-host).
        let list = vec![
            serde_json::json!({ "name": "alpha", "full_name": "badnext/alpha" }),
            serde_json::json!({ "name": "beta", "full_name": "badnext/beta" }),
        ];
        (
            "200 OK",
            "<http://169.254.169.254/steal>; rel=\"next\"".to_string(),
            serde_json::to_string(&list).expect("json"),
        )
    } else if path == "/repos/myorg/alpha-repo" {
        (
            "200 OK",
            String::new(),
            r#"{"name":"alpha-repo"}"#.to_string(),
        )
    } else if path == "/user" {
        (
            "200 OK",
            String::new(),
            r#"{"login":"mockuser"}"#.to_string(),
        )
    } else {
        (
            "404 Not Found",
            String::new(),
            r#"{"message":"Not Found"}"#.to_string(),
        )
    };

    let link_hdr = if link.is_empty() {
        String::new()
    } else {
        format!("Link: {link}\r\n")
    };
    let resp = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\n{link_hdr}Content-Length: {}\r\nConnection: close\r\n\r\n{resp_body}",
        resp_body.len()
    );
    stream.write_all(resp.as_bytes()).await?;
    stream.flush().await?;
    Ok(())
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

// ─────────────────────────────── tests ───────────────────────────────

/// Libsodium-compatible sealing round-trip: Buz seals with the repo public key
/// and the mock (holding the private key) must recover the plaintext.
#[tokio::test]
async fn seal_roundtrip_with_recipient_private_key() {
    let mock = MockGitHub::start().await;
    let sealed = crate::services::github_actions_sync::seal_secret_value(
        &mock.public_key_b64,
        "supersecret-value",
    )
    .expect("seal");
    let raw = base64::engine::general_purpose::STANDARD
        .decode(&sealed)
        .expect("b64");
    // 32-byte ephemeral pubkey + ciphertext + 16-byte poly1305 tag.
    assert_eq!(raw.len(), 32 + "supersecret-value".len() + 16);
    assert_eq!(mock.unseal_b64(&sealed), "supersecret-value");
}

/// Non-admin cannot manage connections.
#[tokio::test]
async fn connections_require_platform_admin() {
    let app = test_app().await;
    let (status, body) = post_json(
        &app,
        &regular_jwt(),
        "/api/v1/github-sync/connections",
        serde_json::json!({ "name": "gh", "token": "tok" }),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
}

/// Full secret flow: connection → repo → secret → bind → push.
/// Verifies the encrypted value on the wire decrypts to the stored secret.
#[tokio::test]
async fn push_secret_to_repo_uses_sealed_value() {
    let app = test_app().await;
    let token = admin_jwt();
    let mock = MockGitHub::start().await;

    // Connection
    let (status, body) = post_json(
        &app,
        &token,
        "/api/v1/github-sync/connections",
        serde_json::json!({ "name": "gh", "api_base_url": mock.base_url(), "token": "pat" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let conn_id = body["data"]["id"].as_str().expect("conn id").to_string();

    // Repo
    let (status, body) = post_json(
        &app,
        &token,
        "/api/v1/github-sync/repos",
        serde_json::json!({ "connection_id": conn_id, "owner": "myorg", "repo": "alpha-repo" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let repo_id = body["data"]["id"].as_str().expect("repo id").to_string();

    // Secret
    let (status, body) = post_json(
        &app,
        &token,
        "/api/v1/secrets",
        serde_json::json!({ "name": "MY_SECRET", "value": "supersecret-value", "team_id": "team-1" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let secret_id = body["data"]["id"].as_str().expect("secret id").to_string();

    // List enrichment includes empty binds.
    let (status, body) = get_json(&app, &token, "/api/v1/secrets").await;
    assert_eq!(status, StatusCode::OK);
    let row = body["data"]
        .as_array()
        .expect("arr")
        .iter()
        .find(|r| r["id"] == secret_id)
        .expect("row");
    assert!(row["github_binds"].is_array());
    assert_eq!(row["github_binds"].as_array().expect("binds").len(), 0);

    // Bind
    let (status, body) = post_json(
        &app,
        &token,
        &format!("/api/v1/secrets/{secret_id}/github-binds"),
        serde_json::json!({ "repo_id": repo_id }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let bind_id = body["data"]["id"].as_str().expect("bind id").to_string();

    // List enrichment now shows the bound repo metadata (no values).
    let (status, body) = get_json(&app, &token, "/api/v1/secrets").await;
    assert_eq!(status, StatusCode::OK);
    let row = body["data"]
        .as_array()
        .expect("arr")
        .iter()
        .find(|r| r["id"] == secret_id)
        .expect("row");
    let binds = row["github_binds"].as_array().expect("binds");
    assert_eq!(binds.len(), 1);
    assert_eq!(binds[0]["owner"], "myorg");
    assert_eq!(binds[0]["repo"], "alpha-repo");
    assert_eq!(binds[0]["last_status"], "pending");

    // Push
    let (status, body) = post_json(
        &app,
        &token,
        &format!("/api/v1/secrets/{secret_id}/github-binds/push"),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["results"][0]["status"], "ok", "{body}");
    assert_eq!(body["data"]["results"][0]["repo"], "alpha-repo");

    // The mock saw public-key GET + secret PUT; the encrypted value must
    // decrypt back to the stored plaintext. GitHub 403s requests without a
    // User-Agent header, so assert every call carries one.
    let reqs = mock.snapshot();
    assert!(
        reqs.iter()
            .all(|r| r.user_agent.starts_with("buz-manager/")),
        "every GitHub request must send a User-Agent, got: {reqs:?}"
    );
    let put = reqs
        .iter()
        .find(|r| r.method == "PUT" && r.path.contains("/actions/secrets/"))
        .expect("secret PUT seen");
    assert!(put.path.ends_with("/actions/secrets/MY_SECRET"));
    let wire: serde_json::Value = serde_json::from_str(&put.body).expect("put body json");
    assert_eq!(wire["key_id"], "k1");
    assert_eq!(
        mock.unseal_b64(wire["encrypted_value"].as_str().expect("enc")),
        "supersecret-value"
    );

    // DB bind status flipped to ok.
    let (status, body) = get_json(
        &app,
        &token,
        &format!("/api/v1/secrets/{secret_id}/github-binds"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"][0]["id"], bind_id);
    assert_eq!(body["data"][0]["last_status"], "ok");
    assert_eq!(body["data"][0]["last_op"], "push");
    assert!(body["data"][0]["last_pushed_at"].is_string());
}

/// Per-repo secret override is encrypted at rest and sealed to GitHub on push.
#[tokio::test]
async fn push_secret_uses_bind_override_value() {
    let app = test_app().await;
    let token = admin_jwt();
    let mock = MockGitHub::start().await;

    let (status, body) = post_json(
        &app,
        &token,
        "/api/v1/github-sync/connections",
        serde_json::json!({ "name": "gh", "api_base_url": mock.base_url(), "token": "pat" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let conn_id = body["data"]["id"].as_str().expect("conn id").to_string();

    let (status, body) = post_json(
        &app,
        &token,
        "/api/v1/github-sync/repos",
        serde_json::json!({ "connection_id": conn_id, "owner": "myorg", "repo": "alpha-repo" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let repo_id = body["data"]["id"].as_str().expect("repo id").to_string();

    let (status, body) = post_json(
        &app,
        &token,
        "/api/v1/secrets",
        serde_json::json!({ "name": "ACR_TOKEN", "value": "shared-default-secret", "team_id": "team-1" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let secret_id = body["data"]["id"].as_str().expect("secret id").to_string();

    let (status, body) = post_json(
        &app,
        &token,
        &format!("/api/v1/secrets/{secret_id}/github-binds"),
        serde_json::json!({ "repo_id": repo_id, "override_value": "per-repo-secret" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["has_override"], true);
    assert!(body["data"]["override_value"].is_null(), "{body}");
    let bind_id = body["data"]["id"].as_str().expect("bind id").to_string();

    let (status, body) = post_json(
        &app,
        &token,
        &format!("/api/v1/secrets/{secret_id}/github-binds/push?bind_id={bind_id}"),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["results"][0]["status"], "ok", "{body}");

    let reqs = mock.snapshot();
    let put = reqs
        .iter()
        .find(|r| r.method == "PUT" && r.path.contains("/actions/secrets/"))
        .expect("secret PUT seen");
    let wire: serde_json::Value = serde_json::from_str(&put.body).expect("put body json");
    assert_eq!(
        mock.unseal_b64(wire["encrypted_value"].as_str().expect("enc")),
        "per-repo-secret"
    );

    // Clear override → push default again.
    let (status, body) = patch_json(
        &app,
        &token,
        &format!("/api/v1/secrets/{secret_id}/github-binds/{bind_id}"),
        serde_json::json!({ "override_value": null }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["has_override"], false);

    mock.clear_requests();
    let (status, body) = post_json(
        &app,
        &token,
        &format!("/api/v1/secrets/{secret_id}/github-binds/push?bind_id={bind_id}"),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["results"][0]["status"], "ok", "{body}");

    let reqs = mock.snapshot();
    let put = reqs
        .iter()
        .find(|r| r.method == "PUT" && r.path.contains("/actions/secrets/"))
        .expect("secret PUT after inherit");
    let wire: serde_json::Value = serde_json::from_str(&put.body).expect("put body json");
    assert_eq!(
        mock.unseal_b64(wire["encrypted_value"].as_str().expect("enc")),
        "shared-default-secret"
    );
}

/// Variable flow: create → bind → push → POST (create) variable on GitHub.
#[tokio::test]
async fn push_variable_creates_on_github_when_missing() {
    let app = test_app().await;
    let token = admin_jwt();
    let mock = MockGitHub::start().await;

    let (status, body) = post_json(
        &app,
        &token,
        "/api/v1/github-sync/connections",
        serde_json::json!({ "name": "gh", "api_base_url": mock.base_url(), "token": "pat" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let conn_id = body["data"]["id"].as_str().expect("conn id").to_string();

    let (status, body) = post_json(
        &app,
        &token,
        "/api/v1/github-sync/repos",
        serde_json::json!({ "connection_id": conn_id, "owner": "myorg", "repo": "alpha-repo" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let repo_id = body["data"]["id"].as_str().expect("repo id").to_string();

    let (status, body) = post_json(
        &app,
        &token,
        "/api/v1/variables",
        serde_json::json!({ "name": "MY_VAR", "value": "var-value" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let var_id = body["data"]["id"].as_str().expect("var id").to_string();

    let (status, body) = post_json(
        &app,
        &token,
        &format!("/api/v1/variables/{var_id}/github-binds"),
        serde_json::json!({ "repo_id": repo_id }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, body) = post_json(
        &app,
        &token,
        &format!("/api/v1/variables/{var_id}/github-binds/push"),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["results"][0]["status"], "ok", "{body}");

    // List enrichment: variable carries its bind summary.
    let (status, body) = get_json(&app, &token, "/api/v1/variables").await;
    assert_eq!(status, StatusCode::OK);
    let row = body["data"]
        .as_array()
        .expect("arr")
        .iter()
        .find(|r| r["id"] == var_id)
        .expect("row");
    assert_eq!(row["github_binds"][0]["last_status"], "ok");

    let reqs = mock.snapshot();
    let created = reqs
        .iter()
        .find(|r| r.method == "POST" && r.path.ends_with("/actions/variables"))
        .expect("variable POST seen");
    let wire: serde_json::Value = serde_json::from_str(&created.body).expect("json");
    assert_eq!(wire["name"], "MY_VAR");
    assert_eq!(wire["value"], "var-value");
}

/// Per-repo override_value is pushed instead of the platform default.
#[tokio::test]
async fn push_variable_uses_bind_override_value() {
    let app = test_app().await;
    let token = admin_jwt();
    let mock = MockGitHub::start().await;

    let (status, body) = post_json(
        &app,
        &token,
        "/api/v1/github-sync/connections",
        serde_json::json!({ "name": "gh", "api_base_url": mock.base_url(), "token": "pat" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let conn_id = body["data"]["id"].as_str().expect("conn id").to_string();

    let (status, body) = post_json(
        &app,
        &token,
        "/api/v1/github-sync/repos",
        serde_json::json!({ "connection_id": conn_id, "owner": "myorg", "repo": "alpha-repo" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let repo_id = body["data"]["id"].as_str().expect("repo id").to_string();

    let (status, body) = post_json(
        &app,
        &token,
        "/api/v1/variables",
        serde_json::json!({ "name": "ACR_REPOSITORY", "value": "shared-default" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let var_id = body["data"]["id"].as_str().expect("var id").to_string();

    let (status, body) = post_json(
        &app,
        &token,
        &format!("/api/v1/variables/{var_id}/github-binds"),
        serde_json::json!({ "repo_id": repo_id, "override_value": "example/alpha" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["override_value"], "example/alpha");
    let bind_id = body["data"]["id"].as_str().expect("bind id").to_string();

    let (status, body) = post_json(
        &app,
        &token,
        &format!("/api/v1/variables/{var_id}/github-binds/push?bind_id={bind_id}"),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["results"][0]["status"], "ok", "{body}");

    let reqs = mock.snapshot();
    let created = reqs
        .iter()
        .find(|r| r.method == "POST" && r.path.ends_with("/actions/variables"))
        .expect("variable POST seen");
    let wire: serde_json::Value = serde_json::from_str(&created.body).expect("json");
    assert_eq!(wire["name"], "ACR_REPOSITORY");
    assert_eq!(wire["value"], "example/alpha");

    // Clear override → inherit default on next push.
    // Mock always 404s GET-by-name, so upsert uses POST again — value must be the default.
    let (status, body) = patch_json(
        &app,
        &token,
        &format!("/api/v1/variables/{var_id}/github-binds/{bind_id}"),
        serde_json::json!({ "override_value": null }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["data"]["override_value"].is_null(), "{body}");

    mock.clear_requests();
    let (status, body) = post_json(
        &app,
        &token,
        &format!("/api/v1/variables/{var_id}/github-binds/push?bind_id={bind_id}"),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["results"][0]["status"], "ok", "{body}");

    let reqs = mock.snapshot();
    let created = reqs
        .iter()
        .find(|r| r.method == "POST" && r.path.ends_with("/actions/variables"))
        .expect("variable POST seen after inherit");
    let wire: serde_json::Value = serde_json::from_str(&created.body).expect("json");
    assert_eq!(wire["value"], "shared-default");
}

/// Fetch repos from an org and test-connection round-trips.
#[tokio::test]
async fn fetch_repos_and_test_connection() {
    let app = test_app().await;
    let token = admin_jwt();
    let mock = MockGitHub::start().await;

    let (status, body) = post_json(
        &app,
        &token,
        "/api/v1/github-sync/connections",
        serde_json::json!({ "name": "gh", "api_base_url": mock.base_url(), "token": "pat" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let conn_id = body["data"]["id"].as_str().expect("conn id").to_string();

    // Fetch
    let (status, body) = post_json(
        &app,
        &token,
        "/api/v1/github-sync/repos/fetch",
        serde_json::json!({ "connection_id": conn_id, "owner": "myorg" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["added"], 1);

    // Manual fetch stamps last_fetch_at so the scheduler skips this connection
    // until its interval elapses.
    let (status, body) = get_json(&app, &token, "/api/v1/github-sync/connections").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let conn = body["data"]
        .as_array()
        .expect("arr")
        .iter()
        .find(|r| r["id"] == conn_id)
        .expect("conn");
    assert!(
        conn["last_fetch_at"].is_string()
            && !conn["last_fetch_at"].as_str().unwrap_or("").is_empty(),
        "manual fetch must stamp last_fetch_at, got: {conn}"
    );

    let (status, body) = get_json(&app, &token, "/api/v1/github-sync/repos").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"][0]["owner"], "myorg");
    assert_eq!(body["data"][0]["repo"], "alpha-repo");
    assert_eq!(body["data"][0]["connection_name"], "gh");

    // Test connection
    let (status, body) = post_json(
        &app,
        &token,
        "/api/v1/github-sync/test",
        serde_json::json!({ "connection_id": conn_id, "owner": "myorg" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["ok"], true);
}

/// Test with only a connection (no owner/repo) validates the token via
/// `GET /user` and reports the authenticated login.
#[tokio::test]
async fn test_connection_token_only() {
    let app = test_app().await;
    let token = admin_jwt();
    let mock = MockGitHub::start().await;

    let (status, body) = post_json(
        &app,
        &token,
        "/api/v1/github-sync/connections",
        serde_json::json!({ "name": "gh", "api_base_url": mock.base_url(), "token": "pat" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let conn_id = body["data"]["id"].as_str().expect("conn id").to_string();

    let (status, body) = post_json(
        &app,
        &token,
        "/api/v1/github-sync/test",
        serde_json::json!({ "connection_id": conn_id }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["ok"], true);
    assert_eq!(body["data"]["login"], "mockuser");

    let reqs = mock.snapshot();
    assert!(
        reqs.iter().any(|r| r.path == "/user"),
        "token-only test must call GET /user, got: {reqs:?}"
    );
}

/// api_base_url is validated at create time: non-https schemes and non-local
/// http hosts are rejected so the stored PAT can never be aimed at an
/// arbitrary host by construction.
#[tokio::test]
async fn connection_rejects_unsafe_api_base_url() {
    let app = test_app().await;
    let token = admin_jwt();

    for (label, base) in [
        ("http external", "http://evil.example.com"),
        ("ftp scheme", "ftp://api.github.com"),
        ("malformed", "not a url"),
    ] {
        let (status, body) = post_json(
            &app,
            &token,
            "/api/v1/github-sync/connections",
            serde_json::json!({
                "name": format!("conn-{label}"),
                "api_base_url": base,
                "token": "pat",
            }),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{label} base must be rejected: {body}"
        );
    }

    // https base is accepted (no outbound request happens at create time).
    let (status, body) = post_json(
        &app,
        &token,
        "/api/v1/github-sync/connections",
        serde_json::json!({
            "name": "conn-https",
            "api_base_url": "https://api.github.com",
            "token": "pat",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

/// A `Link: rel="next"` pointing at a foreign host must not be followed: the
/// client stops paginating instead of forwarding the PAT to that host.
#[tokio::test]
async fn fetch_repos_ignores_cross_host_next_link() {
    let app = test_app().await;
    let token = admin_jwt();
    let mock = MockGitHub::start().await;

    let (status, body) = post_json(
        &app,
        &token,
        "/api/v1/github-sync/connections",
        serde_json::json!({ "name": "gh", "api_base_url": mock.base_url(), "token": "pat" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let conn_id = body["data"]["id"].as_str().expect("conn id").to_string();

    let (status, body) = post_json(
        &app,
        &token,
        "/api/v1/github-sync/repos/fetch",
        serde_json::json!({ "connection_id": conn_id, "owner": "badnext" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["data"]["added"], 2,
        "cross-host next link must not be followed"
    );
}

/// Paginated org repo listing: the mock returns a `Link: rel="next"` on page 1
/// and the tail on page 2 — the fetch must drain both pages (GitHub defaults
/// to 30 repos/page, so a single request would silently miss the rest).
#[tokio::test]
async fn fetch_repos_follows_pagination() {
    let app = test_app().await;
    let token = admin_jwt();
    let mock = MockGitHub::start().await;

    let (status, body) = post_json(
        &app,
        &token,
        "/api/v1/github-sync/connections",
        serde_json::json!({ "name": "gh", "api_base_url": mock.base_url(), "token": "pat" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let conn_id = body["data"]["id"].as_str().expect("conn id").to_string();

    let (status, body) = post_json(
        &app,
        &token,
        "/api/v1/github-sync/repos/fetch",
        serde_json::json!({ "connection_id": conn_id, "owner": "pagedorg" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["data"]["added"], 3,
        "page 1 + page 2 must both be fetched"
    );

    let (status, body) = get_json(&app, &token, "/api/v1/github-sync/repos").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let repos = body["data"]
        .as_array()
        .expect("arr")
        .iter()
        .map(|r| r["repo"].as_str().expect("repo").to_string())
        .collect::<Vec<_>>();
    assert!(repos.contains(&"alpha".to_string()));
    assert!(repos.contains(&"beta".to_string()));
    assert!(repos.contains(&"gamma".to_string()));
}

/// Unbind removes the row; duplicate bind is rejected.
#[tokio::test]
async fn secret_bind_requires_platform_admin() {
    let app = test_app().await;
    let admin = admin_jwt();
    let regular = regular_jwt();
    let mock = MockGitHub::start().await;

    // Admin sets up: connection → repo → secret → bind.
    let (_, body) = post_json(
        &app,
        &admin,
        "/api/v1/github-sync/connections",
        serde_json::json!({ "name": "gh", "api_base_url": mock.base_url(), "token": "pat" }),
    )
    .await;
    let conn_id = body["data"]["id"].as_str().expect("conn id").to_string();

    let (_, body) = post_json(
        &app,
        &admin,
        "/api/v1/github-sync/repos",
        serde_json::json!({ "connection_id": conn_id, "owner": "myorg", "repo": "alpha-repo" }),
    )
    .await;
    let repo_id = body["data"]["id"].as_str().expect("repo id").to_string();

    let (_, body) = post_json(
        &app,
        &admin,
        "/api/v1/secrets",
        serde_json::json!({ "name": "GUARDED_SECRET", "value": "v", "team_id": "team-1" }),
    )
    .await;
    let secret_id = body["data"]["id"].as_str().expect("secret id").to_string();

    let (_, body) = post_json(
        &app,
        &admin,
        &format!("/api/v1/secrets/{secret_id}/github-binds"),
        serde_json::json!({ "repo_id": repo_id }),
    )
    .await;
    let bind_id = body["data"]["id"].as_str().expect("bind id").to_string();

    // A team maintainer (non-admin) must not be able to bind, push, or unbind —
    // the PAT-backed push makes these platform-admin operations.
    let (status, body) = post_json(
        &app,
        &regular,
        &format!("/api/v1/secrets/{secret_id}/github-binds"),
        serde_json::json!({ "repo_id": repo_id }),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "add bind: {body}");

    let (status, body) = post_json(
        &app,
        &regular,
        &format!("/api/v1/secrets/{secret_id}/github-binds/push"),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "push: {body}");

    let (status, body) = delete_json(
        &app,
        &regular,
        &format!("/api/v1/secrets/{secret_id}/github-binds/{bind_id}"),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "unbind: {body}");
}

/// Unbind removes the row; duplicate bind is rejected.
#[tokio::test]
async fn secret_bind_unbind_and_conflict() {
    let app = test_app().await;
    let token = admin_jwt();
    let mock = MockGitHub::start().await;

    let (_, body) = post_json(
        &app,
        &token,
        "/api/v1/github-sync/connections",
        serde_json::json!({ "name": "gh", "api_base_url": mock.base_url(), "token": "pat" }),
    )
    .await;
    let conn_id = body["data"]["id"].as_str().expect("conn id").to_string();

    let (_, body) = post_json(
        &app,
        &token,
        "/api/v1/github-sync/repos",
        serde_json::json!({ "connection_id": conn_id, "owner": "myorg", "repo": "alpha-repo" }),
    )
    .await;
    let repo_id = body["data"]["id"].as_str().expect("repo id").to_string();

    let (status, body) = post_json(
        &app,
        &token,
        "/api/v1/secrets",
        serde_json::json!({ "name": "ANOTHER_SECRET", "value": "v", "team_id": "team-1" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let secret_id = body["data"]["id"].as_str().expect("secret id").to_string();

    let (status, body) = post_json(
        &app,
        &token,
        &format!("/api/v1/secrets/{secret_id}/github-binds"),
        serde_json::json!({ "repo_id": repo_id }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let bind_id = body["data"]["id"].as_str().expect("bind id").to_string();
    assert_eq!(body["data"]["last_op"], "bind");

    // Duplicate → conflict
    let (status, _) = post_json(
        &app,
        &token,
        &format!("/api/v1/secrets/{secret_id}/github-binds"),
        serde_json::json!({ "repo_id": repo_id }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);

    // Unbind
    let (status, body) = delete_json(
        &app,
        &token,
        &format!("/api/v1/secrets/{secret_id}/github-binds/{bind_id}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // Unbind must also DELETE the value on GitHub Actions.
    let reqs = mock.snapshot();
    let del = reqs
        .iter()
        .find(|r| r.method == "DELETE" && r.path.contains("/actions/secrets/"))
        .expect("secret DELETE seen");
    assert!(del.path.ends_with("/actions/secrets/ANOTHER_SECRET"));

    let (status, body) = get_json(
        &app,
        &token,
        &format!("/api/v1/secrets/{secret_id}/github-binds"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"].as_array().expect("arr").len(), 0);
}

/// Unbinding a never-pushed secret: GitHub returns 404 for the DELETE, which
/// must be treated as success (idempotent) — the local bind still goes away.
#[tokio::test]
async fn unbind_404_remote_cleanup_is_idempotent() {
    let app = test_app().await;
    let token = admin_jwt();
    let mock = MockGitHub::start().await;

    let (_, body) = post_json(
        &app,
        &token,
        "/api/v1/github-sync/connections",
        serde_json::json!({ "name": "gh", "api_base_url": mock.base_url(), "token": "pat" }),
    )
    .await;
    let conn_id = body["data"]["id"].as_str().expect("conn id").to_string();

    let (_, body) = post_json(
        &app,
        &token,
        "/api/v1/github-sync/repos",
        serde_json::json!({ "connection_id": conn_id, "owner": "myorg", "repo": "alpha-repo" }),
    )
    .await;
    let repo_id = body["data"]["id"].as_str().expect("repo id").to_string();

    let (status, body) = post_json(
        &app,
        &token,
        "/api/v1/secrets",
        serde_json::json!({ "name": "MISSING_SECRET", "value": "v", "team_id": "team-1" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let secret_id = body["data"]["id"].as_str().expect("secret id").to_string();

    let (status, body) = post_json(
        &app,
        &token,
        &format!("/api/v1/secrets/{secret_id}/github-binds"),
        serde_json::json!({ "repo_id": repo_id }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let bind_id = body["data"]["id"].as_str().expect("bind id").to_string();

    // Unbind: remote DELETE returns 404 but the API must still succeed.
    let (status, body) = delete_json(
        &app,
        &token,
        &format!("/api/v1/secrets/{secret_id}/github-binds/{bind_id}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // The DELETE was actually attempted against the mock.
    let reqs = mock.snapshot();
    assert!(
        reqs.iter()
            .any(|r| r.method == "DELETE" && r.path.ends_with("/actions/secrets/MISSING_SECRET")),
        "DELETE must have been attempted even when the remote returns 404, got: {reqs:?}"
    );

    // Local bind is gone.
    let (status, body) = get_json(
        &app,
        &token,
        &format!("/api/v1/secrets/{secret_id}/github-binds"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"].as_array().expect("arr").len(), 0);
}

/// Unbind with a hard remote failure (500): the local bind must be kept and
/// marked `last_op='unbind', last_status='failed'` with the error recorded, so
/// the UI can surface "unbind failed" instead of silently dropping the row.
#[tokio::test]
async fn unbind_failure_keeps_bind_and_records_state() {
    let app = test_app().await;
    let token = admin_jwt();
    let mock = MockGitHub::start().await;

    let (_, body) = post_json(
        &app,
        &token,
        "/api/v1/github-sync/connections",
        serde_json::json!({ "name": "gh", "api_base_url": mock.base_url(), "token": "pat" }),
    )
    .await;
    let conn_id = body["data"]["id"].as_str().expect("conn id").to_string();

    let (_, body) = post_json(
        &app,
        &token,
        "/api/v1/github-sync/repos",
        serde_json::json!({ "connection_id": conn_id, "owner": "myorg", "repo": "alpha-repo" }),
    )
    .await;
    let repo_id = body["data"]["id"].as_str().expect("repo id").to_string();

    let (status, body) = post_json(
        &app,
        &token,
        "/api/v1/secrets",
        serde_json::json!({ "name": "FAIL_DELETE_SECRET", "value": "v", "team_id": "team-1" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let secret_id = body["data"]["id"].as_str().expect("secret id").to_string();

    let (status, body) = post_json(
        &app,
        &token,
        &format!("/api/v1/secrets/{secret_id}/github-binds"),
        serde_json::json!({ "repo_id": repo_id }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let bind_id = body["data"]["id"].as_str().expect("bind id").to_string();

    // Unbind: remote DELETE returns 500 → API must reject AND keep the row.
    let (status, body) = delete_json(
        &app,
        &token,
        &format!("/api/v1/secrets/{secret_id}/github-binds/{bind_id}"),
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");

    // Bind row survives and carries the failed-unbind state.
    let (status, body) = get_json(
        &app,
        &token,
        &format!("/api/v1/secrets/{secret_id}/github-binds"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let binds = body["data"].as_array().expect("arr");
    assert_eq!(binds.len(), 1);
    assert_eq!(binds[0]["id"], bind_id);
    assert_eq!(binds[0]["last_op"], "unbind");
    assert_eq!(binds[0]["last_status"], "failed");
    let err = binds[0]["last_error"].as_str().expect("last_error");
    assert!(
        err.contains("unbind failed"),
        "unexpected last_error: {err}"
    );
}

#[tokio::test]
async fn connection_fetch_config_roundtrip() {
    let app = test_app().await;
    let token = admin_jwt();

    // Create with owners (messy separators) + interval.
    let (status, body) = post_json(
        &app,
        &token,
        "/api/v1/github-sync/connections",
        serde_json::json!({
            "name": "gh",
            "token": "pat",
            "fetch_owners": "myorg, pagedorg\notherorg",
            "fetch_interval_minutes": 30,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let conn_id = body["data"]["id"].as_str().expect("conn id").to_string();
    assert_eq!(body["data"]["fetch_owners"], "myorg,pagedorg,otherorg");
    assert_eq!(body["data"]["fetch_interval_minutes"], 30);
    assert!(body["data"]["last_fetch_at"].is_null());

    // Non-positive interval → validation error.
    let (status, body) = post_json(
        &app,
        &token,
        "/api/v1/github-sync/connections",
        serde_json::json!({ "name": "gh2", "token": "pat", "fetch_interval_minutes": 0 }),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");

    // Update changes both fields.
    let (status, body) = put_json(
        &app,
        &token,
        &format!("/api/v1/github-sync/connections/{conn_id}"),
        serde_json::json!({ "fetch_owners": "onlyorg", "fetch_interval_minutes": 5 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["fetch_owners"], "onlyorg");
    assert_eq!(body["data"]["fetch_interval_minutes"], 5);

    // Explicit null disables auto-fetch.
    let (status, body) = put_json(
        &app,
        &token,
        &format!("/api/v1/github-sync/connections/{conn_id}"),
        serde_json::json!({ "fetch_interval_minutes": null }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["data"]["fetch_interval_minutes"].is_null());
    assert_eq!(body["data"]["fetch_owners"], "onlyorg");

    // List reflects the persisted config.
    let (status, body) = get_json(&app, &token, "/api/v1/github-sync/connections").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let row = body["data"]
        .as_array()
        .expect("arr")
        .iter()
        .find(|r| r["id"] == conn_id)
        .expect("row");
    assert!(row["fetch_interval_minutes"].is_null());
    assert_eq!(row["fetch_owners"], "onlyorg");
}

/// The repos list must surface every secret/variable bound to each repo
/// (metadata only — never values) so the GitHub Sync table can show them.
#[tokio::test]
async fn list_repos_includes_bound_resources() {
    let app = test_app().await;
    let token = admin_jwt();
    let mock = MockGitHub::start().await;

    let (_, body) = post_json(
        &app,
        &token,
        "/api/v1/github-sync/connections",
        serde_json::json!({ "name": "gh", "api_base_url": mock.base_url(), "token": "pat" }),
    )
    .await;
    let conn_id = body["data"]["id"].as_str().expect("conn id").to_string();

    let (_, body) = post_json(
        &app,
        &token,
        "/api/v1/github-sync/repos",
        serde_json::json!({ "connection_id": conn_id, "owner": "myorg", "repo": "alpha-repo" }),
    )
    .await;
    let repo_id = body["data"]["id"].as_str().expect("repo id").to_string();

    // Bind one secret + one variable to the same repo.
    let (status, body) = post_json(
        &app,
        &token,
        "/api/v1/secrets",
        serde_json::json!({ "name": "BOUND_SECRET", "value": "SENTINEL-VALUE-123", "team_id": "team-1" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let secret_id = body["data"]["id"].as_str().expect("secret id").to_string();
    let (status, _) = post_json(
        &app,
        &token,
        &format!("/api/v1/secrets/{secret_id}/github-binds"),
        serde_json::json!({ "repo_id": repo_id }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = post_json(
        &app,
        &token,
        "/api/v1/variables",
        serde_json::json!({ "name": "BOUND_VAR", "value": "vv" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let var_id = body["data"]["id"].as_str().expect("var id").to_string();
    let (status, _) = post_json(
        &app,
        &token,
        &format!("/api/v1/variables/{var_id}/github-binds"),
        serde_json::json!({ "repo_id": repo_id }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Repo row lists both binds, never the values.
    let (status, body) = get_json(&app, &token, "/api/v1/github-sync/repos").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let row = body["data"]
        .as_array()
        .expect("arr")
        .iter()
        .find(|r| r["id"] == repo_id)
        .expect("repo row");
    let binds = row["binds"].as_array().expect("binds");
    assert_eq!(binds.len(), 2);
    let secret = binds
        .iter()
        .find(|b| b["kind"] == "secret")
        .expect("secret bind");
    assert_eq!(secret["name"], "BOUND_SECRET");
    assert_eq!(secret["last_status"], "pending");
    assert_eq!(secret["last_op"], "bind");
    let var = binds
        .iter()
        .find(|b| b["kind"] == "variable")
        .expect("variable bind");
    assert_eq!(var["name"], "BOUND_VAR");
    assert_eq!(var["last_status"], "pending");
    assert_eq!(var["last_op"], "bind");
    assert!(
        !row["binds"].to_string().contains("SENTINEL-VALUE-123"),
        "values must never leak: {row}"
    );
}
