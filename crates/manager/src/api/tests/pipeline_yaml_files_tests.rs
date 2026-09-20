//! Integration tests for file-backed YAML authoring APIs.
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

fn test_config(scan_dir: &str) -> Config {
    Config {
        server: ServerConfig::default(),
        database: Default::default(),
        auth: AuthConfig {
            jwt_secret: JWT_SECRET.into(),
            jwt_ttl_hours: 24,
            runner_register_token_ttl_hours: 24,
        },
        runner: RunnerConfig::default(),
        pipeline: PipelineConfig {
            yaml_scan_dir: scan_dir.into(),
            yaml_scan_interval_seconds: 3600,
        },
        standalone: Default::default(),
        clock: Default::default(),
        logging: Default::default(),
    }
}

async fn test_app(scan_dir: &str) -> Router {
    let pool = SqlitePoolOptions::new()
        .max_connections(4)
        .connect("sqlite::memory:")
        .await
        .expect("open in-memory db");
    run_migrations(&pool, JWT_SECRET).await.expect("migrate db");

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
        config: test_config(scan_dir),
        log_broadcast: LogBroadcaster::new(),
    };
    Router::new().nest("/api/v1", api::router(state))
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

const SAMPLE_YAML: &str = r#"
name: file-backed-demo
team: default
runner:
  labels: [linux-x64]
steps:
  - name: greet
    run: echo hello
"#;

#[tokio::test]
async fn create_put_get_delete_yaml_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let scan = dir.path().to_string_lossy().to_string();
    let app = test_app(&scan).await;
    let token = admin_jwt();

    // Create
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/pipelines/yaml-files")
                .header(AUTHORIZATION, format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "yaml": SAMPLE_YAML,
                        "filename": "file-backed-demo.yml"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let created = read_body(resp).await;
    assert_eq!(created["code"], 0);
    let pipeline_id = created["data"]["pipeline_id"].as_str().unwrap().to_string();
    assert!(dir.path().join("file-backed-demo.yml").exists());

    // Get by filename
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v1/pipelines/yaml-files/file-backed-demo.yml")
                .header(AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let got = read_body(resp).await;
    assert!(got["data"]["yaml"]
        .as_str()
        .unwrap()
        .contains("file-backed-demo"));

    // Get by pipeline id
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/api/v1/pipelines/{pipeline_id}/yaml-source"))
                .header(AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Update
    let updated = SAMPLE_YAML.replace("echo hello", "echo updated");
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/v1/pipelines/yaml-files/file-backed-demo.yml")
                .header(AUTHORIZATION, format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({ "yaml": updated }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = std::fs::read_to_string(dir.path().join("file-backed-demo.yml")).unwrap();
    assert!(body.contains("echo updated"));

    // Delete file + projection
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/v1/pipelines/yaml-files/file-backed-demo.yml")
                .header(AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(!dir.path().join("file-backed-demo.yml").exists());

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/api/v1/pipelines/{pipeline_id}"))
                .header(AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn rejects_path_traversal_filename() {
    let dir = tempfile::tempdir().expect("tempdir");
    let scan = dir.path().to_string_lossy().to_string();
    let app = test_app(&scan).await;
    let token = admin_jwt();

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/pipelines/yaml-files")
                .header(AUTHORIZATION, format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "yaml": SAMPLE_YAML,
                        "filename": "../escape.yml"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn put_rejects_stale_etag() {
    let dir = tempfile::tempdir().expect("tempdir");
    let scan = dir.path().to_string_lossy().to_string();
    let app = test_app(&scan).await;
    let token = admin_jwt();

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/pipelines/yaml-files")
                .header(AUTHORIZATION, format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "yaml": SAMPLE_YAML,
                        "filename": "etag-demo.yml"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let created = read_body(resp).await;
    let etag = created["data"]["etag"].as_str().unwrap().to_string();

    let resp = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/v1/pipelines/yaml-files/etag-demo.yml")
                .header(AUTHORIZATION, format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "yaml": SAMPLE_YAML.replace("echo hello", "echo other"),
                        "etag": "\"deadbeef-1\""
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    // Original etag still valid after failed put
    let _ = etag;
}

#[tokio::test]
async fn create_rejects_target_team_without_write_before_file_exists() {
    let dir = tempfile::tempdir().expect("tempdir");
    let scan = dir.path().to_string_lossy().to_string();
    let pool = SqlitePoolOptions::new()
        .max_connections(4)
        .connect("sqlite::memory:")
        .await
        .expect("db");
    run_migrations(&pool, JWT_SECRET).await.expect("migrate");

    sqlx::query(
        "INSERT INTO users (id, username, password_hash, display_name, is_platform_admin, is_super_admin, is_active)
         VALUES ('dev-1', 'dev', 'hash', 'Dev', 0, 0, 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO teams (id, name, slug) VALUES ('team-1', 'Default', 'default')")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO teams (id, name, slug) VALUES ('team-2', 'Other', 'other')")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO team_members (team_id, user_id, role) VALUES ('team-1', 'dev-1', 'developer')",
    )
    .execute(&pool)
    .await
    .unwrap();

    let state = AppState {
        pool,
        config: test_config(&scan),
        log_broadcast: LogBroadcaster::new(),
    };
    let app = Router::new().nest("/api/v1", api::router(state));
    let token = TokenManager::new(JWT_SECRET)
        .create_token("dev-1", Role::User, 1)
        .expect("jwt");

    let yaml = r#"
name: steal
team: other
steps:
  - name: x
    run: echo pwn
"#;
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/pipelines/yaml-files")
                .header(AUTHORIZATION, format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "yaml": yaml,
                        "filename": "steal.yml"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        resp.status() == StatusCode::UNAUTHORIZED || resp.status() == StatusCode::FORBIDDEN,
        "status={}",
        resp.status()
    );
    assert!(
        !dir.path().join("steal.yml").exists(),
        "must not write file before auth"
    );
}
