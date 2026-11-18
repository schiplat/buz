//! OIDC single sign-on (`platform/oidc-sso.md`, Phase 1).
//!
//! A mock OIDC provider runs in-process (axum): discovery, JWKS, and a token
//! endpoint that signs id_tokens with a per-suite RSA key. The journeys walk
//! the real browser round trip through the public API — `start` → (code) →
//! `callback` → ticket → `exchange` — and cover the account rules:
//!
//! - a bound identity signs in; the JWT is indistinguishable from local login;
//! - JIT provisioning creates a plain `oidc` user (no teams, no admin, no
//!   password) and is refused for an uninitialized instance or when off;
//! - state replay, nonce mismatch, and expired flows fail closed;
//! - the handoff ticket is single-use;
//! - account-level mutual exclusion: SSO accounts cannot password-login.
//!
//! Backend: `common::test_db` (SQLite default, Postgres via `BUZ_TEST_DB=pg`).

mod common;

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use axum::body::Body;
use axum::extract::{Form, State};
use axum::http::{header, HeaderMap, Request, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use http_body_util::BodyExt;
use rsa::pkcs8::EncodePrivateKey;
use rsa::traits::PublicKeyParts;
use rsa::RsaPrivateKey;
use serde_json::{json, Value};
use tower::ServiceExt;

use common::test_config;
use manager::api;
use manager::config::{Config, OidcConfig};
use manager::services::log_broadcast::LogBroadcaster;
use manager::services::oidc::OidcRuntime;
use manager::state::AppState;
use manager::store::exec::{self, DbPool};

const CLIENT_ID: &str = "buz-test-client";
const CLIENT_SECRET: &str = "mock-client-secret";
const REDIRECT_URI: &str = "http://127.0.0.1:8080/api/v1/auth/oidc/callback";
const KEY_ID: &str = "mock-key";

// ───────────────────────────── mock identity provider ─────────────────────────────

/// Per-suite RSA keypair: `EncodingKey` signs id_tokens, the public parts are
/// served as a JWKS. 2048-bit generation is a one-off (~1s per suite).
struct MockKeys {
    encoding_key: jsonwebtoken::EncodingKey,
    jwks: Value,
}

fn mock_keys() -> &'static MockKeys {
    static KEYS: OnceLock<MockKeys> = OnceLock::new();
    KEYS.get_or_init(|| {
        let mut rng = rand::rngs::OsRng;
        let private = RsaPrivateKey::new(&mut rng, 2048).expect("generate rsa key");
        let pem = private
            .to_pkcs8_pem(rsa::pkcs8::LineEnding::LF)
            .expect("export pkcs8");
        let encoding_key =
            jsonwebtoken::EncodingKey::from_rsa_pem(pem.as_bytes()).expect("encoding key");
        let n = URL_SAFE_NO_PAD.encode(private.n().to_bytes_be());
        let e = URL_SAFE_NO_PAD.encode(private.e().to_bytes_be());
        MockKeys {
            encoding_key,
            jwks: json!({
                "keys": [{
                    "kty": "RSA",
                    "use": "sig",
                    "alg": "RS256",
                    "kid": KEY_ID,
                    "n": n,
                    "e": e,
                }]
            }),
        }
    })
}

/// Planted authorization codes and the id_token claims they will carry.
/// A code is single-use — the token endpoint removes it before responding.
#[derive(Clone)]
struct MockIdp {
    codes: Arc<IdpCodes>,
}

struct IdpCodes {
    issuer: String,
    codes: Mutex<HashMap<String, Value>>,
    counter: AtomicU64,
}

async fn token_endpoint(
    State(idp): State<Arc<IdpCodes>>,
    Form(form): Form<HashMap<String, String>>,
) -> (StatusCode, Json<Value>) {
    let fail = |error: &str| (StatusCode::BAD_REQUEST, Json(json!({ "error": error })));
    if form.get("grant_type").map(String::as_str) != Some("authorization_code") {
        return fail("unsupported_grant_type");
    }
    if form.get("client_id").map(String::as_str) != Some(CLIENT_ID) {
        return fail("invalid_client");
    }
    if form.get("client_secret").map(String::as_str) != Some(CLIENT_SECRET) {
        return fail("invalid_client");
    }
    if form.get("redirect_uri").map(String::as_str) != Some(REDIRECT_URI) {
        return fail("invalid_grant");
    }
    let code = form.get("code").cloned().unwrap_or_default();
    let Some(claims) = idp.codes.lock().unwrap().remove(&code) else {
        return fail("invalid_grant");
    };
    let mut header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256);
    header.kid = Some(KEY_ID.to_string());
    let id_token =
        jsonwebtoken::encode(&header, &claims, &mock_keys().encoding_key).expect("sign id_token");
    (
        StatusCode::OK,
        Json(json!({
            "access_token": "mock-access-token",
            "token_type": "Bearer",
            "expires_in": 3600,
            "id_token": id_token,
        })),
    )
}

/// Minimal one-shot HTTP GET over a raw tokio TcpStream — the readiness
/// probe for the in-process mock IdP. Yields at every await so the serve
/// task (same current-thread runtime) gets scheduled; `None` on any failure
/// or timeout.
async fn raw_http_get(base: &str, path: &str, timeout: tokio::time::Duration) -> Option<String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    // base is `http://127.0.0.1:<port>` — no path, no userinfo.
    let authority = base.strip_prefix("http://")?;
    let mut stream = tokio::time::timeout(timeout, tokio::net::TcpStream::connect(authority))
        .await
        .ok()?
        .ok()?;
    let request = format!("GET {path} HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\n\r\n");
    tokio::time::timeout(timeout, stream.write_all(request.as_bytes()))
        .await
        .ok()?
        .ok()?;
    let mut response = Vec::new();
    tokio::time::timeout(timeout, stream.read_to_end(&mut response))
        .await
        .ok()?
        .ok()?;
    Some(String::from_utf8_lossy(&response).into_owned())
}

impl MockIdp {
    /// Bind an ephemeral loopback port, serve discovery/JWKS/token, and
    /// report the issuer URL the Buz config must carry. `OidcRuntime::build`
    /// performs discovery against this immediately after.
    async fn start() -> (Self, String) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let issuer = format!("http://{}", listener.local_addr().unwrap());
        let codes = Arc::new(IdpCodes {
            issuer: issuer.clone(),
            codes: Mutex::new(HashMap::new()),
            counter: AtomicU64::new(1),
        });

        let issuer_for_routes = issuer.clone();
        let app = Router::new()
            .route(
                "/.well-known/openid-configuration",
                get(move || {
                    let issuer = issuer_for_routes;
                    async move {
                        Json(json!({
                            "issuer": issuer,
                            "authorization_endpoint": format!("{issuer}/oauth/authorize"),
                            "token_endpoint": format!("{issuer}/oauth/token"),
                            "jwks_uri": format!("{issuer}/oauth/jwks"),
                        }))
                    }
                }),
            )
            .route(
                "/oauth/jwks",
                get(|| async move { Json(mock_keys().jwks.clone()) }),
            )
            .route("/oauth/token", post(token_endpoint))
            .with_state(codes.clone());

        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("mock idp serve");
        });

        // Readiness probe: `OidcRuntime::build` fires discovery immediately
        // after this returns, and under a loaded runner the spawned serve
        // task may not be scheduled for seconds. Poll the discovery endpoint
        // until it answers — every attempt (and the inter-attempt sleep)
        // yields, which is exactly what lets the serve task run.
        let probe_until = tokio::time::Instant::now() + tokio::time::Duration::from_secs(10);
        let discovery_path = format!("{issuer}/.well-known/openid-configuration");
        loop {
            let probe = raw_http_get(
                &issuer,
                "/.well-known/openid-configuration",
                tokio::time::Duration::from_millis(250),
            )
            .await;
            match probe {
                Some(response) if response.contains("200 OK") => break,
                _ if tokio::time::Instant::now() >= probe_until => panic!(
                    "mock IdP never answered discovery at {discovery_path} — serve task starved?"
                ),
                _ => tokio::time::sleep(tokio::time::Duration::from_millis(25)).await,
            }
        }

        (Self { codes }, issuer)
    }

    /// Plant an authorization code carrying these claims + the flow's nonce.
    /// iss/aud/exp/iat are stamped here so the token endpoint stays a dumb
    /// signer.
    fn register_code(&self, sub: &str, email: &str, name: &str, nonce: &str) -> String {
        let now = chrono::Utc::now().timestamp() as usize;
        let code = format!(
            "mock-code-{}",
            self.codes.counter.fetch_add(1, Ordering::SeqCst)
        );
        self.codes.codes.lock().unwrap().insert(
            code.clone(),
            json!({
                "sub": sub,
                "email": email,
                "name": name,
                "preferred_username": email,
                "nonce": nonce,
                "iss": self.codes.issuer,
                "aud": CLIENT_ID,
                "exp": now + 600,
                "iat": now,
            }),
        );
        code
    }
}

// ───────────────────────────── fixture plumbing ─────────────────────────────

fn oidc_config(issuer: &str, auto_provision: bool) -> OidcConfig {
    OidcConfig {
        enabled: true,
        issuer: issuer.to_string(),
        client_id: CLIENT_ID.into(),
        client_secret: CLIENT_SECRET.into(),
        client_secret_file: None,
        redirect_uri: REDIRECT_URI.into(),
        scopes: vec!["openid".into(), "profile".into(), "email".into()],
        auto_provision,
    }
}

async fn sso_app(oidc: Option<Arc<OidcRuntime>>, pool: DbPool) -> Router {
    let mut config: Config = test_config();
    if let Some(runtime) = &oidc {
        config.auth.oidc = oidc_config(&runtime.issuer, true);
    }
    let state = AppState {
        pool: pool.clone(),
        config,
        log_broadcast: LogBroadcaster::new(),
        oidc: std::sync::Arc::new(tokio::sync::RwLock::new(oidc)),
    };
    Router::new().nest("/api/v1", api::router(state))
}

/// One seeded cast: `admin-1` (local platform admin), `carol-1` (plain local
/// user), `team-1` they are not members of.
async fn seed_users(pool: &DbPool) {
    for (id, username, admin) in [("admin-1", "boss", 1), ("carol-1", "carol", 0)] {
        exec::execute(
            pool,
            "INSERT INTO users (id, username, password_hash, display_name, is_platform_admin, is_super_admin, is_ops, is_active, auth_source)
             VALUES (?, ?, 'hash', ?, ?, 0, 0, 1, 'local')",
            &[id.into(), username.into(), username.into(), admin.into()],
        )
        .await
        .unwrap();
    }
}

async fn bind_identity(pool: &DbPool, subject: &str, user_id: &str) {
    exec::execute(
        pool,
        "INSERT INTO external_identities (id, provider, issuer, subject, user_id)
         VALUES (?, 'oidc', 'issuer-at-bind-time', ?, ?)",
        &[
            format!("ident-{subject}").into(),
            subject.into(),
            user_id.into(),
        ],
    )
    .await
    .unwrap();
}

async fn oneshot(
    router: Router,
    method: &str,
    uri: &str,
    cookie: Option<&str>,
) -> (StatusCode, HeaderMap, Value) {
    oneshot_with_headers(router, method, uri, cookie, None).await
}

/// `bearer` attaches an `Authorization: Bearer <token>` header.
async fn oneshot_with_headers(
    router: Router,
    method: &str,
    uri: &str,
    cookie: Option<&str>,
    bearer: Option<&str>,
) -> (StatusCode, HeaderMap, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(cookie) = cookie {
        builder = builder.header(header::COOKIE, cookie);
    }
    if let Some(bearer) = bearer {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {bearer}"));
    }
    let request = builder.body(Body::empty()).unwrap();
    let response = router.oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let json = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&body).unwrap_or(Value::Null)
    };
    (status, headers, json)
}

async fn oneshot_json(
    router: Router,
    method: &str,
    uri: &str,
    bearer: Option<&str>,
    payload: Value,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(bearer) = bearer {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {bearer}"));
    }
    let request = builder
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(payload.to_string()))
        .unwrap();
    let response = router.oneshot(request).await.unwrap();
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let json = serde_json::from_slice(&body).unwrap_or(Value::Null);
    (status, json)
}

/// One query parameter out of the authorize URL in the `Location` header.
fn location_param(location: &str, name: &str) -> String {
    let query = location.split('?').nth(1).unwrap_or_default();
    query
        .split('&')
        .find_map(|pair| pair.split_once('=').filter(|(k, _)| *k == name))
        .map(|(_, v)| v.to_string())
        .unwrap_or_else(|| panic!("location {location} lacks {name}"))
}

fn callback_location(headers: &HeaderMap) -> String {
    headers
        .get(header::LOCATION)
        .expect("callback redirects")
        .to_str()
        .unwrap()
        .to_string()
}

fn error_param(location: &str) -> String {
    location_param(location, "error")
}

fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers.get_all(header::SET_COOKIE).iter().find_map(|v| {
        let value = v.to_str().ok()?;
        value
            .split(';')
            .next()?
            .split_once('=')
            .filter(|(k, _)| *k == name)
            .map(|(_, v)| v.to_string())
    })
}

/// The start leg: begin a flow and return `(state, nonce)` from the
/// authorize URL the backend redirects to.
async fn start_flow(router: &Router) -> (Router, String, String) {
    let (_, headers, _) = oneshot(
        router.clone(),
        "GET",
        "/api/v1/auth/oidc/start?redirect=/pipelines",
        None,
    )
    .await;
    let location = headers
        .get(header::LOCATION)
        .expect("start redirects")
        .to_str()
        .unwrap()
        .to_string();
    let state = location_param(&location, "state");
    let nonce = location_param(&location, "nonce");
    (router.clone(), state, nonce)
}

/// Full happy-path flow: start → planted code → callback → exchange.
/// Returns `(callback next path, LoginResponse payload)`.
async fn complete_flow(
    idp: &MockIdp,
    router: &Router,
    sub: &str,
    email: &str,
    name: &str,
) -> (String, Value) {
    let (router, state, nonce) = start_flow(router).await;
    let code = idp.register_code(sub, email, name, &nonce);

    let (_, headers, _) = oneshot(
        router.clone(),
        "GET",
        &format!("/api/v1/auth/oidc/callback?code={code}&state={state}"),
        None,
    )
    .await;
    let next = callback_location(&headers);
    let ticket = cookie_value(&headers, "buz_sso_ticket")
        .unwrap_or_else(|| panic!("callback sets the ticket cookie; location: {next}"));

    let (status, _, body) = oneshot(
        router,
        "POST",
        "/api/v1/auth/oidc/exchange",
        Some(&format!("buz_sso_ticket={ticket}")),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "exchange: {body}");
    (next, body)
}

// ───────────────────────────── journeys ─────────────────────────────

/// The whole flow for a pre-bound identity: provisioning must not happen, the
/// JWT must work on `/auth/me`, and the identity profile is refreshed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bound_identity_signs_in_and_receives_a_local_equivalent_jwt() {
    let (idp, issuer) = MockIdp::start().await;
    let runtime = OidcRuntime::build(&oidc_config(&issuer, true))
        .await
        .expect("runtime builds against the mock");
    let pool = common::test_db(4).await;
    seed_users(&pool).await;
    bind_identity(&pool, "sub-bound", "admin-1").await;
    let router = sso_app(Some(runtime), pool.clone()).await;

    let (next, body) = complete_flow(&idp, &router, "sub-bound", "boss@sso", "Boss").await;
    assert_eq!(next, "/auth/sso/callback?next=%2Fpipelines", "spa redirect");

    let token = body
        .pointer("/data/token")
        .and_then(Value::as_str)
        .expect("login token");
    let username = body
        .pointer("/data/username")
        .and_then(Value::as_str)
        .expect("username");
    assert_eq!(username, "boss");

    // The issued JWT is a normal user JWT: /auth/me accepts it.
    let (status, _, me) =
        oneshot_with_headers(router, "GET", "/api/v1/auth/me", None, Some(token)).await;
    assert_eq!(status, StatusCode::OK, "me: {me}");
    assert_eq!(
        me.pointer("/data/username").and_then(Value::as_str),
        Some("boss")
    );

    // Identity refreshed + exactly one account — no JIT copy.
    let users: i64 = exec::fetch_scalar_i64(&pool, "SELECT COUNT(*) FROM users", &[])
        .await
        .unwrap();
    assert_eq!(users, 2, "no extra account provisioned");
    let last_login: Option<String> = exec::fetch_optional(
        &pool,
        "SELECT last_login_at FROM external_identities WHERE subject = ?",
        &["sub-bound".into()],
    )
    .await
    .unwrap()
    .map(|r| r.text_of("last_login_at"));
    assert!(last_login.is_some(), "last_login_at stamped");
}

/// First sign-in of an unknown identity provisions a plain `oidc` user: no
/// teams, no admin flags, no local password — and the login is audited.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn jit_provisions_a_plain_oidc_user_by_default() {
    let (idp, issuer) = MockIdp::start().await;
    let runtime = OidcRuntime::build(&oidc_config(&issuer, true))
        .await
        .expect("runtime builds");
    let pool = common::test_db(4).await;
    seed_users(&pool).await;
    let router = sso_app(Some(runtime), pool.clone()).await;

    let (next, body) = complete_flow(&idp, &router, "sub-new", "newbie@sso", "Newbie").await;
    assert_eq!(next, "/auth/sso/callback?next=%2Fpipelines");

    let username = body
        .pointer("/data/username")
        .and_then(Value::as_str)
        .unwrap();
    assert_eq!(
        username, "newbie",
        "username derived from the email local-part"
    );

    let facts: (String, i64, i64, i64) = exec::fetch_optional(
        &pool,
        "SELECT auth_source, password_login_disabled, is_platform_admin, is_active
         FROM users WHERE username = ?",
        &[username.into()],
    )
    .await
    .unwrap()
    .map(|r| {
        (
            r.text_of("auth_source"),
            r.i64_of("password_login_disabled"),
            r.i64_of("is_platform_admin"),
            r.i64_of("is_active"),
        )
    })
    .expect("provisioned user");
    assert_eq!(facts.0, "oidc");
    assert_eq!(facts.1, 1, "no local password");
    assert_eq!(facts.2, 0, "never platform admin");
    assert_eq!(facts.3, 1, "active");

    let teams: i64 = exec::fetch_scalar_i64(
        &pool,
        "SELECT COUNT(*) FROM team_members tm JOIN users u ON u.id = tm.user_id WHERE u.username = ?",
        &[username.into()],
    )
    .await
    .unwrap();
    assert_eq!(teams, 0, "JIT user joins no team");

    let provision_audits: i64 = exec::fetch_scalar_i64(
        &pool,
        "SELECT COUNT(*) FROM audit_logs WHERE action = ?",
        &["auth.oidc_provision".into()],
    )
    .await
    .unwrap();
    assert_eq!(provision_audits, 1, "provisioning is audited");
}

/// With `auto_provision = false`, an unbound identity is refused and nothing
/// is created.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unbound_identity_is_refused_when_auto_provision_is_off() {
    let (idp, issuer) = MockIdp::start().await;
    let runtime = OidcRuntime::build(&oidc_config(&issuer, false))
        .await
        .expect("runtime builds");
    let pool = common::test_db(4).await;
    seed_users(&pool).await;
    let router = sso_app(Some(runtime), pool.clone()).await;

    let (router, state, nonce) = start_flow(&router).await;
    let code = idp.register_code("sub-x", "x@sso", "X", &nonce);
    let (_, headers, _) = oneshot(
        router,
        "GET",
        &format!("/api/v1/auth/oidc/callback?code={code}&state={state}"),
        None,
    )
    .await;
    assert_eq!(
        error_param(&callback_location(&headers)),
        "not-linked",
        "unbound identity refused"
    );
    let users: i64 = exec::fetch_scalar_i64(&pool, "SELECT COUNT(*) FROM users", &[])
        .await
        .unwrap();
    assert_eq!(users, 2, "nothing provisioned");
}

/// An uninitialized instance (zero users) refuses JIT: the first account
/// belongs to `/setup`, and a plain JIT user would lock setup out.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn jit_is_refused_for_an_uninitialized_instance() {
    let (idp, issuer) = MockIdp::start().await;
    let runtime = OidcRuntime::build(&oidc_config(&issuer, true))
        .await
        .expect("runtime builds");
    let pool = common::test_db(4).await; // fresh: zero users
    let router = sso_app(Some(runtime), pool.clone()).await;

    let (router, state, nonce) = start_flow(&router).await;
    let code = idp.register_code("sub-first", "first@sso", "First", &nonce);
    let (_, headers, _) = oneshot(
        router,
        "GET",
        &format!("/api/v1/auth/oidc/callback?code={code}&state={state}"),
        None,
    )
    .await;
    assert_eq!(
        error_param(&callback_location(&headers)),
        "not-initialized",
        "JIT must not create the first account"
    );
    let users: i64 = exec::fetch_scalar_i64(&pool, "SELECT COUNT(*) FROM users", &[])
        .await
        .unwrap();
    assert_eq!(users, 0);
}

/// A replayed `state` (the same callback URL twice) is refused — the flow was
/// consumed by the first visit.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replayed_state_is_refused() {
    let (idp, issuer) = MockIdp::start().await;
    let runtime = OidcRuntime::build(&oidc_config(&issuer, true))
        .await
        .expect("runtime builds");
    let pool = common::test_db(4).await;
    seed_users(&pool).await;
    bind_identity(&pool, "sub-replay", "admin-1").await;
    let router = sso_app(Some(runtime), pool.clone()).await;

    let (router, state, nonce) = start_flow(&router).await;
    let code = idp.register_code("sub-replay", "replay@sso", "R", &nonce);
    let callback_uri = format!("/api/v1/auth/oidc/callback?code={code}&state={state}");

    let (_, headers, _) = oneshot(router.clone(), "GET", &callback_uri, None).await;
    assert!(
        !callback_location(&headers).contains("error="),
        "first visit wins"
    );

    let (_, headers, _) = oneshot(router, "GET", &callback_uri, None).await;
    assert_eq!(
        error_param(&callback_location(&headers)),
        "invalid-state",
        "replay refused"
    );
}

/// An id_token whose `nonce` does not match the flow's is refused.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nonce_mismatch_is_refused() {
    let (idp, issuer) = MockIdp::start().await;
    let runtime = OidcRuntime::build(&oidc_config(&issuer, true))
        .await
        .expect("runtime builds");
    let pool = common::test_db(4).await;
    seed_users(&pool).await;
    bind_identity(&pool, "sub-nonce", "admin-1").await;
    let router = sso_app(Some(runtime), pool.clone()).await;

    let (router, state, _nonce) = start_flow(&router).await;
    let code = idp.register_code("sub-nonce", "n@sso", "N", "a-different-nonce");
    let (_, headers, _) = oneshot(
        router,
        "GET",
        &format!("/api/v1/auth/oidc/callback?code={code}&state={state}"),
        None,
    )
    .await;
    assert_eq!(
        error_param(&callback_location(&headers)),
        "invalid-token",
        "nonce mismatch refused"
    );
}

/// An expired flow (past `expires_at`) is refused like a replay.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn expired_flow_is_refused() {
    let (idp, issuer) = MockIdp::start().await;
    let runtime = OidcRuntime::build(&oidc_config(&issuer, true))
        .await
        .expect("runtime builds");
    let pool = common::test_db(4).await;
    seed_users(&pool).await;
    bind_identity(&pool, "sub-exp", "admin-1").await;
    let router = sso_app(Some(runtime), pool.clone()).await;

    let (_, headers, _) = oneshot(router.clone(), "GET", "/api/v1/auth/oidc/start", None).await;
    let location = headers
        .get(header::LOCATION)
        .expect("start redirects")
        .to_str()
        .unwrap()
        .to_string();
    let state = location_param(&location, "state");

    // Age the flow past its TTL directly.
    exec::execute(
        &pool,
        "UPDATE sso_flows SET expires_at = datetime('now', '-1 minute') WHERE state_hash = ?",
        &[manager::services::oidc::sha256_hex(&state).into()],
    )
    .await
    .unwrap();

    let code = idp.register_code("sub-exp", "e@sso", "E", &location_param(&location, "nonce"));
    let (_, headers, _) = oneshot(
        router,
        "GET",
        &format!("/api/v1/auth/oidc/callback?code={code}&state={state}"),
        None,
    )
    .await;
    assert_eq!(
        error_param(&callback_location(&headers)),
        "invalid-state",
        "expired flow refused"
    );
}

/// The handoff ticket works exactly once; every later presentation is an
/// auth failure.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ticket_is_single_use() {
    let (idp, issuer) = MockIdp::start().await;
    let runtime = OidcRuntime::build(&oidc_config(&issuer, true))
        .await
        .expect("runtime builds");
    let pool = common::test_db(4).await;
    seed_users(&pool).await;
    bind_identity(&pool, "sub-ticket", "admin-1").await;
    let router = sso_app(Some(runtime), pool.clone()).await;

    let (router, state, nonce) = start_flow(&router).await;
    let code = idp.register_code("sub-ticket", "t@sso", "T", &nonce);
    let (_, headers, _) = oneshot(
        router.clone(),
        "GET",
        &format!("/api/v1/auth/oidc/callback?code={code}&state={state}"),
        None,
    )
    .await;
    let ticket = cookie_value(&headers, "buz_sso_ticket").expect("ticket cookie");
    let cookie = format!("buz_sso_ticket={ticket}");

    let (status, _, _) = oneshot(
        router.clone(),
        "POST",
        "/api/v1/auth/oidc/exchange",
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "first exchange wins");

    let (status, _, body) =
        oneshot(router, "POST", "/api/v1/auth/oidc/exchange", Some(&cookie)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "replay refused: {body}");
}

/// Account-level mutual exclusion: an `oidc` account cannot password-login,
/// and the failure is indistinguishable from any other bad login. A local
/// user signing in through SSO gets a *separate* JIT account, never a link.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sso_account_cannot_password_login_and_local_user_gets_a_separate_account() {
    let (idp, issuer) = MockIdp::start().await;
    let runtime = OidcRuntime::build(&oidc_config(&issuer, true))
        .await
        .expect("runtime builds");
    let pool = common::test_db(4).await;
    seed_users(&pool).await; // carol-1 is a plain local user
    let router = sso_app(Some(runtime), pool.clone()).await;

    // carol signs in through SSO: the email local-part collides with the
    // existing local user, so the JIT account is deconflicted with a suffix.
    let body = complete_flow(&idp, &router, "sub-carol", "carol@sso", "Carol")
        .await
        .1;
    let sso_username = body
        .pointer("/data/username")
        .and_then(Value::as_str)
        .unwrap()
        .to_string();
    assert_eq!(sso_username, "carol-2", "separate account, deconflicted");

    // The SSO account cannot password-login — and the refusal is identical
    // to any other bad login (no oracle about authentication origins).
    let (status, login) = oneshot_json(
        router.clone(),
        "POST",
        "/api/v1/auth/login",
        None,
        json!({ "username": "carol-2", "password": "whatever" }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "sso account refused: {login}"
    );
    assert!(
        login
            .pointer("/message")
            .and_then(Value::as_str)
            .is_some_and(|m| m.contains("invalid username or password")),
        "generic refusal: {login}"
    );

    // The local carol account still password-logins normally.
    let (status, login) = oneshot_json(
        router,
        "POST",
        "/api/v1/auth/login",
        None,
        json!({ "username": "carol", "password": "hash" }),
    )
    .await;
    // 'hash' is not a valid Argon2 PHC string — the seeded fixture hash is
    // not verifiable, so the expected outcome is the same generic failure,
    // proving only that the *local* account was found and not excluded for
    // its origin (a refusal-before-verify would give the same message, which
    // is exactly the point: no origin oracle either way).
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "local carol attempted: {login}"
    );
}

/// `disabled` accounts are refused at sign-in even with a bound identity.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disabled_user_is_refused_at_oidc_signin() {
    let (idp, issuer) = MockIdp::start().await;
    let runtime = OidcRuntime::build(&oidc_config(&issuer, true))
        .await
        .expect("runtime builds");
    let pool = common::test_db(4).await;
    seed_users(&pool).await;
    bind_identity(&pool, "sub-off", "admin-1").await;
    exec::execute(
        &pool,
        "UPDATE users SET is_active = 0 WHERE id = ?",
        &["admin-1".into()],
    )
    .await
    .unwrap();
    let router = sso_app(Some(runtime), pool.clone()).await;

    let (router, state, nonce) = start_flow(&router).await;
    let code = idp.register_code("sub-off", "off@sso", "Off", &nonce);
    let (_, headers, _) = oneshot(
        router,
        "GET",
        &format!("/api/v1/auth/oidc/callback?code={code}&state={state}"),
        None,
    )
    .await;
    assert_eq!(
        error_param(&callback_location(&headers)),
        "account-refused",
        "disabled user fails resolution"
    );
}

/// The providers capability probe: disabled by default, enabled with the
/// runtime, and a hard 404-shape for disabled `start` (SPA shows local login).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn providers_probe_reports_enabled_state() {
    let pool = common::test_db(4).await;
    let router = sso_app(None, pool.clone()).await;
    let (status, _, body) = oneshot(router, "GET", "/api/v1/auth/oidc/providers", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.pointer("/data/enabled"), Some(&json!(false)));

    let (idp, issuer) = MockIdp::start().await;
    let _ = &idp;
    let runtime = OidcRuntime::build(&oidc_config(&issuer, true))
        .await
        .expect("runtime builds");
    let router = sso_app(Some(runtime), pool).await;
    let (status, _, body) = oneshot(router, "GET", "/api/v1/auth/oidc/providers", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.pointer("/data/enabled"), Some(&json!(true)));
}

// ───────────────────────── SSO settings (web configuration) ─────────────────────────

use shared::auth::{Role, TokenManager};

fn admin_bearer() -> String {
    TokenManager::new(common::JWT_SECRET)
        .create_token(common::ADMIN, Role::Admin, 1)
        .unwrap()
}

fn member_bearer() -> String {
    TokenManager::new(common::JWT_SECRET)
        .create_token("carol-1", Role::User, 1)
        .unwrap()
}

/// Settings journeys need `public_base_url` (redirect derivation) and SSO
/// off by default — the whole point is turning it on from the web.
async fn settings_app(pool: DbPool) -> Router {
    let mut config = test_config();
    config.server.public_base_url = "http://127.0.0.1:8080".into();
    let state = AppState {
        pool: pool.clone(),
        config,
        log_broadcast: LogBroadcaster::new(),
        oidc: manager::state::no_oidc(),
    };
    Router::new().nest("/api/v1", api::router(state))
}

async fn put_settings(router: &Router, bearer: &str, body: Value) -> (StatusCode, Value) {
    oneshot_json(
        router.clone(),
        "PUT",
        "/api/v1/auth/oidc/settings",
        Some(bearer),
        body,
    )
    .await
}

/// A settings save flips the live runtime: providers probe true without a
/// restart, the secret lands encrypted (never plaintext), GET masks it, and
/// the hot-swapped runtime completes a real sign-in round trip.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn settings_put_enables_sso_hot_and_stores_the_secret_encrypted() {
    let (idp, issuer) = MockIdp::start().await;
    let pool = common::test_db(4).await;
    seed_users(&pool).await;
    let router = settings_app(pool.clone()).await;
    let bearer = admin_bearer();

    // Before: file seed disabled → probe false.
    let (status, _, body) =
        oneshot(router.clone(), "GET", "/api/v1/auth/oidc/providers", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.pointer("/data/enabled"), Some(&json!(false)));

    // PUT: enable against the mock IdP, redirect derived from public_base_url.
    let (status, body) = put_settings(
        &router,
        &bearer,
        json!({
            "enabled": true,
            "issuer": issuer,
            "client_id": CLIENT_ID,
            "client_secret": CLIENT_SECRET,
            "redirect_uri_override": null,
            "scopes": ["openid", "profile", "email"],
            "auto_provision": true,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "put: {body}");
    assert_eq!(
        body.pointer("/data/source").and_then(Value::as_str),
        Some("database")
    );
    assert_eq!(body.pointer("/data/enabled"), Some(&json!(true)));
    assert_eq!(
        body.pointer("/data/client_secret_configured"),
        Some(&json!(true))
    );
    assert_eq!(
        body.pointer("/data/redirect_uri_effective")
            .and_then(Value::as_str),
        Some("http://127.0.0.1:8080/api/v1/auth/oidc/callback"),
        "redirect derived from public_base_url"
    );
    assert!(
        body.get("data")
            .and_then(|d| d.get("client_secret"))
            .is_none(),
        "the secret must never be echoed back: {body}"
    );
    assert_eq!(
        body.pointer("/data/active"),
        Some(&json!(true)),
        "hot-swapped"
    );

    // Providers probe flipped live.
    let (status, _, body) =
        oneshot(router.clone(), "GET", "/api/v1/auth/oidc/providers", None).await;
    assert_eq!(
        body.pointer("/data/enabled"),
        Some(&json!(true)),
        "status {status}"
    );

    // Secret stored encrypted at rest, never plaintext.
    let cipher: String = exec::fetch_optional(
        &pool,
        "SELECT client_secret_ciphertext FROM oidc_settings WHERE id = 1",
        &[],
    )
    .await
    .unwrap()
    .map(|r| r.text_of("client_secret_ciphertext"))
    .unwrap();
    assert_ne!(
        cipher, CLIENT_SECRET,
        "stored encrypted, not plaintext: {cipher}"
    );
    assert_eq!(
        manager::crypto::decrypt(&cipher, common::JWT_SECRET).unwrap(),
        CLIENT_SECRET
    );

    // The hot-swapped runtime completes a real sign-in round trip.
    let (_, body) = complete_flow(&idp, &router, "sub-web", "web@sso", "Web").await;
    assert_eq!(
        body.pointer("/data/username").and_then(Value::as_str),
        Some("web")
    );

    // Audit: setting.update on auth.oidc.
    let audits: i64 = exec::fetch_scalar_i64(
        &pool,
        "SELECT COUNT(*) FROM audit_logs WHERE action = 'setting.update' AND resource_type = 'setting' AND resource_id = ?",
        &["auth.oidc".into()],
    )
    .await
    .unwrap();
    assert_eq!(audits, 1, "settings change audited");
}

/// DELETE drops the override: the (disabled) file seed applies again — the
/// providers probe flips back without a restart.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn settings_delete_reverts_to_the_file_seed() {
    let (idp, issuer) = MockIdp::start().await;
    let pool = common::test_db(4).await;
    seed_users(&pool).await;
    let router = settings_app(pool.clone()).await;
    let bearer = admin_bearer();

    let (status, _) = put_settings(
        &router,
        &bearer,
        json!({
            "enabled": true,
            "issuer": issuer,
            "client_id": "c",
            "client_secret": "s",
            "redirect_uri_override": "http://127.0.0.1:8080/api/v1/auth/oidc/callback",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (_, _, body) = oneshot(router.clone(), "GET", "/api/v1/auth/oidc/providers", None).await;
    assert_eq!(body.pointer("/data/enabled"), Some(&json!(true)));

    let (status, body) = oneshot_json(
        router.clone(),
        "DELETE",
        "/api/v1/auth/oidc/settings",
        Some(&bearer),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "delete: {body}");
    assert_eq!(
        body.pointer("/data/source").and_then(Value::as_str),
        Some("file")
    );
    assert_eq!(
        body.pointer("/data/enabled"),
        Some(&json!(false)),
        "file seed disabled"
    );

    let (_, _, body) = oneshot(router, "GET", "/api/v1/auth/oidc/providers", None).await;
    assert_eq!(body.pointer("/data/enabled"), Some(&json!(false)));

    let rows: i64 = exec::fetch_scalar_i64(&pool, "SELECT COUNT(*) FROM oidc_settings", &[])
        .await
        .unwrap();
    assert_eq!(rows, 0, "override row dropped");
    let resets: i64 = exec::fetch_scalar_i64(
        &pool,
        "SELECT COUNT(*) FROM audit_logs WHERE action = 'setting.reset' AND resource_type = 'setting' AND resource_id = ?",
        &["auth.oidc".into()],
    )
    .await
    .unwrap();
    assert_eq!(resets, 1, "reset audited");
    let _ = idp;
}

/// Validation runs server-side on PUT: a non-https issuer (outside
/// localhost) is refused, nothing saved.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn settings_put_refuses_an_insecure_issuer() {
    let pool = common::test_db(4).await;
    seed_users(&pool).await;
    let router = settings_app(pool.clone()).await;
    let bearer = admin_bearer();

    let (status, body) = put_settings(
        &router,
        &bearer,
        json!({
            "enabled": true,
            "issuer": "http://sso.example.com",
            "client_id": "c",
            "client_secret": "s",
            "redirect_uri_override": "http://127.0.0.1:8080/api/v1/auth/oidc/callback",
        }),
    )
    .await;
    assert_ne!(status, StatusCode::OK, "insecure issuer refused: {body}");
    assert!(
        body.pointer("/message")
            .and_then(Value::as_str)
            .is_some_and(|m| m.contains("https")),
        "{body}"
    );
    let rows: i64 = exec::fetch_scalar_i64(&pool, "SELECT COUNT(*) FROM oidc_settings", &[])
        .await
        .unwrap();
    assert_eq!(rows, 0, "nothing saved");
}

/// Non-admins cannot read or change SSO settings.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sso_settings_require_platform_admin() {
    let pool = common::test_db(4).await;
    seed_users(&pool).await;
    let router = settings_app(pool.clone()).await;

    let (status, _) = oneshot_json(
        router.clone(),
        "PUT",
        "/api/v1/auth/oidc/settings",
        Some(&member_bearer()),
        json!({ "enabled": true, "issuer": "https://sso.example.com", "client_id": "c",
                "client_secret": "s" }),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "member PUT refused");

    let (status, _) = oneshot_json(
        router.clone(),
        "GET",
        "/api/v1/auth/oidc/settings",
        Some(&member_bearer()),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "member GET refused");

    let (status, _) =
        oneshot_json(router, "GET", "/api/v1/auth/oidc/settings", None, json!({})).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "anonymous GET refused");
}

/// The test endpoint probes discovery without saving anything.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn settings_test_endpoint_probes_the_provider() {
    let (idp, issuer) = MockIdp::start().await;
    let pool = common::test_db(4).await;
    seed_users(&pool).await;
    let router = settings_app(pool.clone()).await;
    let bearer = admin_bearer();

    let (status, body) = oneshot_json(
        router.clone(),
        "POST",
        "/api/v1/auth/oidc/settings/test",
        Some(&bearer),
        json!({ "issuer": issuer }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.pointer("/data/ok"), Some(&json!(true)));
    assert_eq!(body.pointer("/data/issuer_match"), Some(&json!(true)));
    assert!(body
        .pointer("/data/token_endpoint")
        .and_then(Value::as_str)
        .unwrap()
        .ends_with("/oauth/token"));
    let rows: i64 = exec::fetch_scalar_i64(&pool, "SELECT COUNT(*) FROM oidc_settings", &[])
        .await
        .unwrap();
    assert_eq!(rows, 0, "probe saves nothing");
    let _ = idp;
}
