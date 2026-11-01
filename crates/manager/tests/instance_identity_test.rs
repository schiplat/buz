//! Instance-identity integration: `/api/v1/meta`, persistent
//! `manager_instance_id`, and runner-reported `host_id`/`runner_instance_id`
//! persisted on register/heartbeat (malformed values dropped, never fatal).

use axum::{
    body::Body,
    http::{Request, StatusCode},
    Router,
};
use http_body_util::BodyExt;
use sqlx::sqlite::SqlitePoolOptions;
use std::sync::Once;
use tower::ServiceExt;

use manager::api;
use manager::api::runners::create_runner_record;
use manager::config::{
    AuthConfig, Config, PipelineConfig, RunnerConfig, ServerConfig, StandaloneConfig,
};
use manager::db::run_migrations;
use manager::services::log_broadcast::LogBroadcaster;
use manager::state::AppState;
use shared::auth::TokenManager;
use shared::protocol::CreateRunnerRequest;

const JWT_SECRET: &str = "test-secret-0123456789-abcdefghijklmnop";

static SET_INSTANCE: Once = Once::new();

fn production_config() -> Config {
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
        standalone: StandaloneConfig {
            enabled: false,
            auto_approve: vec![],
        },
        plugin_index: Default::default(),
        storage: Default::default(),
        clock: Default::default(),
        logging: Default::default(),
        marketplace_url: String::new(),
    }
}

async fn test_pool() -> sqlx::SqlitePool {
    let pool = SqlitePoolOptions::new()
        .max_connections(2)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    run_migrations(&pool, JWT_SECRET).await.unwrap();
    pool
}

/// App + pool + (runner_id, one-shot register_token).
async fn app_with_runner() -> (Router, sqlx::SqlitePool, String, String) {
    let pool = test_pool().await;
    sqlx::query(
        "INSERT INTO users (id, username, password_hash, display_name, is_platform_admin, is_super_admin, is_active)
         VALUES ('admin-1', 'admin', 'hash', 'admin', 1, 1, 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    let created = create_runner_record(
        &pool,
        &CreateRunnerRequest {
            name: "id-runner-01".into(),
            labels: None,
            team_id: None,
            execution_mode: None,
            version: None,
            max_concurrent_jobs: None,
            clean_workspace_after_job: None,
            shell_enabled: None,
            ssh_enabled: None,
            docker_enabled: None,
            wasm_enabled: None,
            ssh_host_key_policy: None,
            claim_strategy: None,
        },
    )
    .await
    .unwrap();
    let state = AppState {
        pool: pool.clone(),
        config: production_config(),
        log_broadcast: LogBroadcaster::new(),
    };
    (
        Router::new().nest("/api/v1", api::router(state)),
        pool,
        created.id,
        created.register_token,
    )
}

async fn post_json(
    app: &Router,
    uri: &str,
    bearer: Option<&str>,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let mut builder = Request::builder()
        .method("POST")
        .uri(uri)
        .header("Content-Type", "application/json");
    if let Some(b) = bearer {
        builder = builder.header("Authorization", format!("Bearer {b}"));
    }
    let req = builder.body(Body::from(body.to_string())).unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json = serde_json::from_slice(&bytes).unwrap_or_default();
    (status, json)
}

async fn get_json(app: &Router, uri: &str) -> serde_json::Value {
    let req = Request::builder()
        .method("GET")
        .uri(uri)
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap_or_default()
}

fn data(json: &serde_json::Value) -> serde_json::Value {
    json.get("data").cloned().unwrap_or_default()
}

/// Set the process-global manager identity once for this test binary.
fn pin_instance_id() -> &'static str {
    SET_INSTANCE.call_once(|| {
        api::version::set_instance_id("mgr-test-fixed".into());
    });
    api::version::instance_id().expect("instance id set")
}

#[tokio::test]
async fn meta_reports_the_manager_instance_id() {
    pin_instance_id();
    let (app, _pool, _runner_id, _token) = app_with_runner().await;
    let json = get_json(&app, "/api/v1/meta").await;
    assert_eq!(json["manager_instance_id"], pin_instance_id());
    assert_eq!(json["component"], "manager");
}

#[tokio::test]
async fn ensure_manager_instance_id_persists_and_is_stable() {
    let pool = test_pool().await;
    std::env::remove_var("BUZ_MANAGER_INSTANCE_ID");
    let first = manager::db::ensure_manager_instance_id(&pool)
        .await
        .unwrap();
    let second = manager::db::ensure_manager_instance_id(&pool)
        .await
        .unwrap();
    assert_eq!(first, second, "identity must survive restarts");
    assert!(first.starts_with("mgr-"));
    // Env pin overrides and is re-persisted.
    std::env::set_var("BUZ_MANAGER_INSTANCE_ID", "pinned-mgr-9");
    let pinned = manager::db::ensure_manager_instance_id(&pool)
        .await
        .unwrap();
    assert_eq!(pinned, "pinned-mgr-9");
    let stored: String =
        sqlx::query_scalar("SELECT value FROM manager_meta WHERE key = 'manager_instance_id'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(stored, "pinned-mgr-9");
    std::env::remove_var("BUZ_MANAGER_INSTANCE_ID");
}

#[tokio::test]
async fn register_and_heartbeat_persist_instance_ids() {
    let (app, pool, _runner_id, register_token) = app_with_runner().await;

    let (status, resp) = post_json(
        &app,
        "/api/v1/runners/register",
        None,
        serde_json::json!({
            "name": "id-runner-01",
            "register_token": register_token,
            "version": "test",
            "host_id": "host-a-01",
            "runner_instance_id": "rn-abc-01"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "register failed: {resp}");
    let runner_jwt = data(&resp)["token"].as_str().unwrap().to_string();

    let (host_id, instance_id): (String, String) = sqlx::query_as(
        "SELECT host_id, runner_instance_id FROM runners WHERE name = 'id-runner-01'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(host_id, "host-a-01");
    assert_eq!(instance_id, "rn-abc-01");

    // Heartbeat with new values overwrites.
    let (status, resp) = post_json(
        &app,
        "/api/v1/runners/heartbeat",
        Some(&runner_jwt),
        serde_json::json!({
            "status": "online",
            "host_id": "host-a-02",
            "runner_instance_id": "rn-abc-01"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "heartbeat failed: {resp}");
    let host_id: String =
        sqlx::query_scalar("SELECT host_id FROM runners WHERE name = 'id-runner-01'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(host_id, "host-a-02");
}

#[tokio::test]
async fn heartbeat_without_ids_keeps_stored_values() {
    let (app, pool, _runner_id, register_token) = app_with_runner().await;
    let (status, resp) = post_json(
        &app,
        "/api/v1/runners/register",
        None,
        serde_json::json!({
            "name": "id-runner-01",
            "register_token": register_token,
            "version": "test",
            "host_id": "host-keep",
            "runner_instance_id": "rn-keep"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "register failed: {resp}");
    let runner_jwt = data(&resp)["token"].as_str().unwrap().to_string();

    // Old-runner-shaped heartbeat: no identity fields at all.
    let (status, resp) = post_json(
        &app,
        "/api/v1/runners/heartbeat",
        Some(&runner_jwt),
        serde_json::json!({ "status": "online" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "heartbeat failed: {resp}");
    let (host_id, instance_id): (String, String) = sqlx::query_as(
        "SELECT host_id, runner_instance_id FROM runners WHERE name = 'id-runner-01'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(host_id, "host-keep");
    assert_eq!(instance_id, "rn-keep");
}

#[tokio::test]
async fn malformed_ids_are_dropped_without_failing_register() {
    let (app, pool, _runner_id, register_token) = app_with_runner().await;
    let (status, resp) = post_json(
        &app,
        "/api/v1/runners/register",
        None,
        serde_json::json!({
            "name": "id-runner-01",
            "register_token": register_token,
            "version": "test",
            "host_id": "bad host id!",
            "runner_instance_id": "ok-rn-1"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "register must succeed: {resp}");
    let (host_id, instance_id): (String, String) = sqlx::query_as(
        "SELECT host_id, runner_instance_id FROM runners WHERE name = 'id-runner-01'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(host_id, "", "invalid host id must not be stored");
    assert_eq!(instance_id, "ok-rn-1");
}

#[tokio::test]
async fn runner_dto_carries_the_instance_ids() {
    pin_instance_id();
    let (app, _pool, _runner_id, register_token) = app_with_runner().await;
    let (status, resp) = post_json(
        &app,
        "/api/v1/runners/register",
        None,
        serde_json::json!({
            "name": "id-runner-01",
            "register_token": register_token,
            "version": "test",
            "host_id": "host-dto",
            "runner_instance_id": "rn-dto"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "register failed: {resp}");
    let runner_jwt = data(&resp)["token"].as_str().unwrap().to_string();
    let runner_id = data(&resp)["runner_id"].as_str().unwrap().to_string();

    let admin = TokenManager::new(JWT_SECRET)
        .create_token("admin-1", shared::auth::Role::Admin, 24)
        .unwrap();
    let req = Request::builder()
        .method("GET")
        .uri(format!("/api/v1/runners/{runner_id}"))
        .header("Authorization", format!("Bearer {admin}"))
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(json["data"]["host_id"], "host-dto");
    assert_eq!(json["data"]["runner_instance_id"], "rn-dto");

    // And the runner's own heartbeat path stays authenticated as itself.
    let (status, resp) = post_json(
        &app,
        "/api/v1/runners/heartbeat",
        Some(&runner_jwt),
        serde_json::json!({ "status": "online" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "heartbeat failed: {resp}");
}
