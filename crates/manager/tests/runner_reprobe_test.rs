//! Integration tests for the on-demand capability re-probe flow:
//!   POST /runners/{id}/reprobe-capabilities → sets capability_reprobe_requested (admin only)
//!   POST /runners/heartbeat                 → serves the flag once, then clears it
//!
//! Unlike restart, the flag does not block dispatch — probing is observational.

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

const ADMIN_ID: &str = "admin-1";
const MEMBER_ID: &str = "member-1";
const RUNNER_ID: &str = "runner-1";

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
        clock: Default::default(),
        logging: Default::default(),
        marketplace_url: String::new(),
    }
}

async fn test_app() -> (Router, SqlitePool) {
    let pool = SqlitePoolOptions::new()
        .max_connections(4)
        .connect("sqlite::memory:")
        .await
        .expect("open in-memory db");
    run_migrations(&pool, JWT_SECRET).await.expect("migrate db");

    // Platform admin.
    sqlx::query(
        "INSERT INTO users (id, username, password_hash, display_name, is_platform_admin, is_super_admin, is_active)
         VALUES (?, ?, 'hash', ?, 1, 1, 1)",
    )
    .bind(ADMIN_ID)
    .bind("boss")
    .bind("boss")
    .execute(&pool)
    .await
    .expect("seed admin");

    // Regular platform user (not admin).
    sqlx::query(
        "INSERT INTO users (id, username, password_hash, display_name, is_platform_admin, is_super_admin, is_active)
         VALUES (?, ?, 'hash', ?, 0, 0, 1)",
    )
    .bind(MEMBER_ID)
    .bind("dev")
    .bind("dev")
    .execute(&pool)
    .await
    .expect("seed member");

    sqlx::query(
        "INSERT INTO runners (id, name, register_token, labels, status, max_concurrent_jobs)
         VALUES (?, 'r1', 'tok', '[\"linux-x64\"]', 'online', 2)",
    )
    .bind(RUNNER_ID)
    .execute(&pool)
    .await
    .expect("seed runner");

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

fn admin_jwt() -> String {
    TokenManager::new(JWT_SECRET)
        .create_token(ADMIN_ID, Role::Admin, 1)
        .expect("mint admin jwt")
}

fn member_jwt() -> String {
    TokenManager::new(JWT_SECRET)
        .create_token(MEMBER_ID, Role::User, 0)
        .expect("mint member jwt")
}

fn runner_jwt() -> String {
    TokenManager::new(JWT_SECRET)
        .create_token(RUNNER_ID, Role::Runner, 1)
        .expect("mint runner jwt")
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

async fn request_reprobe(app: &Router, token: &str) -> (StatusCode, serde_json::Value) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/v1/runners/{RUNNER_ID}/reprobe-capabilities"))
                .header(AUTHORIZATION, bearer(token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    (status, read_body(resp).await)
}

async fn heartbeat(app: &Router) -> serde_json::Value {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/runners/heartbeat")
                .header(AUTHORIZATION, bearer(&runner_jwt()))
                .header("Content-Type", "application/json")
                .body(Body::from(r#"{"status":"online"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "heartbeat succeeds");
    read_body(resp).await
}

#[tokio::test]
async fn reprobe_requires_platform_admin() {
    let (app, _pool) = test_app().await;

    let (status, _) = request_reprobe(&app, &member_jwt()).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "non-admin re-probe is rejected"
    );

    // Admin path is accepted (flag is set).
    let (status, body) = request_reprobe(&app, &admin_jwt()).await;
    assert_eq!(status, StatusCode::OK, "admin re-probe succeeds");
    assert_eq!(body["data"]["reprobe_requested"], true);
}

#[tokio::test]
async fn heartbeat_serves_reprobe_once_then_clears() {
    let (app, pool) = test_app().await;

    // No pending re-probe → heartbeat reports reprobe=false and restart=false.
    let body = heartbeat(&app).await;
    assert_eq!(body["data"]["reprobe"], false);
    assert_eq!(
        body["data"]["restart"], false,
        "re-probe must not disturb restart"
    );

    // Request re-probe → flag set in DB.
    let (status, _) = request_reprobe(&app, &admin_jwt()).await;
    assert_eq!(status, StatusCode::OK);
    let flag: i64 =
        sqlx::query_scalar("SELECT capability_reprobe_requested FROM runners WHERE id = ?")
            .bind(RUNNER_ID)
            .fetch_one(&pool)
            .await
            .expect("read flag");
    assert_eq!(flag, 1, "flag persisted after re-probe request");

    // First heartbeat serves the command and clears it.
    let body = heartbeat(&app).await;
    assert_eq!(
        body["data"]["reprobe"], true,
        "first heartbeat serves re-probe"
    );
    let flag: i64 =
        sqlx::query_scalar("SELECT capability_reprobe_requested FROM runners WHERE id = ?")
            .bind(RUNNER_ID)
            .fetch_one(&pool)
            .await
            .expect("read flag");
    assert_eq!(flag, 0, "flag cleared after being served");

    // Second heartbeat → steady state again.
    let body = heartbeat(&app).await;
    assert_eq!(
        body["data"]["reprobe"], false,
        "second heartbeat has no re-probe"
    );
}

#[tokio::test]
async fn reprobe_does_not_block_dispatch() {
    let (app, _pool) = test_app().await;

    // Request re-probe (accepted even with no jobs — observational command).
    let (status, _) = request_reprobe(&app, &admin_jwt()).await;
    assert_eq!(status, StatusCode::OK);

    // Poll must still work normally while the re-probe flag is pending.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v1/runners/poll")
                .header(AUTHORIZATION, bearer(&runner_jwt()))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "poll succeeds");
    let body = read_body(resp).await;
    assert!(
        body["data"]["job_id"].is_null(),
        "no job expected (none seeded), but poll must not be blocked: {body}"
    );
}
