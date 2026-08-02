//! End-to-end test for the unified log-masking entry point: the Manager's
//! dispatch must put every sensitive value (registry password, …) into the
//! step payload's `log_mask_values`, which the Runner consumes to redact logs.
//! Included via `api/mod.rs` under `#[cfg(test)]`.

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
use crate::crypto;
use crate::db::run_migrations;
use crate::services::log_broadcast::LogBroadcaster;
use crate::state::AppState;

const JWT_SECRET: &str = "test-secret-0123456789-abcdefghijklmnop";

const ADMIN_ID: &str = "admin-1";

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

    sqlx::query("INSERT INTO teams (id, name, slug) VALUES ('team-1', 'Default', 'default')")
        .execute(&pool)
        .await
        .expect("seed team");

    // Runner with matching label.
    sqlx::query(
        "INSERT INTO runners (id, name, register_token, labels, status, max_concurrent_jobs)
         VALUES ('runner-1', 'r1', 'tok', '[\"linux-x64\"]', 'online', 2)",
    )
    .execute(&pool)
    .await
    .expect("seed runner");

    // Pipeline with one shell step that references a registry secret.
    sqlx::query(
        "INSERT INTO pipelines (id, name, trigger_type, runner_labels, input_defs, is_active, team_id)
         VALUES ('p1', 'dispatch-mask', 'manual', '[\"linux-x64\"]', '[]', 1, 'team-1')",
    )
    .execute(&pool)
    .await
    .expect("seed pipeline");

    sqlx::query(
        "INSERT INTO tasks (id, pipeline_id, step_order, name, exec_type, command_template,
                            registry_secret)
         VALUES ('t1', 'p1', 0, 'echo', 'shell', 'echo hi', 'acr')",
    )
    .execute(&pool)
    .await
    .expect("seed task");

    // Registry secret (same team), value encrypted at rest like production.
    let registry_json = r#"{"registry":"registry.example.com","username":"u1","password":"p4ssw0rd-secret"}"#;
    let sealed = crypto::encrypt(registry_json, JWT_SECRET).expect("encrypt registry secret");
    sqlx::query(
        "INSERT INTO secrets (id, name, value, team_id, kind) VALUES ('s1', 'acr', ?, 'team-1', 'registry')",
    )
    .bind(sealed)
    .execute(&pool)
    .await
    .expect("seed secret");

    let state = AppState {
        pool,
        config: test_config(),
        log_broadcast: LogBroadcaster::new(),
    };
    Router::new().nest("/api/v1", api::router(state))
}

fn bearer(token: &str) -> String {
    format!("Bearer {token}")
}

fn admin_jwt() -> String {
    TokenManager::new(JWT_SECRET)
        .create_token(ADMIN_ID, Role::Admin, 1)
        .expect("mint admin jwt")
}

fn runner_jwt() -> String {
    TokenManager::new(JWT_SECRET)
        .create_token("runner-1", Role::Runner, 1)
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

#[tokio::test]
async fn dispatch_puts_registry_password_in_log_mask_values() {
    let app = test_app().await;

    // Trigger the pipeline → creates a pending job.
    let trigger = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/pipelines/p1/trigger")
                .header(AUTHORIZATION, bearer(&admin_jwt()))
                .header("Content-Type", "application/json")
                .body(Body::from(r#"{"inputs":{}}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(trigger.status(), StatusCode::OK, "trigger succeeds");

    // Runner claims the pending job → dispatch payload.
    let poll = app
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
    assert_eq!(poll.status(), StatusCode::OK, "poll succeeds");
    let body = read_body(poll).await;

    let tasks = body["data"]["tasks"].as_array().expect("tasks array");
    assert_eq!(tasks.len(), 1);

    let mask_values = tasks[0]["log_mask_values"].as_array().expect("log_mask_values");
    let values: Vec<&str> = mask_values
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert!(
        values.contains(&"p4ssw0rd-secret"),
        "registry password registered for log masking: {values:?}"
    );
    assert!(
        values.iter().all(|v| *v != "u1"),
        "username is not registered as sensitive: {values:?}"
    );
}
