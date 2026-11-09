//! Re-registration auth: knowing the runner NAME must not be enough to obtain
//! a fresh JWT. Re-registration requires the previous runner JWT (Bearer,
//! expiry ignored) or an admin-issued register_token.

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
use manager::config::AuthConfig;
use manager::config::Config;
use manager::config::PipelineConfig;
use manager::config::RunnerConfig;
use manager::config::ServerConfig;
use manager::config::StandaloneConfig;
use manager::db::run_migrations;
use manager::services::log_broadcast::LogBroadcaster;
use manager::state::AppState;
use shared::auth::{Role, TokenManager};
use shared::protocol::CreateRunnerRequest;

/// Form retirement: create/update write a YAML file under `yaml_scan_dir`.
/// Tests in one binary run in parallel, so each gets its own directory (leaked
/// on purpose — the process exits after the test binary finishes).
fn isolated_pipeline_config() -> PipelineConfig {
    let path = tempfile::tempdir().expect("temp scan dir").keep();
    PipelineConfig {
        yaml_scan_dir: path.to_string_lossy().into_owned(),
        ..PipelineConfig::default()
    }
}

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
        pipeline: isolated_pipeline_config(),
        standalone: StandaloneConfig {
            enabled: false,
            auto_approve: vec![],
        },
        plugin_index: Default::default(),
        storage: Default::default(),
        logging: Default::default(),
        marketplace_url: String::new(),
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
    sqlx::query(
        "INSERT INTO users (id, username, password_hash, display_name, is_platform_admin, is_super_admin, is_active)
         VALUES ('admin-1', 'admin', 'hash', 'admin', 1, 1, 1),
                ('member-1', 'member', 'hash', 'member', 0, 0, 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    let created = create_runner_record(
        &pool,
        &CreateRunnerRequest {
            name: "lab-runner-01".into(),
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

async fn request_reissue(
    app: &Router,
    runner_id: &str,
    bearer: Option<&str>,
) -> (StatusCode, serde_json::Value) {
    let mut builder = Request::builder().method("POST").uri(format!(
        "/api/v1/runners/{runner_id}/register-token/reissue"
    ));
    if let Some(token) = bearer {
        builder = builder.header("Authorization", format!("Bearer {token}"));
    }
    let response = app
        .clone()
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body = serde_json::from_slice(&bytes).unwrap_or_default();
    (status, body)
}

async fn request_runner(
    app: &Router,
    method: &str,
    uri: &str,
    bearer: &str,
    body: Option<serde_json::Value>,
) -> (StatusCode, serde_json::Value) {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("Authorization", format!("Bearer {bearer}"));
    let request_body = if let Some(body) = body {
        builder = builder.header("Content-Type", "application/json");
        Body::from(body.to_string())
    } else {
        Body::empty()
    };
    let response = app
        .clone()
        .oneshot(builder.body(request_body).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body = serde_json::from_slice(&bytes).unwrap_or_default();
    (status, body)
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

fn admin_jwt() -> String {
    TokenManager::new(JWT_SECRET)
        .create_token("admin-1", Role::Admin, 24)
        .unwrap()
}

fn member_jwt() -> String {
    TokenManager::new(JWT_SECRET)
        .create_token("member-1", Role::User, 24)
        .unwrap()
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
async fn runner_creation_and_reissue_responses_include_one_time_token() {
    let (app, _pool, runner_id, _) = app_with_runner().await;

    let (status, created) = request_runner(
        &app,
        "POST",
        "/api/v1/runners",
        &admin_jwt(),
        Some(serde_json::json!({ "name": "new-runner" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    assert!(!created["data"]["register_token"]
        .as_str()
        .unwrap()
        .is_empty());

    let (status, reissued) = request_reissue(&app, &runner_id, Some(&admin_jwt())).await;
    assert_eq!(status, StatusCode::OK, "{reissued}");
    assert!(!reissued["data"]["register_token"]
        .as_str()
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn runner_detail_and_update_responses_do_not_expose_register_token() {
    let (app, _pool, runner_id, _) = app_with_runner().await;
    let admin = admin_jwt();

    let (status, detail) = request_runner(
        &app,
        "GET",
        &format!("/api/v1/runners/{runner_id}"),
        &admin,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{detail}");
    assert!(detail["data"].get("register_token").is_none());

    let (status, updated) = request_runner(
        &app,
        "PUT",
        &format!("/api/v1/runners/{runner_id}"),
        &admin,
        Some(serde_json::json!({ "labels": ["linux"] })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    assert!(updated["data"].get("register_token").is_none());
}

#[tokio::test]
async fn reissue_register_token_requires_platform_admin() {
    let (app, _pool, runner_id, _) = app_with_runner().await;

    let (status, _) = request_reissue(&app, &runner_id, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, _) = request_reissue(&app, &runner_id, Some(&member_jwt())).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn admin_reissue_token_can_recover_runner_registration() {
    let (app, pool, runner_id, initial_token) = app_with_runner().await;
    let (status, _) = register(&app, "lab-runner-01", &initial_token, None).await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = request_reissue(&app, &runner_id, Some(&admin_jwt())).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let replacement_token = body["data"]["register_token"].as_str().unwrap();
    assert!(!replacement_token.is_empty());
    assert_ne!(replacement_token, initial_token);

    let stored_token: String =
        sqlx::query_scalar("SELECT register_token FROM runners WHERE id = ?")
            .bind(&runner_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    // Hash-only at rest: the row carries SHA-256(replacement).
    assert_eq!(
        stored_token,
        shared::api_token::hash_token(replacement_token)
    );

    let (status, registration) = register(&app, "lab-runner-01", replacement_token, None).await;
    assert_eq!(status, StatusCode::OK, "{registration}");
    assert!(registration["data"]["token"].as_str().unwrap().len() > 10);

    let (status, _) = register(&app, "lab-runner-01", replacement_token, None).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "register token is one-shot"
    );
}

#[tokio::test]
async fn reissue_missing_runner_returns_not_found() {
    let (app, _pool, _, _) = app_with_runner().await;
    let (status, _) = request_reissue(
        &app,
        "00000000-0000-0000-0000-000000000009",
        Some(&admin_jwt()),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
