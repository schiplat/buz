//! Integration tests for API token creation, authentication, revocation, and

use axum::{
    body::Body,
    http::{header::AUTHORIZATION, Request, StatusCode},
    Router,
};
use http_body_util::BodyExt;
use shared::auth::{Role, TokenManager};
use sqlx::sqlite::{SqlitePool, SqlitePoolOptions};
use tower::ServiceExt;

use manager::api;
use manager::config::AuthConfig;
use manager::config::Config;
use manager::config::PipelineConfig;
use manager::config::RunnerConfig;
use manager::config::ServerConfig;
use manager::db::run_migrations;
use manager::services::log_broadcast::LogBroadcaster;
use manager::state::AppState;

const JWT_SECRET: &str = "test-secret-0123456789-abcdefghijklmnop";

const SUPER_ADMIN_ID: &str = "admin-1";
const REGULAR_ADMIN_ID: &str = "admin-2";
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
        pipeline: PipelineConfig::default(),
        standalone: Default::default(),
        plugin_index: Default::default(),
        storage: Default::default(),
        logging: Default::default(),
        marketplace_url: String::new(),
    }
}

async fn test_app() -> Router {
    test_app_with_pool().await.0
}

/// Returns the app plus its pool, for tests that need to seed rows directly
/// (e.g. a legacy `full`-scope token that the API no longer creates).
async fn test_app_with_pool() -> (Router, SqlitePool) {
    let pool = SqlitePoolOptions::new()
        .max_connections(4)
        .connect("sqlite::memory:")
        .await
        .expect("open in-memory db");
    run_migrations(&pool, JWT_SECRET).await.expect("migrate db");

    // Seed a super admin + a regular platform admin + a normal user.
    for (id, username, is_admin, is_super) in [
        ("admin-1", "boss", 1, 1),
        ("admin-2", "ops", 1, 0),
        ("user-1", "pleb", 0, 0),
    ] {
        sqlx::query(
            "INSERT INTO users (id, username, password_hash, display_name, is_platform_admin, is_super_admin, is_active)
             VALUES (?, ?, 'hash', ?, ?, ?, 1)",
        )
        .bind(id)
        .bind(username)
        .bind(username)
        .bind(is_admin)
        .bind(is_super)
        .execute(&pool)
        .await
        .expect("seed user");
    }

    let state = AppState {
        pool: pool.clone(),
        config: test_config(),
        log_broadcast: LogBroadcaster::new(),
    };
    (Router::new().nest("/api/v1", api::router(state)), pool)
}

fn bearer(token: &str) -> String {
    format!("Bearer {token}")
}

fn admin_jwt(role: Role, sub: &str) -> String {
    TokenManager::new(JWT_SECRET)
        .create_token(sub, role, 1)
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

#[tokio::test]
async fn create_token_returns_plaintext_once_and_hashes_in_db() {
    let app = test_app().await;
    let jwt = admin_jwt(Role::Admin, REGULAR_ADMIN_ID);

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/api-tokens")
                .header(AUTHORIZATION, bearer(&jwt))
                .header("Content-Type", "application/json")
                .body(Body::from(r#"{"name":"ci-script"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = read_body(resp).await;
    assert_eq!(body["data"]["name"], "ci-script");
    // Omitted scope defaults to the least-privilege band.
    assert_eq!(body["data"]["scope"], "read");
    let token = body["data"]["token"].as_str().expect("plaintext token");
    assert!(token.starts_with("buz_"), "token prefix");
    assert!(body["data"]["prefix"].as_str().unwrap().ends_with('…'));
}

#[tokio::test]
async fn create_token_rejects_full_scope() {
    let app = test_app().await;
    let jwt = admin_jwt(Role::Admin, REGULAR_ADMIN_ID);

    for scope in ["full", "nonsense"] {
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/api-tokens")
                    .header(AUTHORIZATION, bearer(&jwt))
                    .header("Content-Type", "application/json")
                    .body(Body::from(format!(
                        r#"{{"name":"ci-script","scope":"{scope}"}}"#
                    )))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "scope {scope}"
        );
    }
}

#[tokio::test]
async fn ops_token_works_on_operations_plane() {
    let app = test_app().await;
    let jwt = admin_jwt(Role::Admin, REGULAR_ADMIN_ID);

    let create = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/api-tokens")
                .header(AUTHORIZATION, bearer(&jwt))
                .header("Content-Type", "application/json")
                .body(Body::from(r#"{"name":"ci-script","scope":"ops"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    let created = read_body(create).await;
    assert_eq!(created["data"]["scope"], "ops");
    let token = created["data"]["token"].as_str().expect("plaintext token");

    let me = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/auth/me")
                .header(AUTHORIZATION, bearer(token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(me.status(), StatusCode::OK);
    let me_body = read_body(me).await;
    assert_eq!(me_body["data"]["username"], "ops");
    assert_eq!(me_body["data"]["is_platform_admin"], true);
    assert_eq!(me_body["data"]["is_super_admin"], false);

    // Write on the operations plane (team management) is allowed.
    let create_team = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/teams")
                .header(AUTHORIZATION, bearer(token))
                .header("Content-Type", "application/json")
                .body(Body::from(r#"{"name":"ci-team","slug":"ci-team"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(create_team.status(), StatusCode::OK);
}

#[tokio::test]
async fn read_token_rejects_writes() {
    let app = test_app().await;
    let jwt = admin_jwt(Role::Admin, REGULAR_ADMIN_ID);

    let create = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/api-tokens")
                .header(AUTHORIZATION, bearer(&jwt))
                .header("Content-Type", "application/json")
                .body(Body::from(r#"{"name":"dashboard","scope":"read"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    let created = read_body(create).await;
    let token = created["data"]["token"].as_str().expect("plaintext token");

    let list = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/teams")
                .header(AUTHORIZATION, bearer(token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(list.status(), StatusCode::OK, "GET allowed for read scope");

    let write = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/teams")
                .header(AUTHORIZATION, bearer(token))
                .header("Content-Type", "application/json")
                .body(Body::from(r#"{"name":"nope","slug":"nope"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(write.status(), StatusCode::FORBIDDEN);
    let body = read_body(write).await;
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains("read-only api token"),
        "message points at the scope: {}",
        body["message"]
    );
}

#[tokio::test]
async fn ops_token_denied_identity_and_governance_planes() {
    let app = test_app().await;
    let jwt = admin_jwt(Role::Admin, REGULAR_ADMIN_ID);

    let create = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/api-tokens")
                .header(AUTHORIZATION, bearer(&jwt))
                .header("Content-Type", "application/json")
                .body(Body::from(r#"{"name":"ci-script","scope":"ops"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    let token = read_body(create).await["data"]["token"]
        .as_str()
        .expect("plaintext token")
        .to_string();

    // Identity plane — even though the owner IS a platform admin.
    for (method, uri) in [
        ("GET", "/api/v1/users"),
        ("POST", "/api/v1/api-tokens"),
        ("POST", "/api/v1/auth/change-password"),
    ] {
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .header(AUTHORIZATION, bearer(&token))
                    .header("Content-Type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN, "{method} {uri}");
        let body = read_body(resp).await;
        assert!(
            body["message"]
                .as_str()
                .unwrap()
                .contains("identity endpoints"),
            "{method} {uri}: {}",
            body["message"]
        );
    }

    // Governance plane — platform settings and plugin curation.
    for uri in [
        "/api/v1/admin/settings",
        "/api/v1/admin/plugin-sources",
        "/api/v1/admin/plugin-pool",
    ] {
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(uri)
                    .header(AUTHORIZATION, bearer(&token))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN, "GET {uri}");
        let body = read_body(resp).await;
        assert!(
            body["message"].as_str().unwrap().contains("governance"),
            "GET {uri}: {}",
            body["message"]
        );
    }
}

#[tokio::test]
async fn legacy_full_token_keeps_operations_plane_but_never_identity_plane() {
    let (app, pool) = test_app_with_pool().await;

    // Seed a pre-scope token row directly — the API no longer creates them.
    let raw = shared::api_token::generate_token();
    sqlx::query(
        "INSERT INTO api_tokens (id, user_id, name, token_hash, token_prefix, scope)
         VALUES ('tok-legacy', 'admin-2', 'legacy', ?, 'buz_ab…', 'full')",
    )
    .bind(shared::api_token::hash_token(&raw))
    .execute(&pool)
    .await
    .expect("seed legacy token");

    // Operations plane: still fully authorized (creator-equivalent).
    let create_team = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/teams")
                .header(AUTHORIZATION, bearer(&raw))
                .header("Content-Type", "application/json")
                .body(Body::from(r#"{"name":"legacy-team","slug":"legacy-team"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(create_team.status(), StatusCode::OK);

    // Identity plane: self-perpetuation and user management are closed even
    // for the legacy band.
    for (method, uri) in [("POST", "/api/v1/api-tokens"), ("GET", "/api/v1/users")] {
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .header(AUTHORIZATION, bearer(&raw))
                    .header("Content-Type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN, "{method} {uri}");
    }
}

#[tokio::test]
async fn regular_admin_only_sees_own_tokens() {
    let app = test_app().await;

    let super_jwt = admin_jwt(Role::Admin, SUPER_ADMIN_ID);
    let create = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/api-tokens")
                .header(AUTHORIZATION, bearer(&super_jwt))
                .header("Content-Type", "application/json")
                .body(Body::from(r#"{"name":"boss-token"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(create.status(), StatusCode::OK);

    let ops_jwt = admin_jwt(Role::Admin, REGULAR_ADMIN_ID);
    let create2 = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/api-tokens")
                .header(AUTHORIZATION, bearer(&ops_jwt))
                .header("Content-Type", "application/json")
                .body(Body::from(r#"{"name":"ops-token"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(create2.status(), StatusCode::OK);

    // Regular admin listing sees only their own.
    let list = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/api-tokens")
                .header(AUTHORIZATION, bearer(&ops_jwt))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let list_body = read_body(list).await;
    let items = list_body["data"]["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "regular admin sees only own tokens");
    assert_eq!(items[0]["name"], "ops-token");

    // Super admin listing sees both.
    let super_list = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/api-tokens")
                .header(AUTHORIZATION, bearer(&super_jwt))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let super_body = read_body(super_list).await;
    let all = super_body["data"]["items"].as_array().unwrap();
    assert_eq!(all.len(), 2, "super admin sees all tokens");
}

#[tokio::test]
async fn regular_admin_cannot_revoke_other_admins_token() {
    let app = test_app().await;

    let super_jwt = admin_jwt(Role::Admin, SUPER_ADMIN_ID);
    let create = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/api-tokens")
                .header(AUTHORIZATION, bearer(&super_jwt))
                .header("Content-Type", "application/json")
                .body(Body::from(r#"{"name":"boss-token"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    let created = read_body(create).await;
    let id = created["data"]["id"].as_str().unwrap();

    let ops_jwt = admin_jwt(Role::Admin, REGULAR_ADMIN_ID);
    let revoke = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/v1/api-tokens/{id}"))
                .header(AUTHORIZATION, bearer(&ops_jwt))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(revoke.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn revoked_token_is_rejected() {
    let app = test_app().await;
    let jwt = admin_jwt(Role::Admin, REGULAR_ADMIN_ID);

    let create = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/api-tokens")
                .header(AUTHORIZATION, bearer(&jwt))
                .header("Content-Type", "application/json")
                .body(Body::from(r#"{"name":"ci-script"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    let created = read_body(create).await;
    let id = created["data"]["id"].as_str().unwrap();
    let token = created["data"]["token"].as_str().unwrap();

    let revoke = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/v1/api-tokens/{id}"))
                .header(AUTHORIZATION, bearer(&jwt))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(revoke.status(), StatusCode::OK);

    let me = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/auth/me")
                .header(AUTHORIZATION, bearer(token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(me.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn non_admin_cannot_create_api_token() {
    let app = test_app().await;
    let pleb_jwt = admin_jwt(Role::User, REGULAR_USER_ID);

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/api-tokens")
                .header(AUTHORIZATION, bearer(&pleb_jwt))
                .header("Content-Type", "application/json")
                .body(Body::from(r#"{"name":"x"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}
