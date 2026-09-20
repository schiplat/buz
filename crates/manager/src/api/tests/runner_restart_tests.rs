//! Integration tests for the remote Runner restart flow:
//!   POST /runners/{id}/restart  → sets restart_requested (admin only, no running jobs)
//!   POST /runners/heartbeat     → serves the flag once, then clears it
//!   GET  /runners/poll         → blocks dispatch while restart_requested
//! Included via `api/mod.rs` under `#[cfg(test)]`.

use axum::{
    body::Body,
    http::{header::AUTHORIZATION, Request, StatusCode},
    Router,
};
use http_body_util::BodyExt;
use shared::auth::{Role, TokenManager};
use sqlx::sqlite::{SqlitePool, SqlitePoolOptions};
use tower::ServiceExt;

use crate::api;
use crate::config::{AuthConfig, Config, PipelineConfig, RunnerConfig, ServerConfig};
use crate::db::run_migrations;
use crate::services::log_broadcast::LogBroadcaster;
use crate::state::AppState;

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
        clock: Default::default(),
        logging: Default::default(),
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

    // Runner with matching label.
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

async fn request_restart(app: &Router, token: &str) -> (StatusCode, serde_json::Value) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/v1/runners/{RUNNER_ID}/restart"))
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

async fn poll(app: &Router) -> serde_json::Value {
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
    read_body(resp).await
}

#[tokio::test]
async fn restart_requires_platform_admin() {
    let (app, _pool) = test_app().await;

    let (status, _) = request_restart(&app, &member_jwt()).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "non-admin restart is rejected"
    );

    // Admin path is accepted (flag is set).
    let (status, body) = request_restart(&app, &admin_jwt()).await;
    assert_eq!(status, StatusCode::OK, "admin restart succeeds");
    assert_eq!(body["data"]["restart_requested"], true);
}

#[tokio::test]
async fn heartbeat_serves_restart_once_then_clears() {
    let (app, pool) = test_app().await;

    // No pending restart → heartbeat reports restart=false.
    let body = heartbeat(&app).await;
    assert_eq!(body["data"]["restart"], false);

    // Request restart → flag set in DB.
    let (status, _) = request_restart(&app, &admin_jwt()).await;
    assert_eq!(status, StatusCode::OK);
    let flag: i64 = sqlx::query_scalar("SELECT restart_requested FROM runners WHERE id = ?")
        .bind(RUNNER_ID)
        .fetch_one(&pool)
        .await
        .expect("read flag");
    assert_eq!(flag, 1, "flag persisted after restart request");

    // First heartbeat serves the command and clears it.
    let body = heartbeat(&app).await;
    assert_eq!(
        body["data"]["restart"], true,
        "first heartbeat serves restart"
    );
    let flag: i64 = sqlx::query_scalar("SELECT restart_requested FROM runners WHERE id = ?")
        .bind(RUNNER_ID)
        .fetch_one(&pool)
        .await
        .expect("read flag");
    assert_eq!(flag, 0, "flag cleared after being served");

    // Second heartbeat → no longer restarting.
    let body = heartbeat(&app).await;
    assert_eq!(
        body["data"]["restart"], false,
        "second heartbeat has no restart"
    );
}

#[tokio::test]
async fn restart_refused_while_running_jobs() {
    let (app, pool) = test_app().await;

    // Seed a running job on this runner.
    sqlx::query(
        "INSERT INTO pipelines (id, name, trigger_type, runner_labels, input_defs, is_active)
         VALUES ('p1', 'restart-block', 'manual', '[\"linux-x64\"]', '[]', 1)",
    )
    .execute(&pool)
    .await
    .expect("seed pipeline");

    sqlx::query(
        "INSERT INTO jobs (id, pipeline_id, runner_id, status)
         VALUES ('job-1', 'p1', ?, 'running')",
    )
    .bind(RUNNER_ID)
    .execute(&pool)
    .await
    .expect("seed running job");

    let (status, body) = request_restart(&app, &admin_jwt()).await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "restart blocked while running"
    );
    assert!(
        body["message"].as_str().unwrap_or("").contains("running"),
        "conflict message names running jobs: {body}"
    );

    // No flag was set.
    let flag: i64 = sqlx::query_scalar("SELECT restart_requested FROM runners WHERE id = ?")
        .bind(RUNNER_ID)
        .fetch_one(&pool)
        .await
        .expect("read flag");
    assert_eq!(flag, 0, "flag must not be set when restart is refused");
}

#[tokio::test]
async fn poll_is_blocked_while_restart_pending() {
    let (app, _pool) = test_app().await;

    // Request restart (no running jobs → accepted).
    let (status, _) = request_restart(&app, &admin_jwt()).await;
    assert_eq!(status, StatusCode::OK);

    // Poll must return no task while the flag is set.
    let body = poll(&app).await;
    assert_eq!(
        body["data"],
        serde_json::Value::Null,
        "dispatch blocked while restart pending: {body}"
    );
}
