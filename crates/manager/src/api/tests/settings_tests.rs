use crate::api;
use crate::config::{AuthConfig, Config};
use crate::db::run_migrations;
use crate::services::log_broadcast::LogBroadcaster;
use crate::services::step_resolver;
use crate::state::AppState;
use axum::Router;
use shared::auth::{Role, TokenManager};
use sqlx::{Pool, Sqlite};
use tower::ServiceExt;

const JWT_SECRET: &str = "test-secret-0123456789-abcdefghijklmnop";
const ADMIN_ID: &str = "admin-1";
const MEMBER_ID: &str = "member-1";

fn test_config() -> Config {
    Config {
        server: Default::default(),
        database: Default::default(),
        auth: AuthConfig {
            jwt_secret: JWT_SECRET.into(),
            jwt_ttl_hours: 24,
            runner_register_token_ttl_hours: 24,
        },
        runner: Default::default(),
        pipeline: Default::default(),
        standalone: Default::default(),
        step_index: Default::default(),
        clock: Default::default(),
        logging: Default::default(),
        marketplace_url: String::new(),
    }
}

async fn test_app() -> (Router, Pool<Sqlite>) {
    let pool = Pool::<Sqlite>::connect("sqlite::memory:")
        .await
        .expect("open in-memory db");
    run_migrations(&pool, JWT_SECRET).await.expect("migrate db");

    for (id, username, admin) in [(ADMIN_ID, "boss", 1i64), (MEMBER_ID, "dev", 0i64)] {
        sqlx::query(
            "INSERT INTO users (id, username, password_hash, display_name, is_platform_admin, is_super_admin, is_active)
             VALUES (?, ?, 'hash', ?, ?, 0, 1)",
        )
        .bind(id)
        .bind(username)
        .bind(username)
        .bind(admin)
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

fn jwt(user_id: &str, role: Role) -> String {
    TokenManager::new(JWT_SECRET)
        .create_token(user_id, role, 1)
        .expect("mint jwt")
}

async fn get_settings(router: &Router, token: &str) -> (axum::http::StatusCode, serde_json::Value) {
    let resp = router
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method("GET")
                .uri("/api/v1/admin/settings")
                .header("authorization", format!("Bearer {token}"))
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let body: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(resp.into_body(), 1 << 20)
            .await
            .unwrap(),
    )
    .unwrap_or(serde_json::Value::Null);
    (status, body)
}

async fn put_setting(
    router: &Router,
    token: &str,
    key: &str,
    value: &str,
) -> (axum::http::StatusCode, serde_json::Value) {
    let resp = router
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method("PUT")
                .uri(format!("/api/v1/admin/settings/{key}"))
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(axum::body::Body::from(
                    serde_json::json!({ "value": value }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let body: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(resp.into_body(), 1 << 20)
            .await
            .unwrap(),
    )
    .unwrap_or(serde_json::Value::Null);
    (status, body)
}

fn setting_value(body: &serde_json::Value, key: &str) -> String {
    body["data"]["items"]
        .as_array()
        .expect("items array")
        .iter()
        .find(|item| item["key"] == key)
        .expect("setting present")["value"]
        .as_str()
        .expect("string value")
        .to_string()
}

fn data_ok(body: &serde_json::Value) -> bool {
    body["code"] == 0 && body["data"].is_object()
}

#[tokio::test]
async fn settings_require_platform_admin() {
    let (router, _pool) = test_app().await;

    let (status, _) = get_settings(&router, &jwt(MEMBER_ID, Role::User)).await;
    assert_eq!(status, axum::http::StatusCode::FORBIDDEN);

    let (status, _) = put_setting(
        &router,
        &jwt(MEMBER_ID, Role::User),
        "marketplace_url",
        "https://x",
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn settings_defaults_come_from_config() {
    let (router, _pool) = test_app().await;
    let (status, body) = get_settings(&router, &jwt(ADMIN_ID, Role::Admin)).await;
    assert_eq!(status, axum::http::StatusCode::OK);
    assert!(data_ok(&body));
    assert_eq!(setting_value(&body, "step_review_required"), "false");
    // local_dir default resolves to an ABSOLUTE path under the scan root
    // (CWD-anchored during tests) — operators never see a bare relative path.
    let expected = std::env::current_dir()
        .unwrap()
        .join("pipelines/plugins")
        .to_string_lossy()
        .into_owned();
    assert_eq!(setting_value(&body, "step_index_local_dir"), expected);
    // marketplace default is the built-in URL when manager.toml leaves it empty
    assert_eq!(
        setting_value(&body, "marketplace_url"),
        crate::api::version::DEFAULT_MARKETPLACE_URL
    );
}

#[tokio::test]
async fn update_persists_applies_and_reset_falls_back() {
    let (router, pool) = test_app().await;
    let token = jwt(ADMIN_ID, Role::Admin);

    // Valid update persists and flips the review gate in the resolver.
    let (status, body) = put_setting(&router, &token, "step_review_required", "true").await;
    assert_eq!(status, axum::http::StatusCode::OK, "{body}");
    assert!(data_ok(&body));
    let policy = step_resolver::policy_from_settings();
    assert!(policy.require_reviewed, "override must reach the resolver");

    let row: (String,) =
        sqlx::query_as("SELECT value FROM platform_settings WHERE key = 'step_review_required'")
            .fetch_one(&pool)
            .await
            .expect("override row");
    assert_eq!(row.0, "true");

    // Invalid value is rejected with a validation error and nothing persisted.
    let (status, _) = put_setting(&router, &token, "step_review_required", "yes").await;
    assert_eq!(status, axum::http::StatusCode::UNPROCESSABLE_ENTITY);

    // Reset clears the override; the resolver falls back to config (false).
    let resp = router
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method("DELETE")
                .uri("/api/v1/admin/settings/step_review_required")
                .header("authorization", format!("Bearer {token}"))
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), axum::http::StatusCode::OK);
    let policy = step_resolver::policy_from_settings();
    assert!(!policy.require_reviewed, "reset must fall back to config");
}

#[tokio::test]
async fn unknown_setting_and_invalid_path_are_rejected() {
    let (router, _pool) = test_app().await;
    let token = jwt(ADMIN_ID, Role::Admin);

    let (status, _) = put_setting(&router, &token, "nope", "x").await;
    assert_eq!(status, axum::http::StatusCode::UNPROCESSABLE_ENTITY);

    let (status, _) = put_setting(&router, &token, "step_index_local_dir", "   ").await;
    assert_eq!(status, axum::http::StatusCode::UNPROCESSABLE_ENTITY);
}
