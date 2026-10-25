//! Step library listing API (GET /api/v1/steps) — reads the local step
//! directory; unparseable files are reported, not fatal.

use axum::{
    body::Body,
    http::{header::AUTHORIZATION, Request, StatusCode},
    Router,
};
use http_body_util::BodyExt;
use shared::auth::{Role, TokenManager};
use sqlx::sqlite::SqlitePoolOptions;
use tower::ServiceExt;

use crate::api;
use crate::config::{AuthConfig, Config, PipelineConfig, RunnerConfig, ServerConfig};
use crate::db::run_migrations;
use crate::services::log_broadcast::LogBroadcaster;
use crate::state::AppState;

const JWT_SECRET: &str = "test-secret-0123456789-abcdefghijklmnop";
const ADMIN_ID: &str = "admin-1";

async fn test_app_with_dir(dir: &std::path::Path) -> (Router, String) {
    let pool = SqlitePoolOptions::new()
        .max_connections(2)
        .connect("sqlite::memory:")
        .await
        .expect("open in-memory db");
    run_migrations(&pool, JWT_SECRET).await.expect("migrate db");

    sqlx::query(
        "INSERT INTO users (id, username, password_hash, display_name, is_platform_admin, is_super_admin, is_active)
         VALUES (?, ?, 'hash', ?, 1, 1, 1)",
    )
    .bind(ADMIN_ID)
    .bind("boss")
    .bind("boss")
    .execute(&pool)
    .await
    .expect("seed user");

    let mut config = Config {
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
        step_index: Default::default(),
        storage: Default::default(),
        clock: Default::default(),
        logging: Default::default(),
        marketplace_url: String::new(),
    };
    config.step_index.local_dir = dir.to_string_lossy().into_owned();

    let state = AppState {
        pool,
        config,
        log_broadcast: LogBroadcaster::new(),
    };
    (
        Router::new().nest("/api/v1", api::router(state)),
        dir.to_string_lossy().into_owned(),
    )
}

async fn get_json(app: &Router, jwt: Option<&str>) -> (StatusCode, serde_json::Value) {
    let mut builder = Request::builder().method("GET").uri("/api/v1/steps");
    if let Some(jwt) = jwt {
        builder = builder.header(AUTHORIZATION, format!("Bearer {jwt}"));
    }
    let resp = app
        .clone()
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let json = serde_json::from_slice(&body)
        .unwrap_or_else(|e| panic!("json body ({}): {e}", String::from_utf8_lossy(&body)));
    (status, json)
}

fn admin_jwt() -> String {
    TokenManager::new(JWT_SECRET)
        .create_token(ADMIN_ID, Role::Admin, 1)
        .unwrap()
}

#[tokio::test]
async fn lists_templates_with_attribution_and_reports_unparseable() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("notify-feishu.yaml"),
        r#"
id: buz/feishu
name: Feishu notify
version: 1.0.0
publisher: official
author: "buz maintainers"
source_repo: https://github.com/schiplat/buz-plugins
keywords: [feishu, notify]
category: notify
engine: shell
default_timeout: 30s
inputs:
  webhook:
    type: string
    required: true
    masking: true
    pattern: '^https://[^ ]+$'
    description: Webhook endpoint
    options: ['https://one.example/hook', 'https://two.example/hook']
run: |
  curl -fsSL -X POST "$INPUT_WEBHOOK" -d '{}'
"#,
    )
    .unwrap();
    std::fs::write(dir.path().join("broken.yaml"), "id: [broken").unwrap();
    std::fs::create_dir_all(dir.path().join("community/nested")).unwrap();
    std::fs::write(
        dir.path().join("community/nested/custom.yaml"),
        r#"
id: alice/custom
name: Custom step
version: 1.0.0
author: Alice
source_repo: https://example.com/alice/plugins
run: echo custom
"#,
    )
    .unwrap();
    std::fs::write(dir.path().join(".hidden.yaml"), "id: hidden").unwrap();
    std::fs::create_dir_all(dir.path().join(".hidden-dir")).unwrap();
    std::fs::write(dir.path().join(".hidden-dir/hidden.yaml"), "id: hidden").unwrap();

    let (app, _) = test_app_with_dir(dir.path()).await;
    let (status, body) = get_json(&app, Some(&admin_jwt())).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let items = body["data"]["items"].as_array().unwrap();
    assert_eq!(items.len(), 2, "{body}");
    assert_eq!(items[0]["id"], "alice/custom");
    assert_eq!(items[0]["file"], "community/nested/custom.yaml");
    assert_eq!(items[1]["id"], "buz/feishu");
    assert_eq!(items[1]["file"], "notify-feishu.yaml");
    assert_eq!(items[1]["version"], "1.0.0");
    assert_eq!(items[1]["publisher"], "official");
    assert_eq!(items[1]["category"], "notify");
    assert_eq!(items[1]["buz_compatible"], true);
    assert_eq!(items[1]["content_sha256"].as_str().unwrap().len(), 64);
    assert_eq!(items[1]["reviewed"], false);
    assert_eq!(items[1]["valid"], true);
    assert_eq!(items[1]["inputs"].as_array().unwrap().len(), 1);
    assert_eq!(items[1]["inputs"][0]["name"], "webhook");
    assert_eq!(items[1]["inputs"][0]["masking"], true);
    assert_eq!(items[1]["inputs"][0]["pattern"], "^https://[^ ]+$");
    assert_eq!(
        items[1]["inputs"][0]["options"].as_array().unwrap().len(),
        2
    );

    let unparseable = body["data"]["unparseable"].as_array().unwrap();
    assert_eq!(unparseable.len(), 1);
    assert_eq!(unparseable[0]["file"], "broken.yaml");
}

#[tokio::test]
async fn missing_directory_yields_empty_library() {
    let dir = tempfile::tempdir().unwrap();
    let empty = dir.path().join("nope");
    let (app, _) = test_app_with_dir(&empty).await;
    let (status, body) = get_json(&app, Some(&admin_jwt())).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["items"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn requires_authentication() {
    let dir = tempfile::tempdir().unwrap();
    let (app, _) = test_app_with_dir(dir.path()).await;
    let (status, _) = get_json(&app, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}
