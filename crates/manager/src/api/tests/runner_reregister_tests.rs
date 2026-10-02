//! Re-registration auth: knowing the runner NAME must not be enough to obtain
//! a fresh JWT. Re-registration requires the previous runner JWT (Bearer,
//! expiry ignored) or an admin-issued register_token.
//! Included via `api/runners.rs` under `#[cfg(test)]`.

use axum::{
    body::Body,
    extract::ConnectInfo,
    http::{Request, StatusCode},
    Router,
};
use http_body_util::BodyExt;
use sqlx::sqlite::SqlitePoolOptions;
use std::net::SocketAddr;
use tower::ServiceExt;

use crate::api;
use crate::api::runners::create_runner_record;
use crate::config::{
    AuthConfig, Config, PipelineConfig, RunnerConfig, ServerConfig, StandaloneConfig,
};
use crate::db::run_migrations;
use crate::services::log_broadcast::LogBroadcaster;
use crate::state::AppState;
use shared::auth::{Role, TokenManager};
use shared::protocol::CreateRunnerRequest;

const JWT_SECRET: &str = "test-secret-0123456789-abcdefghijklmnop";
const REMOTE_PEER: &str = "10.0.0.5:9";

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
        clock: Default::default(),
        logging: Default::default(),
    }
}

/// App + pool + (runner_id, one-shot register_token) with a pre-created runner row.
async fn app_with_runner() -> (Router, sqlx::SqlitePool, String, String) {
    let pool = SqlitePoolOptions::new()
        .max_connections(2)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    run_migrations(&pool, JWT_SECRET).await.unwrap();
    let created = create_runner_record(
        &pool,
        &CreateRunnerRequest {
            name: "lab-runner-01".into(),
            labels: None,
            version: None,
            max_concurrent_jobs: None,
            clean_workspace_after_job: None,
            shell_enabled: None,
            ssh_enabled: None,
            docker_enabled: None,
            wasm_enabled: None,
            ssh_host_key_policy: None,
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

async fn register(
    app: &Router,
    name: &str,
    token: &str,
    bearer: Option<&str>,
) -> (StatusCode, serde_json::Value) {
    let body = serde_json::json!({
        "name": name,
        "register_token": token,
        "labels": [],
        "version": "test"
    });
    let mut builder = Request::builder()
        .method("POST")
        .uri("/api/v1/runners/register")
        .header("Content-Type", "application/json");
    if let Some(b) = bearer {
        builder = builder.header("Authorization", format!("Bearer {b}"));
    }
    let req = builder.body(Body::from(body.to_string())).unwrap();
    let req = {
        let (mut parts, body) = req.into_parts();
        parts
            .extensions
            .insert(ConnectInfo(REMOTE_PEER.parse::<SocketAddr>().unwrap()));
        Request::from_parts(parts, body)
    };
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or_default();
    (status, json)
}

/// Expired runner JWT for `runner_id` (refresh semantics: expiry ignored).
fn expired_runner_jwt(runner_id: &str) -> String {
    TokenManager::new(JWT_SECRET)
        .create_token(runner_id, Role::Runner, -1)
        .unwrap()
}

fn foreign_runner_jwt() -> String {
    TokenManager::new(JWT_SECRET)
        .create_token("00000000-0000-0000-0000-000000000009", Role::Runner, 24)
        .unwrap()
}

#[tokio::test]
async fn reregistration_with_name_only_is_rejected() {
    let (app, _pool, _id, register_token) = app_with_runner().await;
    // First registration consumes the one-shot token.
    let (status, _) = register(&app, "lab-runner-01", &register_token, None).await;
    assert_eq!(status, StatusCode::OK);

    // Knowing the name alone must NOT yield a fresh JWT.
    let (status, body) = register(&app, "lab-runner-01", "", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    let (status, _) = register(&app, "lab-runner-01", "wrong-token", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn reregistration_with_previous_runner_jwt_succeeds_even_when_expired() {
    let (app, _pool, runner_id, register_token) = app_with_runner().await;
    let (status, _) = register(&app, "lab-runner-01", &register_token, None).await;
    assert_eq!(status, StatusCode::OK);

    let jwt = expired_runner_jwt(&runner_id);
    let (status, body) = register(&app, "lab-runner-01", "", Some(&jwt)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["data"]["token"].as_str().unwrap().len() > 10);
}

#[tokio::test]
async fn reregistration_with_foreign_runner_jwt_is_rejected() {
    let (app, _pool, _runner_id, register_token) = app_with_runner().await;
    let (status, _) = register(&app, "lab-runner-01", &register_token, None).await;
    assert_eq!(status, StatusCode::OK);

    let (status, _) = register(&app, "lab-runner-01", "", Some(&foreign_runner_jwt())).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn reregistration_with_admin_reissued_register_token_succeeds() {
    let (app, pool, _runner_id, register_token) = app_with_runner().await;
    let (status, _) = register(&app, "lab-runner-01", &register_token, None).await;
    assert_eq!(status, StatusCode::OK);

    // Recovery path: admin re-issues a register_token on the existing row.
    sqlx::query("UPDATE runners SET register_token = 'admin-reset-token' WHERE name = ?")
        .bind("lab-runner-01")
        .execute(&pool)
        .await
        .unwrap();

    let (status, body) = register(&app, "lab-runner-01", "admin-reset-token", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}
