//! Integration tests for declarative manual-input validation at trigger time:
//! `type` / `pattern` / `options` / `required` declared in `input_defs` must be
//! enforced by POST /pipelines/{id}/trigger (and the shared webhook path),
//! returning 422 with field-level messages instead of queuing the job.

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

    sqlx::query(
        "INSERT INTO users (id, username, password_hash, display_name, is_platform_admin, is_super_admin, is_active)
         VALUES (?, ?, 'hash', ?, 1, 1, 1)",
    )
    .bind(SUPER_ADMIN_ID)
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

    // A registered Host so `type: host` inputs can pass validation.
    sqlx::query(
        "INSERT INTO hosts (id, name, host, port, username, credential_type, credential_data)
         VALUES ('host-1', 'prod-01', '10.0.0.1', 22, 'root', 'none', '{}')",
    )
    .execute(&pool)
    .await
    .expect("seed host");

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
        .create_token(SUPER_ADMIN_ID, Role::Admin, 1)
        .expect("mint admin jwt")
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

const INPUT_DEFS_JSON: &str = r#"[
    {
        "name": "tag",
        "description": "ACR image tag",
        "required": true,
        "default": "0.1.0",
        "type": "string",
        "pattern": "^[A-Za-z0-9._-]+$"
    },
    {
        "name": "deploy_host",
        "description": "Buz Host name",
        "required": true,
        "type": "host"
    },
    {
        "name": "env",
        "description": "Target environment",
        "type": "choice",
        "options": ["staging", "production"]
    },
    {
        "name": "replicas",
        "type": "number"
    },
    {
        "name": "dry_run",
        "type": "boolean",
        "default": "false"
    }
]"#;

async fn create_pipeline(app: &Router, input_defs_json: &str) -> String {
    let payload = format!(
        r#"{{
            "name": "input-validate",
            "trigger_type": "manual",
    "environments": ["default"],
            "runner_labels": ["linux-x64"],
            "team_id": "team-1",
            "env": [],
            "steps": [{{ "name": "echo", "run": "echo hi", "engine": "shell" }}],
            "input_defs": {input_defs_json}
        }}"#
    );
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/pipelines")
                .header(AUTHORIZATION, bearer(&admin_jwt()))
                .header("Content-Type", "application/json")
                .body(Body::from(payload))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "create should succeed");
    let created = read_body(resp).await;
    created["data"]["id"]
        .as_str()
        .expect("pipeline id")
        .to_string()
}

async fn trigger_with_inputs(
    app: &Router,
    pipeline_id: &str,
    inputs_json: &str,
) -> axum::response::Response {
    app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/v1/pipelines/{pipeline_id}/trigger"))
                .header(AUTHORIZATION, bearer(&admin_jwt()))
                .header("Content-Type", "application/json")
                .body(Body::from(format!(
                    r#"{{"environment":"default","inputs": {inputs_json}}}"#
                )))
                .unwrap(),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn valid_inputs_pass_validation() {
    let (app, _pool) = test_app().await;
    let pid = create_pipeline(&app, INPUT_DEFS_JSON).await;

    let resp = trigger_with_inputs(
        &app,
        &pid,
        r#"{"tag":"1.2.3","deploy_host":"prod-01","env":"staging","replicas":"4","dry_run":"true"}"#,
    )
    .await;
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "all inputs satisfy constraints: {:?}",
        read_body(resp).await
    );
}

#[tokio::test]
async fn defaults_are_merged_and_validated() {
    let (app, _pool) = test_app().await;
    let pid = create_pipeline(&app, INPUT_DEFS_JSON).await;

    // `tag` and `deploy_host` provided; `dry_run` falls back to declared default.
    let resp = trigger_with_inputs(&app, &pid, r#"{"tag":"1.2.3","deploy_host":"prod-01"}"#).await;
    assert_eq!(resp.status(), StatusCode::OK, "{:?}", read_body(resp).await);
}

#[tokio::test]
async fn missing_required_is_rejected_with_field() {
    let (app, _pool) = test_app().await;
    let pid = create_pipeline(&app, INPUT_DEFS_JSON).await;

    let resp = trigger_with_inputs(&app, &pid, r#"{"tag":"1.2.3"}"#).await;
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body = read_body(resp).await;
    let message = body["message"].as_str().expect("message");
    assert!(
        message.contains("invalid trigger inputs"),
        "unexpected message: {message}"
    );
    assert!(message.contains("deploy_host"), "field name in message");
    assert!(message.contains("required"), "reason in message");
}

#[tokio::test]
async fn required_empty_value_is_rejected() {
    let (app, _pool) = test_app().await;
    let pid = create_pipeline(&app, INPUT_DEFS_JSON).await;

    // Blank string must count as "not filled" even though the key is present.
    // `deploy_host` is required with no default, so a blank value is rejected.
    let resp = trigger_with_inputs(&app, &pid, r#"{"tag":"1.2.3","deploy_host":""}"#).await;
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let message = read_body(resp).await["message"]
        .as_str()
        .expect("message")
        .to_string();
    assert!(message.contains("deploy_host"), "host flagged: {message}");
    assert!(message.contains("required"), "reason in message: {message}");
}

#[tokio::test]
async fn empty_value_falls_back_to_declared_default() {
    let (app, pool) = test_app().await;
    let pid = create_pipeline(&app, INPUT_DEFS_JSON).await;

    // `tag` is required but has a default → blank value merges the default
    // instead of failing; `dry_run` likewise falls back to "false".
    let resp = trigger_with_inputs(
        &app,
        &pid,
        r#"{"tag":"","deploy_host":"prod-01","dry_run":""}"#,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK, "{:?}", read_body(resp).await);

    let params: serde_json::Value = sqlx::query_scalar::<_, String>(
        "SELECT trigger_params FROM jobs ORDER BY created_at DESC LIMIT 1",
    )
    .fetch_one(&pool)
    .await
    .expect("job trigger_params")
    .parse()
    .expect("json");
    assert_eq!(params["tag"], "0.1.0", "default merged for required blank");
    assert_eq!(
        params["dry_run"], "false",
        "default merged for optional blank"
    );
}

#[tokio::test]
async fn pattern_violation_is_rejected() {
    let (app, _pool) = test_app().await;
    let pid = create_pipeline(&app, INPUT_DEFS_JSON).await;

    let resp = trigger_with_inputs(&app, &pid, r#"{"tag":"v1..$","deploy_host":"prod-01"}"#).await;
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body = read_body(resp).await;
    let message = body["message"].as_str().expect("message");
    assert!(message.contains("tag"), "tag field flagged");
    assert!(message.contains("does not match"), "pattern reason");
}

#[tokio::test]
async fn choice_violation_is_rejected() {
    let (app, _pool) = test_app().await;
    let pid = create_pipeline(&app, INPUT_DEFS_JSON).await;

    let resp = trigger_with_inputs(
        &app,
        &pid,
        r#"{"tag":"1.2.3","deploy_host":"prod-01","env":"sandbox"}"#,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body = read_body(resp).await;
    let message = body["message"].as_str().expect("message");
    assert!(message.contains("env"), "env field flagged");
    assert!(message.contains("must be one of"), "choice reason");
}

#[tokio::test]
async fn host_type_checks_registry() {
    let (app, _pool) = test_app().await;
    let pid = create_pipeline(&app, INPUT_DEFS_JSON).await;

    let resp = trigger_with_inputs(&app, &pid, r#"{"tag":"1.2.3","deploy_host":"ghost"}"#).await;
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body = read_body(resp).await;
    let message = body["message"].as_str().expect("message");
    assert!(message.contains("deploy_host"), "host field flagged");
    assert!(message.contains("not registered"), "host reason");
}

#[tokio::test]
async fn number_and_boolean_violations_are_rejected() {
    let (app, _pool) = test_app().await;
    let pid = create_pipeline(&app, INPUT_DEFS_JSON).await;

    let resp = trigger_with_inputs(
        &app,
        &pid,
        r#"{"tag":"1.2.3","deploy_host":"prod-01","replicas":"many","dry_run":"yes"}"#,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body = read_body(resp).await;
    let message = body["message"].as_str().expect("message");
    assert!(message.contains("replicas"), "replicas flagged");
    assert!(message.contains("dry_run"), "dry_run flagged");
}

#[tokio::test]
async fn invalid_input_never_creates_a_job() {
    let (app, pool) = test_app().await;
    let pid = create_pipeline(&app, INPUT_DEFS_JSON).await;

    let resp = trigger_with_inputs(
        &app,
        &pid,
        r#"{"tag":"bad tag with space","deploy_host":"prod-01"}"#,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);

    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM jobs")
        .fetch_one(&pool)
        .await
        .expect("count jobs");
    assert_eq!(count, 0, "validation failure must not queue a job");
}

#[tokio::test]
async fn webhook_path_enforces_required() {
    let (app, _pool) = test_app().await;
    // The team secret must exist before the pipeline: create() rejects a
    // webhook pipeline whose webhook_secret names no team secret.
    let seeded = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/secrets")
                .header(AUTHORIZATION, bearer(&admin_jwt()))
                .header("Content-Type", "application/json")
                .body(Body::from(
                    r#"{"name":"HOOK_SECRET","value":"hook-signing-secret","team_id":"team-1"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        seeded.status(),
        StatusCode::OK,
        "seed team secret: {:?}",
        read_body(seeded).await
    );
    let payload = format!(
        r#"{{
            "name": "hook-validate",
            "trigger_type": "webhook",
    "environments": ["default"],
            "webhook_path": "deploy-prod",
            "webhook_secret": "HOOK_SECRET",
            "runner_labels": ["linux-x64"],
            "team_id": "team-1",
            "env": [],
            "steps": [{{ "name": "echo", "run": "echo hi", "engine": "shell" }}],
            "input_defs": {INPUT_DEFS_JSON}
        }}"#
    );
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/pipelines")
                .header(AUTHORIZATION, bearer(&admin_jwt()))
                .header("Content-Type", "application/json")
                .body(Body::from(payload))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "create webhook pipeline: {:?}",
        read_body(resp).await
    );

    // A webhook without the signature header is refused (403) before any
    // input validation happens — the pipeline row now carries HOOK_SECRET.
    let unsigned = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/hooks/deploy-prod?tag=1.2.3")
                .header("Content-Type", "application/json")
                .body(Body::from(r#"{}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        unsigned.status(),
        StatusCode::FORBIDDEN,
        "signature required once the secret is declared: {:?}",
        read_body(unsigned).await
    );

    // Missing required `deploy_host` via the webhook path (signed).
    let hook = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/hooks/deploy-prod?tag=1.2.3")
                .header("Content-Type", "application/json")
                .header(
                    "X-Buz-Signature",
                    buzz_signature("hook-signing-secret", "{}"),
                )
                .body(Body::from(r#"{}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(hook.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body = read_body(hook).await;
    let message = body["message"].as_str().expect("message");
    assert!(message.contains("deploy_host"), "hook required enforced");

    // Valid hook inputs pass (signed).
    let ok = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/hooks/deploy-prod?tag=1.2.3&deploy_host=prod-01&environment=default")
                .header("Content-Type", "application/json")
                .header(
                    "X-Buz-Signature",
                    buzz_signature("hook-signing-secret", "{}"),
                )
                .body(Body::from(r#"{}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        ok.status(),
        StatusCode::OK,
        "hook with valid inputs: {:?}",
        read_body(ok).await
    );
}

/// Mirror the webhook signature scheme: `sha256=<hex HMAC-SHA256(body)>`.
fn buzz_signature(secret: &str, body: &str) -> String {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(secret.as_bytes())
        .expect("hmac accepts any key length");
    mac.update(body.as_bytes());
    format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
}
