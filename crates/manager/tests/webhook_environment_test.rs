//! Webhook handler environment selector precedence (query > body > YAML
//! `on.webhook.environment` default) and the Cron scheduler's fixed target
//! environment. `environment` is a reserved selector: it never leaks into the
//! business inputs, and there is no fallback to the first declared environment.

use axum::{
    body::Body,
    http::{Request, StatusCode},
    Router,
};
use http_body_util::BodyExt;
use shared::auth::{Role, TokenManager};
use sqlx::sqlite::{SqlitePool, SqlitePoolOptions};
use sqlx::Row;
use std::collections::HashMap;
use tower::ServiceExt;

use manager::api;
use manager::api::pipelines::do_trigger;
use manager::config::AuthConfig;
use manager::config::Config;
use manager::config::PipelineConfig;
use manager::config::RunnerConfig;
use manager::config::ServerConfig;
use manager::db::run_migrations;
use manager::services::log_broadcast::LogBroadcaster;
use manager::state::AppState;

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
        pipeline: isolated_pipeline_config(),
        standalone: Default::default(),
        plugin_index: Default::default(),
        storage: Default::default(),
        logging: Default::default(),
        marketplace_url: String::new(),
    }
}

async fn test_app() -> (Router, SqlitePool, AppState) {
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

    let state = AppState {
        pool: pool.clone(),
        config: test_config(),
        log_broadcast: LogBroadcaster::new(),
    };
    let app = Router::new().nest("/api/v1", api::router(state.clone()));
    (app, pool, state)
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

const HOOK_TEST_SECRET_NAME: &str = "HOOK_TEST_SECRET";
const HOOK_TEST_SECRET_VALUE: &str = "hook-test-signing-secret";

/// Signed webhook hook request (POST /api/v1/hooks/{path}?query) —
/// on.webhook.secret is mandatory, so every hook request carries the
/// X-Buz-Signature header over the exact body bytes.
async fn hook_req(
    app: &Router,
    uri: &str,
    body: Option<serde_json::Value>,
) -> axum::response::Response {
    let body_bytes: Vec<u8> = match &body {
        Some(b) => serde_json::to_vec(b).unwrap(),
        None => Vec::new(),
    };
    let req_body = Request::builder()
        .method("POST")
        .uri(uri)
        .header(
            "X-Buz-Signature",
            buzz_signature(HOOK_TEST_SECRET_VALUE, &body_bytes),
        )
        .header("Content-Type", "application/json")
        .body(Body::from(body_bytes))
        .unwrap();
    app.clone().oneshot(req_body).await.unwrap()
}

/// Mirror the webhook signature scheme: `sha256=<hex HMAC-SHA256(body)>`.
fn buzz_signature(secret: &str, body: &[u8]) -> String {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(secret.as_bytes())
        .expect("hmac accepts any key length");
    mac.update(body);
    format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
}

async fn create_env(app: &Router, jwt: &str, slug: &str) {
    let create = json_req(
        app,
        "POST",
        "/api/v1/environments",
        jwt,
        Some(serde_json::json!({
            "name": slug,
            "slug": slug,
            "protection_enabled": false,
        })),
    )
    .await;
    assert_eq!(create.status(), StatusCode::OK, "create env {slug}");
}

async fn json_req(
    app: &Router,
    method: &str,
    uri: &str,
    jwt: &str,
    body: Option<serde_json::Value>,
) -> axum::response::Response {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("Authorization", format!("Bearer {jwt}"));
    let req_body = if let Some(b) = body {
        builder = builder.header("Content-Type", "application/json");
        Body::from(serde_json::to_vec(&b).unwrap())
    } else {
        Body::empty()
    };
    app.clone()
        .oneshot(builder.body(req_body).unwrap())
        .await
        .unwrap()
}

/// Webhook pipeline declaring several environments, with an optional `version`
/// input so business inputs can be told apart from the reserved selector.
/// Seeds the team secret first, then binds it via the API's `webhook_secret`
/// field — create() rejects webhook pipelines whose secret is missing or
/// unknown, so the order matters.
async fn create_webhook_pipeline(
    app: &Router,
    _pool: &SqlitePool,
    jwt: &str,
    environments: &[&str],
) -> String {
    let seeded = json_req(
        app,
        "POST",
        "/api/v1/secrets",
        jwt,
        Some(serde_json::json!({
            "name": HOOK_TEST_SECRET_NAME,
            "value": HOOK_TEST_SECRET_VALUE,
            "team_id": "team-1",
        })),
    )
    .await;
    assert_eq!(seeded.status(), StatusCode::OK, "seed webhook team secret");
    let pipe = json_req(
        app,
        "POST",
        "/api/v1/pipelines",
        jwt,
        Some(serde_json::json!({
            "name": "hook-pipe",
            "trigger_type": "webhook",
            "webhook_path": "deploy-app",
            "webhook_secret": HOOK_TEST_SECRET_NAME,
            "runner_labels": ["linux-x64"],
            "team_id": "team-1",
            "environments": environments,
            "input_defs": [{ "name": "version", "type": "string", "required": false }],
            "env": [],
            "steps": [{ "name": "echo", "run": "echo hi", "engine": "shell" }]
        })),
    )
    .await;
    assert_eq!(pipe.status(), StatusCode::OK, "create webhook pipeline");
    read_body(pipe).await["data"]["id"]
        .as_str()
        .unwrap()
        .to_string()
}

async fn set_webhook_environment(pool: &SqlitePool, pipeline_id: &str, slug: &str) {
    sqlx::query("UPDATE pipelines SET webhook_environment = ? WHERE id = ?")
        .bind(slug)
        .bind(pipeline_id)
        .execute(pool)
        .await
        .expect("set webhook_environment");
}

/// Latest job of the pipeline: (environment slug, snapshot).
async fn latest_job(pool: &SqlitePool, pipeline_id: &str) -> (Option<String>, serde_json::Value) {
    let row = sqlx::query(
        "SELECT e.slug, j.snapshot FROM jobs j
         LEFT JOIN environments e ON e.id = j.environment_id
         WHERE j.pipeline_id = ?
         ORDER BY j.rowid DESC LIMIT 1",
    )
    .bind(pipeline_id)
    .fetch_optional(pool)
    .await
    .expect("load job")
    .expect("job exists");
    let snapshot: serde_json::Value = serde_json::from_str(&row.get::<String, _>(1)).unwrap();
    (row.get(0), snapshot)
}

// ── Webhook handler: selector precedence ────────────────────────────────────

#[tokio::test]
async fn hook_query_selector_wins_over_body() {
    let (app, pool, _state) = test_app().await;
    let admin = admin_jwt();
    for slug in ["staging", "production"] {
        create_env(&app, &admin, slug).await;
    }
    let pid = create_webhook_pipeline(&app, &pool, &admin, &["staging", "production"]).await;

    let resp = hook_req(
        &app,
        "/api/v1/hooks/deploy-app?environment=staging&version=1.0",
        Some(serde_json::json!({
            "environment": "production",
            "inputs": { "version": "2.0" }
        })),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK, "hook trigger");

    let (env, snapshot) = latest_job(&pool, &pid).await;
    assert_eq!(env.as_deref(), Some("staging"), "query selector wins");
    assert_eq!(
        snapshot["inputs"]["version"], "2.0",
        "body inputs win over query inputs"
    );
    assert!(
        snapshot["inputs"].get("environment").is_none(),
        "environment is reserved — never a business input: {}",
        snapshot["inputs"]
    );
}

#[tokio::test]
async fn hook_body_selector_used_when_query_has_none() {
    let (app, pool, _state) = test_app().await;
    let admin = admin_jwt();
    for slug in ["staging", "production"] {
        create_env(&app, &admin, slug).await;
    }
    let pid = create_webhook_pipeline(&app, &pool, &admin, &["staging", "production"]).await;

    let resp = hook_req(
        &app,
        "/api/v1/hooks/deploy-app",
        Some(serde_json::json!({
            "environment": "production",
            "inputs": { "version": "3.0" }
        })),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let (env, _) = latest_job(&pool, &pid).await;
    assert_eq!(env.as_deref(), Some("production"));
}

#[tokio::test]
async fn hook_body_inputs_environment_selector_used() {
    let (app, pool, _state) = test_app().await;
    let admin = admin_jwt();
    for slug in ["staging", "production"] {
        create_env(&app, &admin, slug).await;
    }
    let pid = create_webhook_pipeline(&app, &pool, &admin, &["staging", "production"]).await;

    let resp = hook_req(
        &app,
        "/api/v1/hooks/deploy-app",
        Some(serde_json::json!({ "inputs": { "environment": "production" } })),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let (env, snapshot) = latest_job(&pool, &pid).await;
    assert_eq!(env.as_deref(), Some("production"));
    assert!(
        snapshot["inputs"].get("environment").is_none(),
        "inputs.environment is still reserved, not a business input"
    );
}

#[tokio::test]
async fn hook_yaml_default_environment_used_when_no_selector() {
    let (app, pool, _state) = test_app().await;
    let admin = admin_jwt();
    for slug in ["staging", "yaml-default"] {
        create_env(&app, &admin, slug).await;
    }
    let pid = create_webhook_pipeline(&app, &pool, &admin, &["staging", "yaml-default"]).await;
    set_webhook_environment(&pool, &pid, "yaml-default").await;

    let resp = hook_req(
        &app,
        "/api/v1/hooks/deploy-app",
        Some(serde_json::json!({ "inputs": { "version": "9.9" } })),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let (env, _) = latest_job(&pool, &pid).await;
    assert_eq!(env.as_deref(), Some("yaml-default"), "YAML default applies");
}

#[tokio::test]
async fn hook_query_selector_overrides_yaml_default() {
    let (app, pool, _state) = test_app().await;
    let admin = admin_jwt();
    for slug in ["staging", "yaml-default"] {
        create_env(&app, &admin, slug).await;
    }
    let pid = create_webhook_pipeline(&app, &pool, &admin, &["staging", "yaml-default"]).await;
    set_webhook_environment(&pool, &pid, "yaml-default").await;

    let resp = hook_req(
        &app,
        "/api/v1/hooks/deploy-app?environment=staging",
        Some(serde_json::json!({})),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let (env, _) = latest_job(&pool, &pid).await;
    assert_eq!(env.as_deref(), Some("staging"), "query beats YAML default");
}

#[tokio::test]
async fn hook_without_selector_never_falls_back_to_first_declared() {
    let (app, pool, _state) = test_app().await;
    let admin = admin_jwt();
    for slug in ["staging", "production"] {
        create_env(&app, &admin, slug).await;
    }
    let pid = create_webhook_pipeline(&app, &pool, &admin, &["staging", "production"]).await;
    set_webhook_environment(&pool, &pid, "").await;

    let resp = hook_req(
        &app,
        "/api/v1/hooks/deploy-app",
        Some(serde_json::json!({ "inputs": { "version": "1.0" } })),
    )
    .await;
    assert_eq!(
        resp.status(),
        StatusCode::UNPROCESSABLE_ENTITY,
        "no selector → rejected, not silently defaulted"
    );
    let body = read_body(resp).await;
    assert!(
        body["message"]
            .as_str()
            .unwrap_or_default()
            .contains("environment is required"),
        "unexpected error: {body}"
    );
    let jobs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM jobs WHERE pipeline_id = ?")
        .bind(&pid)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(jobs, 0, "no job may be created without an environment");
}

// ── Cron scheduler: fixed target environment ───────────────────────────────

#[tokio::test]
async fn cron_trigger_targets_declared_environment() {
    let (app, pool, state) = test_app().await;
    let admin = admin_jwt();
    create_env(&app, &admin, "nightly").await;
    create_env(&app, &admin, "other").await;
    let pipe = json_req(
        &app,
        "POST",
        "/api/v1/pipelines",
        &admin,
        Some(serde_json::json!({
            "name": "cron-pipe",
            "trigger_type": "cron",
            "cron_expr": "0 3 * * *",
            "cron_environment": "nightly",
            "runner_labels": ["linux-x64"],
            "team_id": "team-1",
            "environments": ["nightly", "other"],
            "env": [],
            "steps": [{ "name": "echo", "run": "echo hi", "engine": "shell" }]
        })),
    )
    .await;
    assert_eq!(pipe.status(), StatusCode::OK, "create cron pipeline");
    let pid = read_body(pipe).await["data"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    // The scheduler calls do_trigger with `Some(cron_environment)` — the job
    // must be bound to exactly that environment.
    let result = do_trigger(
        &state,
        &pid,
        "cron",
        &HashMap::new(),
        false,
        false,
        Some("nightly"),
        None,
        None,
    )
    .await;
    assert!(result.is_ok(), "cron trigger must succeed: {result:?}");
    let (env, snapshot) = latest_job(&pool, &pid).await;
    assert_eq!(env.as_deref(), Some("nightly"));
    assert_eq!(snapshot["environment"], "nightly");

    let trigger_by: String = sqlx::query_scalar(
        "SELECT trigger_by FROM jobs WHERE pipeline_id = ? ORDER BY rowid DESC LIMIT 1",
    )
    .bind(&pid)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(trigger_by, "cron");
}

#[tokio::test]
async fn cron_trigger_without_environment_is_rejected() {
    let (_app, _pool, state) = test_app().await;
    let app = Router::new().nest("/api/v1", api::router(state.clone()));
    let admin = admin_jwt();
    create_env(&app, &admin, "nightly").await;
    // YAML-first: a cron trigger without on.cron.environment is rejected at
    // create (the schema has required it all along; the form path was lax).
    let pipe_resp = json_req(
        &app,
        "POST",
        "/api/v1/pipelines",
        &admin,
        Some(serde_json::json!({
            "name": "cron-noenv",
            "trigger_type": "cron",
            "cron_expr": "0 3 * * *",
            "runner_labels": ["linux-x64"],
            "team_id": "team-1",
            "environments": ["nightly"],
            "env": [],
            "steps": [{ "name": "echo", "run": "echo hi", "engine": "shell" }]
        })),
    )
    .await;
    assert_eq!(
        pipe_resp.status(),
        StatusCode::UNPROCESSABLE_ENTITY,
        "cron without cron_environment must be rejected: {:?}",
        read_body(pipe_resp).await
    );
    // (message asserted via the second create below — same contract)

    // do_trigger keeps its defense-in-depth rejection for an empty
    // environment on a declared pipeline.
    let ok_resp = json_req(
        &app,
        "POST",
        "/api/v1/pipelines",
        &admin,
        Some(serde_json::json!({
            "name": "cron-withenv",
            "trigger_type": "cron",
            "cron_expr": "0 3 * * *",
            "cron_environment": "nightly",
            "runner_labels": ["linux-x64"],
            "team_id": "team-1",
            "environments": ["nightly"],
            "env": [],
            "steps": [{ "name": "echo", "run": "echo hi", "engine": "shell" }]
        })),
    )
    .await;
    assert_eq!(ok_resp.status(), StatusCode::OK);
    let pid = read_body(ok_resp).await["data"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    let err = do_trigger(
        &state,
        &pid,
        "cron",
        &HashMap::new(),
        false,
        false,
        Some(""),
        None,
        None,
    )
    .await
    .expect_err("empty environment must be rejected");
    assert!(
        err.to_string().contains("environment is required"),
        "unexpected error: {err}"
    );

    // Undeclared / unregistered environment is rejected too.
    let err = do_trigger(
        &state,
        &pid,
        "cron",
        &HashMap::new(),
        false,
        false,
        Some("ghost"),
        None,
        None,
    )
    .await
    .expect_err("unknown environment must be rejected");
    assert!(err.to_string().contains("ghost"), "unexpected error: {err}");
}
