//! Standalone mode register: loopback tokenless for authorized names only.

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

use manager::api;
use manager::api::runners::create_runner_record;
use manager::api::runners::ensure_standalone_auto_approve;
use manager::config::AuthConfig;
use manager::config::Config;
use manager::config::PipelineConfig;
use manager::config::RunnerConfig;
use manager::config::ServerConfig;
use manager::config::StandaloneConfig;
use manager::db::run_migrations;
use manager::services::log_broadcast::LogBroadcaster;
use manager::state::AppState;
use shared::protocol::CreateRunnerRequest;

const JWT_SECRET: &str = "test-secret-0123456789-abcdefghijklmnop";

fn base_config(standalone: bool) -> Config {
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
            enabled: standalone,
            auto_approve: vec!["standalone-runner".into()],
        },
        plugin_index: Default::default(),
        storage: Default::default(),
        logging: Default::default(),
        marketplace_url: String::new(),
    }
}

async fn test_app(standalone: bool) -> Router {
    let pool = SqlitePoolOptions::new()
        .max_connections(4)
        .connect("sqlite::memory:")
        .await
        .expect("open in-memory db");
    run_migrations(&pool, JWT_SECRET).await.expect("migrate db");

    let config = base_config(standalone);
    ensure_standalone_auto_approve(&pool, &config)
        .await
        .expect("auto_approve");

    let state = AppState {
        pool,
        config,
        log_broadcast: LogBroadcaster::new(),
    };
    Router::new().nest("/api/v1", api::router(state))
}

fn with_peer(mut req: Request<Body>, addr: SocketAddr) -> Request<Body> {
    req.extensions_mut().insert(ConnectInfo(addr));
    req
}

async fn register(
    app: &Router,
    name: &str,
    token: &str,
    peer: Option<SocketAddr>,
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
        .header("Content-Type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    if let Some(addr) = peer {
        builder = with_peer(builder, addr);
    }
    let resp = app.clone().oneshot(builder).await.unwrap();
    let status = resp.status();
    let bytes = resp
        .into_body()
        .collect()
        .await
        .expect("read body")
        .to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or_default();
    (status, json)
}

#[tokio::test]
async fn production_mode_rejects_empty_token() {
    let pool = SqlitePoolOptions::new()
        .max_connections(2)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    run_migrations(&pool, JWT_SECRET).await.unwrap();
    create_runner_record(
        &pool,
        &CreateRunnerRequest {
            name: "prod-runner".into(),
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
        pool,
        config: base_config(false),
        log_broadcast: LogBroadcaster::new(),
    };
    let app = Router::new().nest("/api/v1", api::router(state));
    let loopback: SocketAddr = "127.0.0.1:9".parse().unwrap();
    let (status, _) = register(&app, "prod-runner", "", Some(loopback)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn standalone_loopback_tokenless_succeeds_when_authorized() {
    let app = test_app(true).await;
    let loopback: SocketAddr = "127.0.0.1:9".parse().unwrap();
    let (status, body) = register(&app, "standalone-runner", "", Some(loopback)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["data"]["token"].as_str().unwrap().len() > 10);
}

#[tokio::test]
async fn standalone_rejects_non_loopback_even_with_token() {
    let pool = SqlitePoolOptions::new()
        .max_connections(2)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    run_migrations(&pool, JWT_SECRET).await.unwrap();
    let config = base_config(true);
    ensure_standalone_auto_approve(&pool, &config)
        .await
        .unwrap();
    let token: String = sqlx::query_scalar("SELECT register_token FROM runners WHERE name = ?")
        .bind("standalone-runner")
        .fetch_one(&pool)
        .await
        .unwrap();
    let state = AppState {
        pool,
        config,
        log_broadcast: LogBroadcaster::new(),
    };
    let app = Router::new().nest("/api/v1", api::router(state));
    let remote: SocketAddr = "10.0.0.5:9".parse().unwrap();
    let (status, body) = register(&app, "standalone-runner", &token, Some(remote)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
}

#[tokio::test]
async fn standalone_loopback_rejects_unauthorized_name() {
    let app = test_app(true).await;
    let loopback: SocketAddr = "127.0.0.1:9".parse().unwrap();
    let (status, body) = register(&app, "unknown-runner", "", Some(loopback)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    let msg = body["message"].as_str().unwrap_or("");
    assert!(msg.contains("not authorized"), "{msg}");
}

#[tokio::test]
async fn standalone_without_peer_extension_treated_as_non_loopback() {
    let app = test_app(true).await;
    let (status, _) = register(&app, "standalone-runner", "", None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}
