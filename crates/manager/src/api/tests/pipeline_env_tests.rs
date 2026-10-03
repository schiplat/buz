//! Integration tests for pipeline-level inline env vars (`env:` block):
//! create persists them, get_one returns them, and update does not drop them.
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
use crate::db::run_migrations;
use crate::services::log_broadcast::LogBroadcaster;
use crate::state::AppState;

const JWT_SECRET: &str = "test-secret-0123456789-abcdefghijklmnop";

const SUPER_ADMIN_ID: &str = "admin-1";

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
        step_index: Default::default(),
        clock: Default::default(),
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

    // Seed a super admin + a default team so pipeline create passes team checks.
    sqlx::query(
        "INSERT INTO users (id, username, password_hash, display_name, is_platform_admin, is_super_admin, is_active)
         VALUES (?, ?, 'hash', ?, 1, 1, 1)",
    )
    .bind(SUPER_ADMIN_ID)
    .bind("boss")
    .bind("boss")
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

fn bearer(token: &str) -> String {
    format!("Bearer {token}")
}

fn admin_jwt() -> String {
    TokenManager::new(JWT_SECRET)
        .create_token(SUPER_ADMIN_ID, Role::Admin, 1)
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

fn create_payload(env: &str) -> String {
    format!(
        r#"{{
        "name": "env-roundtrip",
        "trigger_type": "manual",
        "runner_labels": ["linux-x64"],
        "team_id": "team-1",
        "env": {env},
        "steps": [
            {{ "name": "echo", "run": "echo hi", "engine": "shell" }}
        ]
    }}"#
    )
}

#[tokio::test]
async fn create_persists_env_and_get_returns_it() {
    let app = test_app().await;
    let jwt = admin_jwt();

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/pipelines")
                .header(AUTHORIZATION, bearer(&jwt))
                .header("Content-Type", "application/json")
                .body(Body::from(create_payload(
                    r#"[{"name":"DEPLOY_DIR","value":"/opt/manager"},{"name":"REGISTRY","value":"registry.example.com"}]"#,
                )))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "create should succeed");
    let created = read_body(resp).await;
    let id = created["data"]["id"].as_str().expect("pipeline id");

    let got = app
        .oneshot(
            Request::builder()
                .uri(format!("/api/v1/pipelines/{id}"))
                .header(AUTHORIZATION, bearer(&jwt))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(got.status(), StatusCode::OK, "get should succeed");
    let body = read_body(got).await;
    let env = body["data"]["env"].as_array().expect("env array");
    assert_eq!(env.len(), 2, "get returns the saved env vars");
    assert_eq!(env[0]["name"], "DEPLOY_DIR");
    assert_eq!(env[0]["value"], "/opt/manager");
    assert_eq!(env[1]["name"], "REGISTRY");
    assert_eq!(env[1]["value"], "registry.example.com");
}

#[tokio::test]
async fn update_replaces_env_without_losing_it() {
    let app = test_app().await;
    let jwt = admin_jwt();

    // Create with two env vars.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/pipelines")
                .header(AUTHORIZATION, bearer(&jwt))
                .header("Content-Type", "application/json")
                .body(Body::from(create_payload(
                    r#"[{"name":"DEPLOY_DIR","value":"/opt/manager"},{"name":"REGISTRY","value":"registry.example.com"}]"#,
                )))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "create should succeed");
    let created = read_body(resp).await;
    let id = created["data"]["id"].as_str().expect("pipeline id");

    // Update with a different env set (the regression: update used to drop env).
    let update = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/api/v1/pipelines/{id}"))
                .header(AUTHORIZATION, bearer(&jwt))
                .header("Content-Type", "application/json")
                .body(Body::from(create_payload(
                    r#"[{"name":"DEPLOY_DIR","value":"/srv/manager"}]"#,
                )))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(update.status(), StatusCode::OK, "update should succeed");

    // env must reflect the updated values.
    let got = app
        .oneshot(
            Request::builder()
                .uri(format!("/api/v1/pipelines/{id}"))
                .header(AUTHORIZATION, bearer(&jwt))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = read_body(got).await;
    let env = body["data"]["env"].as_array().expect("env array");
    assert_eq!(env.len(), 1, "env is fully replaced on update");
    assert_eq!(env[0]["name"], "DEPLOY_DIR");
    assert_eq!(env[0]["value"], "/srv/manager");
}

#[tokio::test]
async fn update_with_empty_env_clears_it() {
    let app = test_app().await;
    let jwt = admin_jwt();

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/pipelines")
                .header(AUTHORIZATION, bearer(&jwt))
                .header("Content-Type", "application/json")
                .body(Body::from(create_payload(
                    r#"[{"name":"DEPLOY_DIR","value":"/opt/manager"}]"#,
                )))
                .unwrap(),
        )
        .await
        .unwrap();
    let created = read_body(resp).await;
    let id = created["data"]["id"].as_str().expect("pipeline id");

    // Update with an empty env array — env should be cleared.
    let update = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/api/v1/pipelines/{id}"))
                .header(AUTHORIZATION, bearer(&jwt))
                .header("Content-Type", "application/json")
                .body(Body::from(create_payload("[]")))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(update.status(), StatusCode::OK, "update should succeed");

    let got = app
        .oneshot(
            Request::builder()
                .uri(format!("/api/v1/pipelines/{id}"))
                .header(AUTHORIZATION, bearer(&jwt))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = read_body(got).await;
    let env = body["data"]["env"].as_array().expect("env array");
    assert!(env.is_empty(), "empty env array clears the env vars");
}
