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
use axum::extract::{Form, Query, State};
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
use manager::config::Config;
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
    /// The served app, kept for journeys that drive the IdP in-process.
    app: Router,
}

impl MockIdp {
    /// Drive the served IdP in-process (readiness probes and end-session
    /// redirects without a real browser).
    fn router(&self) -> Router {
        self.app.clone()
    }
}

struct IdpCodes {
    issuer: String,
    codes: Mutex<HashMap<String, Value>>,
    counter: AtomicU64,
    /// Declared `token_endpoint_auth_methods_supported` (empty = permissive).
    auth_methods: Vec<String>,
}

/// Decode an `Authorization: Basic` header into `(user, password)`.
fn basic_credentials(headers: &HeaderMap) -> Option<(String, String)> {
    use base64::engine::general_purpose::STANDARD;
    let raw = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let encoded = raw.strip_prefix("Basic ")?.trim();
    let decoded = STANDARD.decode(encoded).ok()?;
    let text = String::from_utf8(decoded).ok()?;
    text.split_once(':')
        .map(|(u, p)| (u.to_string(), p.to_string()))
}

async fn token_endpoint(
    State(idp): State<Arc<IdpCodes>>,
    headers: HeaderMap,
    Form(form): Form<HashMap<String, String>>,
) -> (StatusCode, Json<Value>) {
    let fail = |error: &str| (StatusCode::BAD_REQUEST, Json(json!({ "error": error })));
    if form.get("grant_type").map(String::as_str) != Some("authorization_code") {
        return fail("unsupported_grant_type");
    }
    // Client authentication per what this mock declares: basic-only suites
    // require the Basic header and reject body credentials; everything else
    // accepts the form shape (a provider declaring both accepts either).
    let basic_only = idp.auth_methods.iter().any(|m| m == "client_secret_basic")
        && !idp.auth_methods.iter().any(|m| m == "client_secret_post");
    if basic_only {
        let credentials_ok =
            basic_credentials(&headers).is_some_and(|(u, p)| u == CLIENT_ID && p == CLIENT_SECRET);
        if !credentials_ok {
            return fail("invalid_client");
        }
        if form.contains_key("client_id") || form.contains_key("client_secret") {
            return fail("invalid_client");
        }
    } else {
        if form.get("client_id").map(String::as_str) != Some(CLIENT_ID) {
            return fail("invalid_client");
        }
        if form.get("client_secret").map(String::as_str) != Some(CLIENT_SECRET) {
            return fail("invalid_client");
        }
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

/// POST end-session: acknowledged (the RP's browser would only use this in
/// the legacy auto-submit form; the journeys drive the GET variant).
async fn end_session_endpoint(State(_idp): State<Arc<IdpCodes>>) -> StatusCode {
    StatusCode::OK
}

/// GET end-session, Signet-shaped: a valid hint plus a registered landing
/// redirect the browser out; anything less answers 400 like Signet's GET.
async fn end_session_get(
    Query(q): Query<HashMap<String, String>>,
) -> impl axum::response::IntoResponse {
    match (q.get("id_token_hint"), q.get("post_logout_redirect_uri")) {
        (Some(hint), Some(uri)) if !hint.is_empty() && !uri.is_empty() => {
            (StatusCode::SEE_OTHER, [(header::LOCATION, uri.clone())])
        }
        _ => (StatusCode::BAD_REQUEST, [(header::LOCATION, String::new())]),
    }
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
    /// The default mock: `client_secret_post` (Buz's legacy behavior),
    /// end-session advertised.
    async fn start() -> (Self, String) {
        Self::start_with(vec![], true).await
    }

    /// A mock declaring `auth_methods` in discovery; `end_session = false`
    /// omits the endpoint entirely (the purely-local-logout case).
    async fn start_with(auth_methods: Vec<&str>, end_session: bool) -> (Self, String) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let issuer = format!("http://{}", listener.local_addr().unwrap());
        let codes = Arc::new(IdpCodes {
            issuer: issuer.clone(),
            codes: Mutex::new(HashMap::new()),
            counter: AtomicU64::new(1),
            auth_methods: auth_methods.iter().map(|s| s.to_string()).collect(),
        });

        let issuer_for_routes = issuer.clone();
        let advertised_methods = codes.auth_methods.clone();
        let app = Router::new()
            .route(
                "/.well-known/openid-configuration",
                get(move || {
                    let issuer = issuer_for_routes;
                    let advertised_methods = advertised_methods.clone();
                    async move {
                        let mut doc = json!({
                            "issuer": issuer,
                            "authorization_endpoint": format!("{issuer}/oauth/authorize"),
                            "token_endpoint": format!("{issuer}/oauth/token"),
                            "jwks_uri": format!("{issuer}/oauth/jwks"),
                        });
                        if end_session {
                            doc["end_session_endpoint"] =
                                json!(format!("{issuer}/oauth/end_session"));
                        }
                        if !advertised_methods.is_empty() {
                            doc["token_endpoint_auth_methods_supported"] =
                                json!(advertised_methods);
                        }
                        Json(doc)
                    }
                }),
            )
            .route(
                "/oauth/jwks",
                get(|| async move { Json(mock_keys().jwks.clone()) }),
            )
            .route("/oauth/token", post(token_endpoint))
            .route(
                "/oauth/end_session",
                get(end_session_get).post(end_session_endpoint),
            )
            .with_state(codes.clone());

        let serve_app = app.clone();
        tokio::spawn(async move {
            axum::serve(listener, serve_app)
                .await
                .expect("mock idp serve");
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

        (Self { codes, app }, issuer)
    }

    /// Plant an authorization code carrying these claims + the flow's nonce.
    /// iss/aud/exp/iat are stamped here so the token endpoint stays a dumb
    /// signer. `extra` claims are merged in (custom groups claim tests).
    fn register_code(&self, sub: &str, email: &str, name: &str, nonce: &str) -> String {
        self.register_code_with(sub, email, name, nonce, json!({}))
    }

    fn register_code_with(
        &self,
        sub: &str,
        email: &str,
        name: &str,
        nonce: &str,
        extra: Value,
    ) -> String {
        let now = chrono::Utc::now().timestamp() as usize;
        let code = format!(
            "mock-code-{}",
            self.codes.counter.fetch_add(1, Ordering::SeqCst)
        );
        let mut claims = json!({
            "sub": sub,
            "email": email,
            "name": name,
            "preferred_username": email,
            "nonce": nonce,
            "iss": self.codes.issuer,
            "aud": CLIENT_ID,
            "exp": now + 600,
            "iat": now,
        });
        if let Value::Object(extra) = extra {
            for (key, value) in extra {
                claims[key] = value;
            }
        }
        self.codes
            .codes
            .lock()
            .unwrap()
            .insert(code.clone(), claims);
        code
    }
}

// ───────────────────────────── fixture plumbing ─────────────────────────────

/// The runtime input under the DB-only configuration model: the same facts
/// `oidc_config` used to carry, expressed as a resolved database row would be.
fn services_effective(
    issuer: &str,
    auto_provision: bool,
) -> manager::services::oidc::EffectiveOidc {
    manager::services::oidc::EffectiveOidc {
        enabled: true,
        issuer: issuer.to_string(),
        client_id: CLIENT_ID.into(),
        client_secret: CLIENT_SECRET.into(),
        redirect_uri: REDIRECT_URI.into(),
        scopes: vec!["openid".into(), "profile".into(), "email".into()],
        auto_provision,
        token_auth: manager::services::oidc::TokenAuth::Auto,
        groups_claim: "groups".into(),
        source: "database",
    }
}

async fn sso_app(oidc: Option<Arc<OidcRuntime>>, pool: DbPool) -> Router {
    let mut config: Config = test_config();
    config.server.public_base_url = "http://127.0.0.1:8080".into();
    let state = AppState {
        pool: pool.clone(),
        config,
        log_broadcast: LogBroadcaster::new(),
        oidc: std::sync::Arc::new(tokio::sync::RwLock::new(oidc)),
    };
    Router::new().nest("/api/v1", api::router(state))
}

/// `OidcRuntime::build_effective` with retries. Under nextest's loaded
/// runners the one-shot discovery fetch can starve past its 10s timeout even
/// with multi_thread runtimes — the readiness probe just proved the mock
/// answers, so a short retry is safe and kills the CI-only flake for good
/// (see PR #48, run 44532688240).
async fn build_mock_runtime(
    effective: &manager::services::oidc::EffectiveOidc,
) -> Option<Arc<OidcRuntime>> {
    for _ in 0..3 {
        if let Some(runtime) = OidcRuntime::build_effective(effective).await {
            return Some(runtime);
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    None
}

/// Same app but honoring `auto_provision = false` — the approval-queue mode.
async fn sso_app_jit_off(oidc: Arc<OidcRuntime>, pool: DbPool) -> Router {
    let mut config: Config = test_config();
    config.server.public_base_url = "http://127.0.0.1:8080".into();
    let state = AppState {
        pool: pool.clone(),
        config,
        log_broadcast: LogBroadcaster::new(),
        oidc: std::sync::Arc::new(tokio::sync::RwLock::new(Some(oidc))),
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

/// The callback leg only: start → planted code → callback. Returns the SPA
/// redirect target — the error form (`?error=…`) on refusal, the ticketed
/// success form otherwise.
async fn callback_leg(
    idp: &MockIdp,
    router: &Router,
    sub: &str,
    email: &str,
    name: &str,
) -> String {
    let (router, state, nonce) = start_flow(router).await;
    let code = idp.register_code(sub, email, name, &nonce);
    let (_, headers, _) = oneshot(
        router,
        "GET",
        &format!("/api/v1/auth/oidc/callback?code={code}&state={state}"),
        None,
    )
    .await;
    callback_location(&headers)
}

/// A sign-in that is refused at the callback: runs the callback leg and
/// asserts the refusal code, so queue tests can drive repeated attempts.
async fn refused_flow(
    idp: &MockIdp,
    router: &Router,
    sub: &str,
    email: &str,
    name: &str,
    expected_error: &str,
) {
    let next = callback_leg(idp, router, sub, email, name).await;
    assert!(
        next.contains(&format!("error={expected_error}")),
        "expected refusal {expected_error}, got: {next}"
    );
}

/// Sign a back-channel logout token with the mock key. `claims` is the full
/// payload so each validation case can bend exactly one rule.
fn sign_logout_token(claims: Value) -> String {
    let mut header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256);
    header.kid = Some(KEY_ID.to_string());
    jsonwebtoken::encode(&header, &claims, &mock_keys().encoding_key).expect("sign logout token")
}

/// A valid logout-token payload for `sub`, stamped now.
fn valid_logout_claims(issuer: &str, sub: &str) -> Value {
    json!({
        "iss": issuer,
        "aud": CLIENT_ID,
        "sub": sub,
        "iat": chrono::Utc::now().timestamp(),
        "jti": uuid::Uuid::new_v4().to_string(),
        "events": { "http://schemas.openid.net/event/backchannel-logout": {} },
    })
}

/// POST a form-encoded body and return the status (back-channel logout is
/// `application/x-www-form-urlencoded`).
async fn oneshot_form(router: Router, uri: &str, form: &[(&str, &str)]) -> StatusCode {
    let body = form
        .iter()
        .map(|(k, v)| format!("{k}={}", manager::services::oidc::percent_encode(v)))
        .collect::<Vec<_>>()
        .join("&");
    let request = Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(body))
        .unwrap();
    router.oneshot(request).await.unwrap().status()
}

const BACKCHANNEL_PATH: &str = "/api/v1/auth/oidc/backchannel-logout";

// ───────────────────────────── journeys ─────────────────────────────

/// The whole flow for a pre-bound identity: provisioning must not happen, the
/// JWT must work on `/auth/me`, and the identity profile is refreshed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bound_identity_signs_in_and_receives_a_local_equivalent_jwt() {
    let (idp, issuer) = MockIdp::start().await;
    let runtime = build_mock_runtime(&services_effective(&issuer, true))
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
    let runtime = build_mock_runtime(&services_effective(&issuer, true))
        .await
        .expect("runtime builds against the mock");
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
    let runtime = build_mock_runtime(&services_effective(&issuer, false))
        .await
        .expect("runtime builds against the mock");
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
    let runtime = build_mock_runtime(&services_effective(&issuer, true))
        .await
        .expect("runtime builds against the mock");
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
    let runtime = build_mock_runtime(&services_effective(&issuer, true))
        .await
        .expect("runtime builds against the mock");
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
    let runtime = build_mock_runtime(&services_effective(&issuer, true))
        .await
        .expect("runtime builds against the mock");
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
    let runtime = build_mock_runtime(&services_effective(&issuer, true))
        .await
        .expect("runtime builds against the mock");
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
    let runtime = build_mock_runtime(&services_effective(&issuer, true))
        .await
        .expect("runtime builds against the mock");
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
    let runtime = build_mock_runtime(&services_effective(&issuer, true))
        .await
        .expect("runtime builds against the mock");
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
    let runtime = build_mock_runtime(&services_effective(&issuer, true))
        .await
        .expect("runtime builds against the mock");
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
    let runtime = build_mock_runtime(&services_effective(&issuer, true))
        .await
        .expect("runtime builds against the mock");
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
            "token_auth": "client_secret_post",
            "groups_claim": "roles",
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
        body.pointer("/data/token_auth").and_then(Value::as_str),
        Some("client_secret_post"),
        "token endpoint auth override is reported"
    );
    assert_eq!(
        body.pointer("/data/groups_claim").and_then(Value::as_str),
        Some("roles"),
        "groups claim name is reported"
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

    // The two new knobs land in their columns.
    let stored = exec::fetch_optional(
        &pool,
        "SELECT token_auth, groups_claim FROM oidc_settings WHERE id = 1",
        &[],
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(stored.text_of("token_auth"), "client_secret_post");
    assert_eq!(stored.text_of("groups_claim"), "roles");

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
async fn settings_delete_clears_the_configuration() {
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
        Some("database"),
        "database is the only source — an empty one"
    );
    assert_eq!(
        body.pointer("/data/enabled"),
        Some(&json!(false)),
        "cleared configuration means SSO off"
    );
    assert_eq!(
        body.pointer("/data/client_secret_configured"),
        Some(&json!(false))
    );

    let (_, _, body) = oneshot(router, "GET", "/api/v1/auth/oidc/providers", None).await;
    assert_eq!(body.pointer("/data/enabled"), Some(&json!(false)));

    let rows: i64 = exec::fetch_scalar_i64(&pool, "SELECT COUNT(*) FROM oidc_settings", &[])
        .await
        .unwrap();
    assert_eq!(rows, 0, "configuration row dropped");
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

/// A first save without a secret is refused — the database is the only
/// source, there is no file/env fallback to lean on.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn settings_put_requires_the_secret_on_first_save() {
    let pool = common::test_db(4).await;
    seed_users(&pool).await;
    let router = settings_app(pool.clone()).await;
    let bearer = admin_bearer();

    let (status, body) = put_settings(
        &router,
        &bearer,
        json!({
            "enabled": true,
            "issuer": "https://sso.example.com",
            "client_id": "c",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        body.pointer("/message")
            .and_then(Value::as_str)
            .unwrap()
            .contains("client secret is required"),
        "{body}"
    );
    let rows: i64 = exec::fetch_scalar_i64(&pool, "SELECT COUNT(*) FROM oidc_settings", &[])
        .await
        .unwrap();
    assert_eq!(rows, 0);
}

/// The legacy `[auth.oidc]` file seed imports into the database row once at
/// startup and is then ignored: identical values, enabled stays on, the row
/// is stamped `file-import`, and a second boot with an existing row is a no-op.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn legacy_file_seed_imports_once_into_the_database() {
    let (idp, issuer) = MockIdp::start().await;
    let pool = common::test_db(4).await;
    seed_users(&pool).await;

    // The import runs against the real config shape — build one from the mock.
    let config = manager::config::OidcConfig {
        enabled: true,
        issuer: issuer.clone(),
        client_id: CLIENT_ID.into(),
        client_secret: CLIENT_SECRET.into(),
        client_secret_file: None,
        redirect_uri: REDIRECT_URI.into(),
        scopes: vec!["openid".into(), "profile".into(), "email".into()],
        auto_provision: true,
    };
    api::oidc_settings::import_file_seed(
        &pool,
        &config,
        "http://127.0.0.1:8080",
        common::JWT_SECRET,
    )
    .await;

    let row = exec::fetch_optional(
        &pool,
        "SELECT issuer, client_id, auto_provision, updated_by FROM oidc_settings WHERE id = 1",
        &[],
    )
    .await
    .unwrap()
    .expect("row imported");
    assert_eq!(row.text_of("issuer"), issuer);
    assert_eq!(row.text_of("client_id"), CLIENT_ID);
    assert_eq!(row.i64_of("auto_provision"), 1);
    assert_eq!(row.text_of("updated_by"), "file-import");

    // The imported row resolves and builds a working runtime: a full
    // sign-in round trip against the mock.
    let row = manager::store::oidc_settings::get(&pool)
        .await
        .unwrap()
        .expect("row exists");
    let effective = manager::services::oidc::resolve_effective(
        Some(&row),
        "http://127.0.0.1:8080",
        common::JWT_SECRET,
    )
    .expect("imported row resolves");
    assert!(effective.enabled);
    let runtime = build_mock_runtime(&effective)
        .await
        .expect("runtime builds from the imported row");
    let router = sso_app(Some(runtime), pool.clone()).await;
    let (_, body) = complete_flow(&idp, &router, "sub-bound", "boss@sso", "Boss").await;
    assert!(body
        .pointer("/data/token")
        .and_then(Value::as_str)
        .is_some());

    // A second import with an existing row must not touch it.
    api::oidc_settings::import_file_seed(
        &pool,
        &config,
        "http://127.0.0.1:8080",
        common::JWT_SECRET,
    )
    .await;
    let count: i64 = exec::fetch_scalar_i64(&pool, "SELECT COUNT(*) FROM oidc_settings", &[])
        .await
        .unwrap();
    assert_eq!(count, 1, "import never duplicates");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn legacy_file_seed_with_no_secret_is_not_imported() {
    let pool = common::test_db(4).await;
    seed_users(&pool).await;
    let config = manager::config::OidcConfig {
        enabled: true,
        issuer: "https://sso.example.com".into(),
        client_id: "c".into(),
        client_secret: String::new(),
        client_secret_file: Some("/nonexistent/secret".into()),
        redirect_uri: String::new(),
        scopes: vec!["openid".into()],
        auto_provision: true,
    };
    api::oidc_settings::import_file_seed(
        &pool,
        &config,
        "http://127.0.0.1:8080",
        common::JWT_SECRET,
    )
    .await;
    let count: i64 = exec::fetch_scalar_i64(&pool, "SELECT COUNT(*) FROM oidc_settings", &[])
        .await
        .unwrap();
    assert_eq!(count, 0, "an unusable seed imports nothing");
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

// ─────────────────── groups claim name (mirror only) ───────────────────

/// The configured claim name drives what is mirrored into
/// `external_identities.groups_json`; a provider that sends a single string
/// instead of an array is tolerated. Nothing here grants permissions.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn custom_groups_claim_is_mirrored_and_tolerates_a_single_string() {
    let (idp, issuer) = MockIdp::start().await;
    let mut effective = services_effective(&issuer, true);
    effective.groups_claim = "roles".into();
    let runtime = build_mock_runtime(&effective)
        .await
        .expect("runtime builds against the mock");
    let pool = common::test_db(4).await;
    seed_users(&pool).await;
    let router = sso_app(Some(runtime), pool.clone()).await;

    async fn sign_in_with(idp: &MockIdp, router: &Router, sub: &str, extra: Value) -> Value {
        let (router, state, nonce) = start_flow(router).await;
        let code = idp.register_code_with(sub, &format!("{sub}@sso"), sub, &nonce, extra);
        let (_, headers, _) = oneshot(
            router.clone(),
            "GET",
            &format!("/api/v1/auth/oidc/callback?code={code}&state={state}"),
            None,
        )
        .await;
        let location = callback_location(&headers);
        let ticket = cookie_value(&headers, "buz_sso_ticket")
            .unwrap_or_else(|| panic!("ticket cookie; callback location: {location}"));
        let (status, _, body) = oneshot(
            router,
            "POST",
            "/api/v1/auth/oidc/exchange",
            Some(&format!("buz_sso_ticket={ticket}")),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "exchange: {body}");
        body
    }

    async fn groups_of(pool: &DbPool, subject: &str) -> Vec<String> {
        let row = exec::fetch_optional(
            pool,
            "SELECT groups_json FROM external_identities WHERE subject = ?",
            &[subject.into()],
        )
        .await
        .unwrap()
        .unwrap_or_else(|| panic!("identity {subject} was not recorded"));
        serde_json::from_str(&row.text_of("groups_json")).unwrap()
    }

    let _ = sign_in_with(
        &idp,
        &router,
        "sub-array",
        json!({ "roles": ["dev", "ops"] }),
    )
    .await;
    assert_eq!(groups_of(&pool, "sub-array").await, vec!["dev", "ops"]);

    let _ = sign_in_with(&idp, &router, "sub-string", json!({ "roles": "solo" })).await;
    assert_eq!(groups_of(&pool, "sub-string").await, vec!["solo"]);

    // The default claim name is NOT what this runtime reads.
    let _ = sign_in_with(
        &idp,
        &router,
        "sub-default",
        json!({ "groups": ["ignored"] }),
    )
    .await;
    assert!(groups_of(&pool, "sub-default").await.is_empty());
}

// ─────────────────── session policy (TTL + client idle) ───────────────────

/// TTL and idle-roundtrip through the web API; the TTL is applied to tokens
/// minted afterwards, and `PUT null` clears both overrides.
#[tokio::test]
async fn session_policy_roundtrip_and_ttl_applies_to_new_tokens() {
    let pool = common::test_db(4).await;
    seed_users(&pool).await;
    let hash = manager::password::hash_password("secret-pass").unwrap();
    exec::execute(
        &pool,
        "INSERT INTO users (id, username, password_hash, display_name, is_platform_admin,
                            is_super_admin, is_ops, is_active, auth_source)
         VALUES ('ttl-1', 'ttl', ?, 'ttl', 0, 0, 0, 1, 'local')",
        &[hash.into()],
    )
    .await
    .unwrap();
    let router = settings_app(pool.clone()).await;
    let admin = admin_bearer();

    // Defaults: manager.toml TTL, idle off.
    let (status, _, body) = oneshot_with_headers(
        router.clone(),
        "GET",
        "/api/v1/auth/session-policy",
        None,
        Some(&member_bearer()),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.pointer("/data/ttl_hours"), Some(&json!(24)));
    assert_eq!(body.pointer("/data/ttl_overridden"), Some(&json!(false)));
    assert_eq!(body.pointer("/data/idle_minutes"), Some(&json!(0)));

    let (status, body) = oneshot_json(
        router.clone(),
        "PUT",
        "/api/v1/auth/session-policy",
        Some(&admin),
        json!({ "ttl_hours": 8, "idle_minutes": 15 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "put: {body}");
    assert_eq!(body.pointer("/data/ttl_hours"), Some(&json!(8)));
    assert_eq!(body.pointer("/data/ttl_overridden"), Some(&json!(true)));
    assert_eq!(body.pointer("/data/idle_minutes"), Some(&json!(15)));
    assert_eq!(body.pointer("/data/idle_overridden"), Some(&json!(true)));

    // A token minted after the change carries the overridden TTL.
    let (status, login) = oneshot_json(
        router.clone(),
        "POST",
        "/api/v1/auth/login",
        None,
        json!({ "username": "ttl", "password": "secret-pass" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "login: {login}");
    let token = login
        .pointer("/data/token")
        .and_then(Value::as_str)
        .expect("login token");
    let claims = TokenManager::new(common::JWT_SECRET).verify(token).unwrap();
    assert_eq!(
        claims.exp - claims.iat,
        8 * 3600,
        "new tokens use the overridden TTL"
    );

    // Out-of-range values are refused.
    let (status, body) = oneshot_json(
        router.clone(),
        "PUT",
        "/api/v1/auth/session-policy",
        Some(&admin),
        json!({ "ttl_hours": 0, "idle_minutes": 15 }),
    )
    .await;
    assert_ne!(status, StatusCode::OK, "ttl 0 refused: {body}");
    assert!(
        body.pointer("/message")
            .and_then(Value::as_str)
            .is_some_and(|m| m.contains("session_ttl_hours")),
        "{body}"
    );

    // null clears the overrides and returns to the defaults.
    let (status, body) = oneshot_json(
        router.clone(),
        "PUT",
        "/api/v1/auth/session-policy",
        Some(&admin),
        json!({ "ttl_hours": null, "idle_minutes": null }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "reset: {body}");
    assert_eq!(body.pointer("/data/ttl_hours"), Some(&json!(24)));
    assert_eq!(body.pointer("/data/ttl_overridden"), Some(&json!(false)));
    assert_eq!(body.pointer("/data/idle_minutes"), Some(&json!(0)));
    let overrides: i64 = exec::fetch_scalar_i64(
        &pool,
        "SELECT COUNT(*) FROM platform_settings WHERE key IN
            ('session_ttl_hours', 'session_idle_minutes')",
        &[],
    )
    .await
    .unwrap();
    assert_eq!(overrides, 0, "reset deletes the override rows");
}

/// Changing the policy is a platform-admin action; any signed-in user may read
/// the effective values (the SPA needs the idle value).
#[tokio::test]
async fn session_policy_write_requires_platform_admin() {
    let pool = common::test_db(4).await;
    seed_users(&pool).await;
    let router = settings_app(pool).await;

    let (status, _, _) = oneshot_with_headers(
        router.clone(),
        "GET",
        "/api/v1/auth/session-policy",
        None,
        Some(&member_bearer()),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, _) = oneshot_json(
        router,
        "PUT",
        "/api/v1/auth/session-policy",
        Some(&member_bearer()),
        json!({ "ttl_hours": 8, "idle_minutes": 15 }),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

// ─────────────────── recent SSO events (settings card) ───────────────────

/// Platform administrators see platform-wide SSO events (not only their own
/// actor rows), and the card never includes local password logins.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn oidc_events_are_platform_wide_but_exclude_local_logins() {
    let (idp, issuer) = MockIdp::start().await;
    let runtime = build_mock_runtime(&services_effective(&issuer, true))
        .await
        .expect("runtime builds against the mock");
    let pool = common::test_db(4).await;
    seed_users(&pool).await;
    let hash = manager::password::hash_password("secret-pass").unwrap();
    exec::execute(
        &pool,
        "INSERT INTO users (id, username, password_hash, display_name, is_platform_admin,
                            is_super_admin, is_ops, is_active, auth_source)
         VALUES ('local-1', 'local', ?, 'local', 0, 0, 0, 1, 'local')",
        &[hash.into()],
    )
    .await
    .unwrap();
    let router = sso_app(Some(runtime), pool.clone()).await;

    // One SSO sign-in (provision event), a second for the same identity
    // (login event), and one local login.
    let (_, _) = complete_flow(&idp, &router, "sub-events", "events@sso", "Events").await;
    let (_, _) = complete_flow(&idp, &router, "sub-events", "events@sso", "Events").await;
    let (status, _) = oneshot_json(
        router.clone(),
        "POST",
        "/api/v1/auth/login",
        None,
        json!({ "username": "local", "password": "secret-pass" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // `admin-1` is a platform admin but NOT a super admin — the audit-log API
    // would restrict them to their own rows; this endpoint must not.
    let (status, _, body) = oneshot_with_headers(
        router.clone(),
        "GET",
        "/api/v1/auth/oidc/events?page=1&page_size=50",
        None,
        Some(&admin_bearer()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "events: {body}");
    let items = body
        .pointer("/data/items")
        .and_then(Value::as_array)
        .unwrap();
    let actions: Vec<&str> = items
        .iter()
        .filter_map(|item| item.get("action").and_then(Value::as_str))
        .collect();
    assert!(actions.contains(&"auth.oidc_login"), "{actions:?}");
    assert!(actions.contains(&"auth.oidc_provision"), "{actions:?}");
    assert!(
        !actions.contains(&"auth.login"),
        "local password logins stay out of the SSO card: {actions:?}"
    );

    // The backend writes the Detail sentence (the UI never composes it).
    let login_event = items
        .iter()
        .find(|item| item.get("action").and_then(Value::as_str) == Some("auth.oidc_login"))
        .expect("login event");
    let detail = login_event
        .pointer("/after/detail")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(
        detail.starts_with("mapped to account "),
        "login detail: {detail:?}"
    );
    let total = body
        .pointer("/data/total")
        .and_then(Value::as_i64)
        .expect("total");
    assert!(total >= 2, "both SSO events are counted: {total}");

    // Pagination: page 2 with one row per page returns exactly the older row.
    let (status, _, page2) = oneshot_with_headers(
        router,
        "GET",
        "/api/v1/auth/oidc/events?page=2&page_size=1",
        None,
        Some(&admin_bearer()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "page 2: {page2}");
    assert_eq!(page2.pointer("/data/page"), Some(&json!(2)));
    assert_eq!(
        page2
            .pointer("/data/items")
            .and_then(Value::as_array)
            .map(Vec::len),
        Some(1)
    );
}

// ─────────────────── back-channel logout (session revocation) ───────────────────

/// A provider-delivered revocation kills the user's existing JWT within the
/// same request, while a login that happens after the event works again.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn backchannel_logout_revokes_the_users_jwt_and_allows_a_fresh_login() {
    let (idp, issuer) = MockIdp::start().await;
    let runtime = build_mock_runtime(&services_effective(&issuer, true))
        .await
        .expect("runtime builds against the mock");
    let pool = common::test_db(4).await;
    seed_users(&pool).await;
    bind_identity(&pool, "sub-bound", "admin-1").await;
    let router = sso_app(Some(runtime), pool.clone()).await;

    let (_, login) = complete_flow(&idp, &router, "sub-bound", "boss@sso", "Boss").await;
    let token = login
        .pointer("/data/token")
        .and_then(Value::as_str)
        .expect("login token")
        .to_string();
    let (status, _, _) =
        oneshot_with_headers(router.clone(), "GET", "/api/v1/auth/me", None, Some(&token)).await;
    assert_eq!(status, StatusCode::OK, "token works before revocation");

    let logout = sign_logout_token(valid_logout_claims(&issuer, "sub-bound"));
    let status = oneshot_form(
        router.clone(),
        BACKCHANNEL_PATH,
        &[("logout_token", &logout)],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "valid logout token accepted");

    let (status, _, _) =
        oneshot_with_headers(router.clone(), "GET", "/api/v1/auth/me", None, Some(&token)).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "the revoked JWT must be refused"
    );

    // The stamp is second-granular: wait for the next second so the fresh
    // token is strictly newer than the revocation instant.
    tokio::time::sleep(tokio::time::Duration::from_millis(1100)).await;
    let (_, login) = complete_flow(&idp, &router, "sub-bound", "boss@sso", "Boss").await;
    let fresh = login
        .pointer("/data/token")
        .and_then(Value::as_str)
        .expect("fresh token")
        .to_string();
    let (status, _, _) =
        oneshot_with_headers(router.clone(), "GET", "/api/v1/auth/me", None, Some(&fresh)).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a login after the event must work again"
    );

    let audited: i64 = exec::fetch_scalar_i64(
        &pool,
        "SELECT COUNT(*) FROM audit_logs WHERE action = 'auth.oidc_backchannel_logout'",
        &[],
    )
    .await
    .unwrap();
    assert_eq!(audited, 1, "the system-actor revocation is audited");
}

/// Every shape a forged/broken logout token can take is refused with 400 and
/// leaves the live session untouched.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn backchannel_logout_rejects_tokens_that_do_not_verify() {
    let (idp, issuer) = MockIdp::start().await;
    let runtime = build_mock_runtime(&services_effective(&issuer, true))
        .await
        .expect("runtime builds against the mock");
    let pool = common::test_db(4).await;
    seed_users(&pool).await;
    bind_identity(&pool, "sub-bound", "admin-1").await;
    let router = sso_app(Some(runtime), pool.clone()).await;

    let (_, login) = complete_flow(&idp, &router, "sub-bound", "boss@sso", "Boss").await;
    let token = login
        .pointer("/data/token")
        .and_then(Value::as_str)
        .expect("login token")
        .to_string();

    let now = chrono::Utc::now().timestamp();
    let mut cases: Vec<(&str, Value)> = Vec::new();

    let mut wrong_aud = valid_logout_claims(&issuer, "sub-bound");
    wrong_aud["aud"] = json!("some-other-client");
    cases.push(("wrong audience", wrong_aud));

    let mut wrong_iss = valid_logout_claims(&issuer, "sub-bound");
    wrong_iss["iss"] = json!(format!("{issuer}/someone-else"));
    cases.push(("wrong issuer", wrong_iss));

    let mut with_nonce = valid_logout_claims(&issuer, "sub-bound");
    with_nonce["nonce"] = json!("should-not-be-here");
    cases.push(("nonce present", with_nonce));

    let mut no_event = valid_logout_claims(&issuer, "sub-bound");
    no_event["events"] = json!({});
    cases.push(("missing logout event", no_event));

    let mut stale = valid_logout_claims(&issuer, "sub-bound");
    stale["iat"] = json!(now - 3600);
    cases.push(("stale iat", stale));

    let mut future = valid_logout_claims(&issuer, "sub-bound");
    future["iat"] = json!(now + 3600);
    cases.push(("future iat", future));

    for (label, claims) in cases {
        let signed = sign_logout_token(claims);
        let status = oneshot_form(
            router.clone(),
            BACKCHANNEL_PATH,
            &[("logout_token", &signed)],
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "case: {label}");
        let (status, _, _) =
            oneshot_with_headers(router.clone(), "GET", "/api/v1/auth/me", None, Some(&token))
                .await;
        assert_eq!(status, StatusCode::OK, "session untouched after {label}");
    }

    // A malformed body (no logout_token) is a 400 too.
    let status = oneshot_form(router.clone(), BACKCHANNEL_PATH, &[]).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "missing logout_token");

    // The same valid token twice stays idempotent.
    let logout = sign_logout_token(valid_logout_claims(&issuer, "sub-bound"));
    for _ in 0..2 {
        let status = oneshot_form(
            router.clone(),
            BACKCHANNEL_PATH,
            &[("logout_token", &logout)],
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }
}

/// An unknown subject is acknowledged (no account enumeration) and revokes
/// nothing; API tokens owned by the user survive a real revocation.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn backchannel_logout_is_unknown_subject_safe_and_leaves_api_tokens_alone() {
    let (idp, issuer) = MockIdp::start().await;
    let runtime = build_mock_runtime(&services_effective(&issuer, true))
        .await
        .expect("runtime builds against the mock");
    let pool = common::test_db(4).await;
    seed_users(&pool).await;
    bind_identity(&pool, "sub-bound", "admin-1").await;
    let router = sso_app(Some(runtime), pool.clone()).await;

    let (_, login) = complete_flow(&idp, &router, "sub-bound", "boss@sso", "Boss").await;
    let jwt = login
        .pointer("/data/token")
        .and_then(Value::as_str)
        .expect("login token")
        .to_string();

    // An identity Buz has never seen: acknowledged, nothing revoked.
    let ghost = sign_logout_token(valid_logout_claims(&issuer, "sub-ghost"));
    let status = oneshot_form(
        router.clone(),
        BACKCHANNEL_PATH,
        &[("logout_token", &ghost)],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, _) =
        oneshot_with_headers(router.clone(), "GET", "/api/v1/auth/me", None, Some(&jwt)).await;
    assert_eq!(status, StatusCode::OK, "unknown subject revokes nothing");

    // A Buz API token is a Buz-owned credential: the revocation targets user
    // JWTs only.
    let raw_api_token = "buz_backchannel_test_token";
    let hash = shared::api_token::hash_token(raw_api_token);
    manager::store::api_tokens::insert(
        &pool,
        "tok-revocation",
        "admin-1",
        "revocation test",
        &hash,
        "buz_back",
        "full",
    )
    .await
    .unwrap();
    let (status, _, _) = oneshot_with_headers(
        router.clone(),
        "GET",
        "/api/v1/auth/me",
        None,
        Some(raw_api_token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "API token works before revocation");

    let logout = sign_logout_token(valid_logout_claims(&issuer, "sub-bound"));
    let status = oneshot_form(
        router.clone(),
        BACKCHANNEL_PATH,
        &[("logout_token", &logout)],
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, _, _) = oneshot_with_headers(
        router.clone(),
        "GET",
        "/api/v1/auth/me",
        None,
        Some(raw_api_token),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "API tokens are not invalidated by IdP revocation"
    );
    let (status, _, _) =
        oneshot_with_headers(router.clone(), "GET", "/api/v1/auth/me", None, Some(&jwt)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "the JWT is revoked");
}

// ───────────────────── identity approval queue (auto_provision off) ─────────────────────

/// The queue endpoint payloads.
async fn pending_list(router: &Router, bearer: &str) -> (StatusCode, Value) {
    oneshot_json(
        router.clone(),
        "GET",
        "/api/v1/auth/oidc/identities/pending",
        Some(bearer),
        Value::Null,
    )
    .await
}

async fn bind_pending(router: &Router, bearer: &str, id: &str, body: Value) -> (StatusCode, Value) {
    oneshot_json(
        router.clone(),
        "POST",
        &format!("/api/v1/auth/oidc/identities/pending/{id}/bind"),
        Some(bearer),
        body,
    )
    .await
}

async fn dismiss_pending(router: &Router, bearer: &str, id: &str) -> (StatusCode, Value) {
    oneshot_json(
        router.clone(),
        "DELETE",
        &format!("/api/v1/auth/oidc/identities/pending/{id}"),
        Some(bearer),
        Value::Null,
    )
    .await
}

async fn one_pending(pool: &DbPool) -> (String, String, i64) {
    let row = exec::fetch_optional(
        pool,
        "SELECT id, subject, attempts FROM pending_identities ORDER BY first_seen_at ASC",
        &[],
    )
    .await
    .unwrap()
    .expect("pending row exists");
    (
        row.text_of("id"),
        row.text_of("subject"),
        row.i64_of("attempts"),
    )
}

/// A refused sign-in lands in the queue and every retry refreshes it; the
/// queue requires a platform admin; approval by creation yields a plain SSO
/// user who can then actually sign in.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refused_identity_is_queued_then_approved_by_creating_a_user() {
    let (idp, issuer) = MockIdp::start().await;
    let runtime = build_mock_runtime(&services_effective(&issuer, false))
        .await
        .expect("runtime builds against the mock");
    let pool = common::test_db(4).await;
    seed_users(&pool).await;
    let router = sso_app_jit_off(runtime, pool.clone()).await;
    let bearer = admin_bearer();

    // First refused attempt: recorded once.
    refused_flow(&idp, &router, "sub-dave", "dave@sso", "Dave", "not-linked").await;
    let (id, subject, attempts) = one_pending(&pool).await;
    assert_eq!(subject, "sub-dave");
    assert_eq!(attempts, 1);

    // Second attempt refreshes the same row.
    refused_flow(&idp, &router, "sub-dave", "dave@sso", "Dave", "not-linked").await;
    let (_, _, attempts) = one_pending(&pool).await;
    assert_eq!(attempts, 2, "retries are counted, not duplicated");

    // The queue is platform-admin only.
    let (status, _) = pending_list(&router, &member_bearer()).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // Approve by creating a user (explicit username).
    let (status, body) = bind_pending(
        &router,
        &bearer,
        &id,
        json!({ "create_username": "dave", "disable_local_password": true }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "approve: {body}");
    let user_id = body
        .pointer("/data/user_id")
        .and_then(Value::as_str)
        .unwrap()
        .to_string();
    assert_eq!(body.pointer("/data/created_user"), Some(&json!(true)));

    // Queue is empty; the created account is a plain oidc user; users list
    // exposes the origin.
    let rows: i64 = exec::fetch_scalar_i64(&pool, "SELECT COUNT(*) FROM pending_identities", &[])
        .await
        .unwrap();
    assert_eq!(rows, 0);
    let (status, body) = oneshot_json(
        router.clone(),
        "GET",
        "/api/v1/users",
        Some(&bearer),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let dave = body
        .pointer("/data")
        .and_then(Value::as_array)
        .unwrap()
        .iter()
        .find(|u| u["id"].as_str() == Some(user_id.as_str()))
        .expect("created user listed")
        .clone();
    assert_eq!(dave["auth_source"], json!("oidc"));

    // The audit trail carries the approval (admin actor).
    let audited: i64 = exec::fetch_scalar_i64(
        &pool,
        "SELECT COUNT(*) FROM audit_logs WHERE action = 'auth.oidc_identity_approve'",
        &[],
    )
    .await
    .unwrap();
    assert_eq!(audited, 1);

    // The identity can now actually sign in.
    let (next, login) = complete_flow(&idp, &router, "sub-dave", "dave@sso", "Dave").await;
    assert_eq!(next, "/auth/sso/callback?next=%2Fpipelines");
    assert!(login
        .pointer("/data/token")
        .and_then(Value::as_str)
        .is_some());
}

/// Approval by binding keeps the local password by default; with the
/// explicit flag the account flips to SSO-only and the password is refused.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bind_to_existing_user_honors_the_password_choice() {
    let (idp, issuer) = MockIdp::start().await;
    let runtime = build_mock_runtime(&services_effective(&issuer, false))
        .await
        .expect("runtime builds against the mock");
    let pool = common::test_db(4).await;
    seed_users(&pool).await;
    // A second plain local user for the disable-password branch.
    exec::execute(
        &pool,
        "INSERT INTO users (id, username, password_hash, display_name, is_platform_admin, is_super_admin, is_ops, is_active, auth_source)
         VALUES ('erin-1', 'erin', 'hash', 'erin', 0, 0, 0, 1, 'local')",
        &[],
    )
    .await
    .unwrap();
    let router = sso_app_jit_off(runtime, pool.clone()).await;
    let bearer = admin_bearer();

    async fn auth_source_of(pool: &DbPool, user_id: &str) -> String {
        let row = exec::fetch_optional(
            pool,
            "SELECT auth_source FROM users WHERE id = ?",
            &[user_id.into()],
        )
        .await
        .unwrap()
        .unwrap();
        row.text_of("auth_source")
    }

    // Bind carol keeping the password (default).
    refused_flow(
        &idp,
        &router,
        "sub-carol",
        "carol@sso",
        "Carol",
        "not-linked",
    )
    .await;
    let (id, _, _) = one_pending(&pool).await;
    let (status, body) = bind_pending(&router, &bearer, &id, json!({ "user_id": "carol-1" })).await;
    assert_eq!(status, StatusCode::OK, "bind: {body}");
    assert_eq!(
        body.pointer("/data/password_login_disabled"),
        Some(&json!(false))
    );
    assert_eq!(auth_source_of(&pool, "carol-1").await, "local");

    // A second identity for carol conflicts on UNIQUE (provider, user_id).
    refused_flow(
        &idp,
        &router,
        "sub-carol2",
        "carol2@sso",
        "Carol II",
        "not-linked",
    )
    .await;
    let (id2, _, _) = one_pending(&pool).await;
    let (status, _) = bind_pending(&router, &bearer, &id2, json!({ "user_id": "carol-1" })).await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, _) = dismiss_pending(&router, &bearer, &id2).await;
    assert_eq!(status, StatusCode::OK);

    // Bind erin with the explicit flag: her account flips to SSO-only.
    refused_flow(&idp, &router, "sub-erin", "erin@sso", "Erin", "not-linked").await;
    let (id3, _, _) = one_pending(&pool).await;
    let (status, body) = bind_pending(
        &router,
        &bearer,
        &id3,
        json!({ "user_id": "erin-1", "disable_local_password": true }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "bind erin: {body}");
    assert_eq!(
        body.pointer("/data/password_login_disabled"),
        Some(&json!(true))
    );
    assert_eq!(auth_source_of(&pool, "erin-1").await, "oidc");

    // Password login is now refused at the origin gate…
    let (_, body) = oneshot_json(
        router.clone(),
        "POST",
        "/api/v1/auth/login",
        None,
        json!({ "username": "erin", "password": "whatever" }),
    )
    .await;
    assert_ne!(
        body.pointer("/code"),
        Some(&json!(0)),
        "login must fail: {body}"
    );
    // …and the SSO sign-in works.
    let (next, login) = complete_flow(&idp, &router, "sub-erin", "erin@sso", "Erin").await;
    assert_eq!(next, "/auth/sso/callback?next=%2Fpipelines");
    assert!(login
        .pointer("/data/token")
        .and_then(Value::as_str)
        .is_some());
}

/// Binding an SSO identity onto an account that already has one, a disabled
/// account, or an SSO-origin account is refused in-tx with clear errors.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bind_refuses_conflicts_without_side_effects() {
    let (idp, issuer) = MockIdp::start().await;
    let runtime = build_mock_runtime(&services_effective(&issuer, false))
        .await
        .expect("runtime builds against the mock");
    let pool = common::test_db(4).await;
    seed_users(&pool).await;
    bind_identity(&pool, "sub-existing", "carol-1").await;
    let router = sso_app_jit_off(runtime, pool.clone()).await;
    let bearer = admin_bearer();

    // Unknown user.
    refused_flow(&idp, &router, "sub-x", "x@sso", "X", "not-linked").await;
    let (id, _, _) = one_pending(&pool).await;
    let (status, _) = bind_pending(&router, &bearer, &id, json!({ "user_id": "nope" })).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Carol already carries an identity for this provider.
    let (status, body) = bind_pending(&router, &bearer, &id, json!({ "user_id": "carol-1" })).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");

    // Still queued — the failed binds consumed nothing.
    let (same_id, _, _) = one_pending(&pool).await;
    assert_eq!(same_id, id);

    // Dismiss removes it; a subsequent refused sign-in re-records it.
    let (status, _) = dismiss_pending(&router, &bearer, &id).await;
    assert_eq!(status, StatusCode::OK);
    let rows: i64 = exec::fetch_scalar_i64(&pool, "SELECT COUNT(*) FROM pending_identities", &[])
        .await
        .unwrap();
    assert_eq!(rows, 0);
    let dismissed: i64 = exec::fetch_scalar_i64(
        &pool,
        "SELECT COUNT(*) FROM audit_logs WHERE action = 'auth.oidc_identity_reject'",
        &[],
    )
    .await
    .unwrap();
    assert_eq!(dismissed, 1);
    refused_flow(&idp, &router, "sub-x", "x@sso", "X", "not-linked").await;
    let (_, _, attempts) = one_pending(&pool).await;
    assert_eq!(attempts, 1, "re-recorded after dismissal");
}

/// The last active local platform admin cannot lose password login through
/// the approval flow (break-glass invariant).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn last_local_admin_password_cannot_be_disabled_via_bind() {
    let (idp, issuer) = MockIdp::start().await;
    let runtime = build_mock_runtime(&services_effective(&issuer, false))
        .await
        .expect("runtime builds against the mock");
    let pool = common::test_db(4).await;
    seed_users(&pool).await;
    let router = sso_app_jit_off(runtime, pool.clone()).await;
    let bearer = admin_bearer();

    refused_flow(&idp, &router, "sub-boss", "boss@sso", "Boss", "not-linked").await;
    let (id, _, _) = one_pending(&pool).await;
    let (status, body) = bind_pending(
        &router,
        &bearer,
        &id,
        json!({ "user_id": "admin-1", "disable_local_password": true }),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    // Nothing was bound.
    let bound: i64 = exec::fetch_scalar_i64(
        &pool,
        "SELECT COUNT(*) FROM external_identities WHERE subject = 'sub-boss'",
        &[],
    )
    .await
    .unwrap();
    assert_eq!(bound, 0, "the failed bind must roll back");
}

// ─────────────────── RP-initiated logout + auth-method negotiation ───────────────────

/// Logout after a real sign-in: the callback plants the RP-logout hint
/// cookie, and `logout` turns it into a top-level 303 to the provider's
/// end-session endpoint with the id_token hint and the registered landing —
/// a GET navigation, so the provider's Lax session cookie rides along.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rp_logout_redirects_to_the_provider_end_session() {
    let (idp, issuer) = MockIdp::start().await;
    let runtime = build_mock_runtime(&services_effective(&issuer, true))
        .await
        .expect("runtime builds against the mock");
    let pool = common::test_db(4).await;
    seed_users(&pool).await;
    bind_identity(&pool, "sub-bound", "admin-1").await;
    let router = sso_app(Some(runtime), pool.clone()).await;

    // Sign in; grab the hint cookie the callback plants.
    let (router, state, nonce) = start_flow(&router).await;
    let code = idp.register_code("sub-bound", "boss@sso", "Boss", &nonce);
    let (_, headers, _) = oneshot(
        router.clone(),
        "GET",
        &format!("/api/v1/auth/oidc/callback?code={code}&state={state}"),
        None,
    )
    .await;
    assert!(
        cookie_value(&headers, "buz_sso_ticket").is_some(),
        "ticket set"
    );
    let hint = cookie_value(&headers, "buz_oidc_hint")
        .unwrap_or_else(|| panic!("callback plants the RP-logout hint cookie"));

    // Sign out: 303 to the provider end-session endpoint with the hint.
    let request = Request::builder()
        .method("GET")
        .uri("/api/v1/auth/oidc/logout")
        .header(header::COOKIE, format!("buz_oidc_hint={hint}"))
        .body(Body::empty())
        .unwrap();
    let response = router.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        response
            .headers()
            .get(header::CACHE_CONTROL)
            .and_then(|v| v.to_str().ok()),
        Some("no-store")
    );
    let location = response
        .headers()
        .get(header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .unwrap()
        .to_string();
    assert!(
        location.starts_with(&format!("{issuer}/oauth/end_session?")),
        "targets the provider: {location}"
    );
    assert!(location.contains("id_token_hint="), "carries the hint");
    assert!(
        location.contains(&format!("client_id={CLIENT_ID}")),
        "carries client_id"
    );
    let landing = manager::services::oidc::EffectiveOidc::derived_post_logout_redirect_uri(
        "http://127.0.0.1:8080",
    )
    .unwrap();
    assert!(
        location.contains(&manager::services::oidc::percent_encode(&landing)),
        "carries the registered landing: {location}"
    );

    // The hint cookie is cleared on the way out.
    let cleared = response
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .any(|v| v.to_str().unwrap_or("").starts_with("buz_oidc_hint=;"));
    assert!(cleared, "hint cookie cleared: {location}");

    // The provider's GET end-session (what the browser lands on) honors the
    // registered landing — Signet-shaped: hint present, redirect out.
    let request = Request::builder()
        .method("GET")
        .uri(&location)
        .body(Body::empty())
        .unwrap();
    let response = idp.router().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        response
            .headers()
            .get(header::LOCATION)
            .and_then(|v| v.to_str().ok()),
        Some(landing.as_str())
    );
}

/// An expired hint never reaches the provider: sign-out degrades to the
/// local landing instead of stranding the user on a provider error page.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rp_logout_with_an_expired_hint_degrades_to_local() {
    let (_idp, issuer) = MockIdp::start().await;
    let runtime = build_mock_runtime(&services_effective(&issuer, true))
        .await
        .expect("runtime builds against the mock");
    let pool = common::test_db(4).await;
    seed_users(&pool).await;
    let router = sso_app(Some(runtime), pool.clone()).await;

    let expired = sign_logout_token(json!({ "iss": issuer, "aud": CLIENT_ID, "sub": "sub-x",
                "iat": chrono::Utc::now().timestamp() - 7200,
                "exp": chrono::Utc::now().timestamp() - 3600 }));
    let request = Request::builder()
        .method("GET")
        .uri("/api/v1/auth/oidc/logout")
        .header(header::COOKIE, format!("buz_oidc_hint={expired}"))
        .body(Body::empty())
        .unwrap();
    let response = router.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        response
            .headers()
            .get(header::LOCATION)
            .and_then(|v| v.to_str().ok()),
        Some("/auth/sso/logged-out")
    );
}

/// A provider without an end-session endpoint → purely local sign-out; SSO
/// off → straight to the login page.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rp_logout_falls_back_to_local_without_provider_support() {
    let (idp, issuer) = MockIdp::start_with(vec![], false).await;
    let runtime = build_mock_runtime(&services_effective(&issuer, true))
        .await
        .expect("runtime builds against the mock");
    let pool = common::test_db(4).await;
    seed_users(&pool).await;
    let router = sso_app(Some(runtime), pool.clone()).await;

    // Sign in so a hint cookie exists, then sign out.
    let (_, login) = complete_flow(&idp, &router, "sub-bound", "boss@sso", "Boss").await;
    assert!(login.pointer("/data/token").is_some());
    let request = Request::builder()
        .method("GET")
        .uri("/api/v1/auth/oidc/logout")
        .header(header::COOKIE, "buz_oidc_hint=whatever")
        .body(Body::empty())
        .unwrap();
    let response = router.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        response
            .headers()
            .get(header::LOCATION)
            .and_then(|v| v.to_str().ok()),
        Some("/auth/sso/logged-out")
    );

    // SSO not configured at all.
    let pool = common::test_db(4).await;
    seed_users(&pool).await;
    let router = sso_app(None, pool.clone()).await;
    let request = Request::builder()
        .method("GET")
        .uri("/api/v1/auth/oidc/logout")
        .body(Body::empty())
        .unwrap();
    let response = router.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        response
            .headers()
            .get(header::LOCATION)
            .and_then(|v| v.to_str().ok()),
        Some("/login")
    );
}

/// Discovery declaring basic-only switches the token request to Basic
/// credentials (the mock refuses body credentials in that mode).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn token_auth_method_is_negotiated_from_discovery() {
    let (idp, issuer) = MockIdp::start_with(vec!["client_secret_basic"], true).await;
    let runtime = build_mock_runtime(&services_effective(&issuer, true))
        .await
        .expect("runtime builds against the mock");
    let pool = common::test_db(4).await;
    seed_users(&pool).await;
    bind_identity(&pool, "sub-basic", "admin-1").await;
    let router = sso_app(Some(runtime), pool.clone()).await;

    // A full round trip only succeeds when the mock accepted Basic auth.
    let (next, body) = complete_flow(&idp, &router, "sub-basic", "boss@sso", "Boss").await;
    assert_eq!(next, "/auth/sso/callback?next=%2Fpipelines");
    assert!(body
        .pointer("/data/token")
        .and_then(Value::as_str)
        .is_some());
}
