//! Step pool integration (step-marketplace §6.8 Phase 1): a remote source's
//! index is synced into the pool, curated on, and a pipeline step referencing
//! it resolves at save time over HTTP — with review-gate and branch-pin
//! behavior covered.

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
use crate::config::{AuthConfig, Config};
use crate::db::run_migrations;
use crate::services::log_broadcast::LogBroadcaster;
use crate::state::AppState;

const JWT_SECRET: &str = "test-secret-0123456789-abcdefghijklmnop";
const ADMIN_ID: &str = "admin-1";

fn community_spec() -> String {
    r#"
id: alice/oss-upload
version: 1.0.0
publisher: community
author: alice
source_repo: https://github.com/alice/my-buz-steps
engine: shell
requires_buz: ">=1.0.0"
inputs:
  bucket:
    type: string
    required: true
  file:
    type: string
    required: true
run: |
  set -eu
  : "${INPUT_BUCKET:?}" "${INPUT_FILE:?}"
  echo "upload ${INPUT_FILE} -> ${INPUT_BUCKET}"
"#
    .into()
}

fn index_json(good_sha: &str) -> String {
    let broken_hash = "0".repeat(64);
    format!(
        r#"{{ "steps": [
            {{ "id": "alice/oss-upload", "version": "1.0.0",
               "file": "steps/oss-upload/1.0.0.yaml", "sha256": "{good_sha}" }},
            {{ "id": "alice/broken-fetch", "version": "0.1.0",
               "file": "steps/broken-fetch/0.1.0.yaml", "sha256": "{}" }},
            {{ "id": "alice/broken-hash", "version": "0.1.1",
               "file": "steps/broken-hash.yaml", "sha256": "{broken_hash}" }} ] }}"#,
        "b".repeat(64)
    )
}

/// Serves the community source: index.json, the template YAML, and a
/// deliberately missing branch index (for the branch-pin error path).
async fn spawn_source_server(yaml: String, yaml_sha: String) -> String {
    let yaml = std::sync::Arc::new(yaml);
    let yaml_sha = std::sync::Arc::new(yaml_sha);
    let app = Router::new()
        .route(
            "/index.json",
            axum::routing::get(move || {
                let sha = yaml_sha.clone();
                async move { index_json(&sha) }
            }),
        )
        .route(
            "/steps/oss-upload/1.0.0.yaml",
            axum::routing::get(move || {
                let body = (*yaml).clone();
                async move { body }
            }),
        )
        .route(
            "/steps/broken-hash.yaml",
            axum::routing::get(|| async {
                "actual content does not match the declared zeros hash"
            }),
        )
        .route(
            "/reviews.json",
            axum::routing::get(|| async { r#"{"reviews": []}"# }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

async fn test_app() -> (Router, sqlx::SqlitePool) {
    let pool = SqlitePoolOptions::new()
        .max_connections(4)
        .connect("sqlite::memory:")
        .await
        .expect("open in-memory db");
    run_migrations(&pool, JWT_SECRET).await.expect("migrate db");

    sqlx::query(
        "INSERT INTO users (id, username, password_hash, display_name, is_platform_admin, is_super_admin, is_active)
         VALUES (?, ?, 'hash', ?, 1, 0, 1)",
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
    sqlx::query(
        "INSERT INTO runners (id, name, register_token, labels, status, max_concurrent_jobs)
         VALUES ('runner-1', 'r1', 'tok', '[\"linux-x64\"]', 'online', 2)",
    )
    .execute(&pool)
    .await
    .expect("seed runner");

    let state = AppState {
        pool: pool.clone(),
        config: Config {
            server: Default::default(),
            database: Default::default(),
            auth: AuthConfig {
                jwt_secret: JWT_SECRET.into(),
                jwt_ttl_hours: 24,
                runner_register_token_ttl_hours: 24,
            },
            runner: Default::default(),
            pipeline: Default::default(),
            standalone: Default::default(),
            step_index: Default::default(),
            clock: Default::default(),
            logging: Default::default(),
            marketplace_url: String::new(),
        },
        log_broadcast: LogBroadcaster::new(),
    };
    (Router::new().nest("/api/v1", api::router(state)), pool)
}

async fn read_json(response: axum::response::Response) -> serde_json::Value {
    let body = response
        .into_body()
        .collect()
        .await
        .expect("read body")
        .to_bytes();
    serde_json::from_slice(&body)
        .unwrap_or_else(|e| panic!("json body ({}): {e}", String::from_utf8_lossy(&body)))
}

async fn add_source(app: &Router, jwt: &str, url: &str) -> serde_json::Value {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/admin/step-sources")
                .header(AUTHORIZATION, format!("Bearer {jwt}"))
                .header("Content-Type", "application/json")
                .body(Body::from(
                    serde_json::json!({ "name": "alice", "url": url, "auto_enable": false })
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let body = read_json(resp).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

async fn save_pipeline(app: &Router, jwt: &str, uses: &str) -> (StatusCode, serde_json::Value) {
    let payload = serde_json::json!({
        "name": "pool-smoke",
        "trigger_type": "manual",
        "environments": ["default"],
        "runner_labels": ["linux-x64"],
        "team_id": "team-1",
        "steps": [
            { "name": "upload", "uses": uses,
              "with": { "bucket": "s3://x", "file": "a.txt" } }
        ]
    });
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/pipelines")
                .header(AUTHORIZATION, format!("Bearer {jwt}"))
                .header("Content-Type", "application/json")
                .body(Body::from(payload.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let body = read_json(resp).await;
    (status, body)
}

#[tokio::test]
async fn community_source_curates_resolves_and_gates() {
    let yaml = community_spec();
    let yaml_sha = {
        use sha2::{Digest, Sha256};
        format!("{:x}", Sha256::digest(yaml.as_bytes()))
    };
    let source_url = spawn_source_server(yaml.clone(), yaml_sha.clone()).await;
    let (app, pool) = test_app().await;
    let jwt = TokenManager::new(JWT_SECRET)
        .create_token(ADMIN_ID, Role::Admin, 1)
        .unwrap();

    // 1. Add the source: immediate sync stages the pool row (disabled).
    let body = add_source(&app, &jwt, &format!("{source_url}/index.json")).await;
    assert_eq!(body["data"]["synced"], true, "{body}");
    assert_eq!(body["data"]["source"]["auto_enable"], false);

    // 2. Pool row exists but is staged (disabled) — resolution must fail.
    let (status, body) = save_pipeline(&app, &jwt, "alice/oss-upload@1.0.0").await;
    assert_ne!(
        status,
        StatusCode::OK,
        "staged template must not resolve: {body}"
    );

    // 3a. Enabling a broken entry is REJECTED (file 404) and stays staged.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/v1/admin/step-pool/alice/broken-fetch/0.1.0")
                .header(AUTHORIZATION, format!("Bearer {jwt}"))
                .header("Content-Type", "application/json")
                .body(Body::from(r#"{ "enabled": true }"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let staged: (i64,) =
        sqlx::query_as("SELECT enabled FROM step_pool WHERE step_id = 'broken-fetch'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(staged.0, 0, "failed enable must keep the entry staged");

    // 3b. Enabling an entry whose content hash mismatches the index → rejected.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/v1/admin/step-pool/alice/broken-hash/0.1.1")
                .header(AUTHORIZATION, format!("Bearer {jwt}"))
                .header("Content-Type", "application/json")
                .body(Body::from(r#"{ "enabled": true }"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);

    // 3c. Curator enables the good entry.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/v1/admin/step-pool/alice/oss-upload/1.0.0")
                .header(AUTHORIZATION, format!("Bearer {jwt}"))
                .header("Content-Type", "application/json")
                .body(Body::from(r#"{ "enabled": true }"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // 4. Save-time resolution now succeeds over HTTP and records provenance.
    let (status, body) = save_pipeline(&app, &jwt, "alice/oss-upload@1.0.0").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let pipeline_id = body["data"]["id"]
        .as_str()
        .expect("pipeline id")
        .to_string();
    let resolutions: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM step_resolutions WHERE pipeline_id = ?")
            .bind(&pipeline_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        resolutions.0, 1,
        "provenance recorded for the remote template"
    );

    // 5. sha256 pin resolves to the same content.
    let (status, body) =
        save_pipeline(&app, &jwt, &format!("alice/oss-upload@sha256:{yaml_sha}")).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // 6. Strict review gate (via the settings API so the runtime override
    // applies): the pool row has no review badge → save rejected.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/v1/admin/settings/step_review_required")
                .header(AUTHORIZATION, format!("Bearer {jwt}"))
                .header("Content-Type", "application/json")
                .body(Body::from(r#"{ "value": "true" }"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let (status, body) = save_pipeline(&app, &jwt, "alice/oss-upload@1.0.0").await;
    assert_ne!(status, StatusCode::OK, "{body}");
    let message = body["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("review"),
        "gate error must mention review: {message}"
    );

    // 7. Branch pins surface a clear error when the branch has no index.
    let (status, body) = save_pipeline(&app, &jwt, "alice/oss-upload@main").await;
    assert_ne!(status, StatusCode::OK, "{body}");
    let message = body["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("branch") || message.contains("main"),
        "branch pin error should name the ref: {message}"
    );
}

#[tokio::test]
async fn official_source_seeds_and_startup_sync_populates_pool() {
    // The migration pre-seeds the official source (auto_enable) — a fresh
    // database must already carry it.
    let pool = SqlitePoolOptions::new()
        .max_connections(4)
        .connect("sqlite::memory:")
        .await
        .expect("open in-memory db");
    run_migrations(&pool, JWT_SECRET).await.expect("migrate db");

    let seeded: (String, String, i64, i64) = sqlx::query_as(
        "SELECT name, url, auto_enable, enabled FROM step_sources WHERE id = 'official'",
    )
    .fetch_one(&pool)
    .await
    .expect("official source seeded");
    assert_eq!(seeded.0, "Official buz-plugins");
    assert!(
        seeded.2 != 0,
        "auto_enable must be on for the official source"
    );
    assert!(seeded.3 != 0, "official source must be enabled");

    // Startup sync (sync_all_enabled) pulls the index and auto-enables every
    // entry — the "official templates work out of the box" contract.
    let yaml = r#"
id: buz/demo
version: 1.0.0
publisher: official
author: buz maintainers
engine: shell
inputs:
  name:
    type: string
    required: true
run: |
  echo "hello ${INPUT_NAME}"
"#
    .replace(
        "\n", "
",
    );
    let yaml_sha = {
        use sha2::{Digest, Sha256};
        format!("{:x}", Sha256::digest(yaml.as_bytes()))
    };
    let app = Router::new()
        .route(
            "/index.json",
            axum::routing::get(move || {
                let body = format!(
                    r#"{{ "steps": [ {{ "id": "buz/demo", "version": "1.0.0",
                       "file": "steps/demo/1.0.0.yaml", "sha256": "{yaml_sha}" }} ] }}"#
                );
                async move { body }
            }),
        )
        .route(
            "/steps/demo/1.0.0.yaml",
            axum::routing::get(move || {
                let body = yaml.clone();
                async move { body }
            }),
        )
        .route(
            "/reviews.json",
            axum::routing::get(|| async { r#"{"reviews": [{"sha256": "PLACEHOLDER"}]}"# }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    // Point the official source at the mock and run the startup sync.
    sqlx::query("UPDATE step_sources SET url = ? WHERE id = 'official'")
        .bind(format!("http://{addr}/index.json"))
        .execute(&pool)
        .await
        .unwrap();
    let synced = crate::services::step_pool::sync_all_enabled(&pool, &reqwest::Client::new()).await;
    assert_eq!(synced, 1);

    let entry: (String, i64, i64) =
        sqlx::query_as("SELECT version, enabled, reviewed FROM step_pool WHERE step_id = 'demo'")
            .fetch_one(&pool)
            .await
            .expect("pool row for buz/demo");
    assert_eq!(entry.0, "1.0.0");
    assert_eq!(entry.1, 1, "official auto_enable must enable entries");
    // reviews.json placeholder hash never matches → no badge (strict mode
    // would reject; official entries carry CI-signed hashes in production).
    assert_eq!(entry.2, 0);
}
