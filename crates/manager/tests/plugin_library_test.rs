//! Plugin library smoke test (ADR step-marketplace Phase 1): a pipeline step referencing a
//! local-index template resolves and locks at save time. Covers the happy
//! path end-to-end over the HTTP API plus the two main authoring errors.

use axum::{
    body::Body,
    http::{header::AUTHORIZATION, Request, StatusCode},
    Router,
};
use http_body_util::BodyExt;
use shared::auth::{Role, TokenManager};
use sqlx::sqlite::SqlitePoolOptions;
use tower::ServiceExt;

use manager::api;
use manager::config::AuthConfig;
use manager::config::Config;
use manager::config::PipelineConfig;
use manager::config::RunnerConfig;
use manager::config::ServerConfig;
use manager::crypto;
use manager::db::run_migrations;
use manager::services::log_broadcast::LogBroadcaster;
use manager::state::AppState;

const JWT_SECRET: &str = "test-secret-0123456789-abcdefghijklmnop";
const ADMIN_ID: &str = "admin-1";

async fn test_app_with_index(index_dir: &std::path::Path) -> (Router, sqlx::SqlitePool) {
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
    .bind(ADMIN_ID)
    .bind("boss")
    .bind("boss")
    .execute(&pool)
    .await
    .expect("seed user");
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

    let secret = crypto::encrypt("not-a-url", JWT_SECRET).expect("encrypt test secret");
    sqlx::query(
        "INSERT INTO secrets (id, name, value, team_id, kind)
         VALUES ('secret-1', 'FEISHU', ?, 'team-1', 'generic')",
    )
    .bind(secret)
    .execute(&pool)
    .await
    .expect("seed template secret");

    let mut config = Config {
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
    };
    config.plugin_index.local_dir = index_dir.to_string_lossy().into_owned();

    let state = AppState {
        pool: pool.clone(),
        config,
        log_broadcast: LogBroadcaster::new(),
    };
    (Router::new().nest("/api/v1", api::router(state)), pool)
}

fn feishu_spec() -> String {
    r#"
id: buz/feishu
version: 1.2.0
publisher: official
author: Buz Maintainers
engine: shell
source_repo: https://github.com/schiplat/buz-plugins
inputs:
  webhook:
    type: string
    required: true
    masking: true
    pattern: '^https://[^ ]+$'
run: |
  curl -fsSL -X POST "$INPUT_WEBHOOK" -d '{}'
"#
    .into()
}

fn create_payload(steps: &str) -> String {
    format!(
        r#"{{
        "name": "uses-smoke",
        "trigger_type": "manual",
    "environments": ["default"],
        "runner_labels": ["linux-x64"],
        "team_id": "team-1",
        "steps": {steps}
    }}"#
    )
}

async fn create_pipeline(app: &Router, jwt: &str, steps: &str) -> (StatusCode, serde_json::Value) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/pipelines")
                .header(AUTHORIZATION, format!("Bearer {jwt}"))
                .header("Content-Type", "application/json")
                .body(Body::from(create_payload(steps)))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let body = read_json(resp).await;
    (status, body)
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

#[tokio::test]
async fn uses_step_resolves_expands_and_locks_provenance() {
    let index = tempfile::tempdir().unwrap();
    std::fs::write(index.path().join("feishu.yaml"), feishu_spec()).unwrap();
    let (app, pool) = test_app_with_index(index.path()).await;
    let jwt = TokenManager::new(JWT_SECRET)
        .create_token(ADMIN_ID, Role::Admin, 1)
        .unwrap();

    let (status, body) = create_pipeline(
        &app,
        &jwt,
        r#"[{ "name": "ordinary", "run": "echo before" },
            { "name": "notify",
              "uses": "buz/feishu@1.2",
              "with": { "webhook": "${secrets.FEISHU}" } }]"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let pipeline_id = body["data"]["id"]
        .as_str()
        .expect("pipeline id")
        .to_string();

    // Stored task is the expanded template: concrete run + INPUT_* env.
    let (command, step_env): (String, String) = sqlx::query_as(
        "SELECT command_template, step_env FROM tasks WHERE pipeline_id = ? AND step_order = 1",
    )
    .bind(&pipeline_id)
    .fetch_one(&pool)
    .await
    .expect("task row");
    assert!(command.contains("INPUT_WEBHOOK"), "command: {command}");
    assert!(step_env.contains("INPUT_WEBHOOK"), "step_env: {step_env}");

    // Provenance row locks the resolution (per-pipeline, ADR §6.4).
    let (uses_ref, version, hash, source): (String, String, String, String) = sqlx::query_as(
        "SELECT uses_ref, resolved_version, content_sha256, source
         FROM step_resolutions WHERE pipeline_id = ?",
    )
    .bind(&pipeline_id)
    .fetch_one(&pool)
    .await
    .expect("provenance row");
    assert_eq!(uses_ref, "buz/feishu@1.2");
    assert_eq!(version, "1.2.0");
    assert_eq!(hash.len(), 64);
    assert!(source.starts_with("local:"), "{source}");

    let jwt = TokenManager::new(JWT_SECRET)
        .create_token(ADMIN_ID, Role::Admin, 1)
        .unwrap();
    let trigger = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/v1/pipelines/{pipeline_id}/trigger"))
                .header(AUTHORIZATION, format!("Bearer {jwt}"))
                .header("Content-Type", "application/json")
                .body(Body::from(r#"{"environment":"default","inputs":{}}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(trigger.status(), StatusCode::OK);
    let trigger_body = read_json(trigger).await;
    let job_id = trigger_body["data"]["job_id"].as_str().unwrap();
    let snapshot_raw: String = sqlx::query_scalar("SELECT snapshot FROM jobs WHERE id = ?")
        .bind(job_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    let snapshot: serde_json::Value = serde_json::from_str(&snapshot_raw).unwrap();
    let task = &snapshot["tasks"][1];
    assert_eq!(task["name"], "notify");
    assert_eq!(task["marketplace"]["uses_ref"], "buz/feishu@1.2");
    assert_eq!(task["marketplace"]["resolved_version"], "1.2.0");
    assert_eq!(
        task["marketplace"]["content_sha256"]
            .as_str()
            .unwrap()
            .len(),
        64
    );
    assert_eq!(task["marketplace"]["source"], source);
    assert_eq!(task["marketplace"]["reviewed"], false);
    assert_eq!(task["step_order"], 1);
    assert!(snapshot_raw.contains("${secrets.FEISHU}"));
    assert!(!snapshot_raw.contains("not-a-url"));

    // Claim validates the decrypted secret against the template constraint,
    // fails closed, and marks the corresponding INPUT_* key for log masking.
    let poll = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v1/runners/poll")
                .header(
                    AUTHORIZATION,
                    format!(
                        "Bearer {}",
                        TokenManager::new(JWT_SECRET)
                            .create_token("runner-1", Role::Runner, 1)
                            .unwrap()
                    ),
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(poll.status(), StatusCode::OK);
    let poll_body = read_json(poll).await;
    let dispatched = &poll_body["data"]["tasks"][1];
    assert_eq!(dispatched["step_order"], 1);
    assert_eq!(dispatched["masking_env_keys"][0], "INPUT_WEBHOOK");
    assert!(dispatched["input_validation_error"]
        .as_str()
        .unwrap()
        .contains("pattern"));
    assert_eq!(dispatched["step_env"]["INPUT_WEBHOOK"], "not-a-url");

    // Later pipeline edits must not rewrite historical execution provenance.
    sqlx::query("UPDATE step_resolutions SET resolved_version = '9.9.9' WHERE pipeline_id = ?")
        .bind(&pipeline_id)
        .execute(&pool)
        .await
        .unwrap();
    let stored_snapshot: String = sqlx::query_scalar("SELECT snapshot FROM jobs WHERE id = ?")
        .bind(job_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    let stored: serde_json::Value = serde_json::from_str(&stored_snapshot).unwrap();
    assert_eq!(
        stored["tasks"][1]["marketplace"]["resolved_version"],
        "1.2.0"
    );
}

#[tokio::test]
async fn frozen_docker_task_claim_preserves_privileged_and_nullable_host_id() {
    let index = tempfile::tempdir().unwrap();
    let (app, pool) = test_app_with_index(index.path()).await;
    let jwt = TokenManager::new(JWT_SECRET)
        .create_token(ADMIN_ID, Role::Admin, 1)
        .unwrap();

    let (status, body) = create_pipeline(
        &app,
        &jwt,
        r#"[{ "name": "container", "run": "echo hi", "engine": "docker", "image": "alpine:3.20", "privileged": true }]"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let pipeline_id = body["data"]["id"].as_str().unwrap();

    let stored_host_id: Option<String> =
        sqlx::query_scalar("SELECT host_id FROM tasks WHERE pipeline_id = ? AND step_order = 0")
            .bind(pipeline_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(stored_host_id, None);

    let trigger = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/v1/pipelines/{pipeline_id}/trigger"))
                .header(AUTHORIZATION, format!("Bearer {jwt}"))
                .header("Content-Type", "application/json")
                .body(Body::from(r#"{"environment":"default","inputs":{}}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(trigger.status(), StatusCode::OK);

    let poll = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v1/runners/poll")
                .header(
                    AUTHORIZATION,
                    format!(
                        "Bearer {}",
                        TokenManager::new(JWT_SECRET)
                            .create_token("runner-1", Role::Runner, 1)
                            .unwrap()
                    ),
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(poll.status(), StatusCode::OK);
    let poll_body = read_json(poll).await;
    let dispatched = &poll_body["data"]["tasks"][0];
    assert_eq!(dispatched["privileged"], true);
}

#[tokio::test]
async fn vars_input_is_validated_after_claim_time_expansion() {
    let index = tempfile::tempdir().unwrap();
    std::fs::write(
        index.path().join("retries.yaml"),
        r#"
id: buz/retries
version: 1.0.0
publisher: official
author: Buz Maintainers
source_repo: https://github.com/schiplat/buz-plugins
inputs:
  retries:
    type: number
    required: true
run: echo "$INPUT_RETRIES"
"#,
    )
    .unwrap();
    let (app, _pool) = test_app_with_index(index.path()).await;
    let jwt = TokenManager::new(JWT_SECRET)
        .create_token(ADMIN_ID, Role::Admin, 1)
        .unwrap();

    let create = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/pipelines")
                .header(AUTHORIZATION, format!("Bearer {jwt}"))
                .header("Content-Type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                                        "name": "vars-input",
                                        "trigger_type": "manual",
                    "environments": ["default"],
                                        "runner_labels": ["linux-x64"],
                                        "team_id": "team-1",
                                        "env": [{"name": "RETRIES", "value": "not-a-number"}],
                                        "steps": [{
                                            "name": "retry-step",
                                            "uses": "buz/retries@1",
                                            "with": {"retries": "${vars.RETRIES}"}
                                        }]
                                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(create.status(), StatusCode::OK);
    let created = read_json(create).await;
    let pipeline_id = created["data"]["id"].as_str().unwrap();

    let trigger = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/v1/pipelines/{pipeline_id}/trigger"))
                .header(AUTHORIZATION, format!("Bearer {jwt}"))
                .header("Content-Type", "application/json")
                .body(Body::from(r#"{"environment":"default","inputs":{}}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(trigger.status(), StatusCode::OK);

    let poll = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v1/runners/poll")
                .header(
                    AUTHORIZATION,
                    format!(
                        "Bearer {}",
                        TokenManager::new(JWT_SECRET)
                            .create_token("runner-1", Role::Runner, 1)
                            .unwrap()
                    ),
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(poll.status(), StatusCode::OK);
    let poll_body = read_json(poll).await;
    let dispatched = &poll_body["data"]["tasks"][0];
    assert_eq!(dispatched["step_env"]["INPUT_RETRIES"], "not-a-number");
    assert!(dispatched["input_validation_error"]
        .as_str()
        .unwrap()
        .contains("integer"));
}

#[tokio::test]
async fn unknown_or_unsatisfiable_uses_are_rejected_at_save() {
    let index = tempfile::tempdir().unwrap();
    std::fs::write(index.path().join("feishu.yaml"), feishu_spec()).unwrap();
    let (app, _pool) = test_app_with_index(index.path()).await;
    let jwt = TokenManager::new(JWT_SECRET)
        .create_token(ADMIN_ID, Role::Admin, 1)
        .unwrap();

    // Unknown step id → explicit not-in-index error, no pipeline created.
    let (status, body) = create_pipeline(
        &app,
        &jwt,
        r#"[{ "name": "notify", "uses": "buz/absent@1" }]"#,
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains("not found in the local plugin index"),
        "{body}"
    );

    // Missing required input → save-time error naming the input.
    let (status, body) = create_pipeline(
        &app,
        &jwt,
        r#"[{ "name": "notify", "uses": "buz/feishu@1.2" }]"#,
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains("missing required input 'webhook'"),
        "{body}"
    );

    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM pipelines WHERE name = 'uses-smoke'")
        .fetch_one(&_pool)
        .await
        .unwrap();
    assert_eq!(rows, 0, "failed saves must not leave a pipeline behind");
}
