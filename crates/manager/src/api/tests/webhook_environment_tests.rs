//! Webhook handler environment selector precedence (query > body > YAML
//! `on.webhook.environment` default) and the Cron scheduler's fixed target
//! environment. `environment` is a reserved selector: it never leaks into the
//! business inputs, and there is no fallback to the first declared environment.
//! Included via `api/mod.rs` under `#[cfg(test)]`.

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

use crate::api;
use crate::api::pipelines::do_trigger;
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
        plugin_index: Default::default(),
        storage: Default::default(),
        clock: Default::default(),
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

/// Unauthenticated webhook hook request (POST /api/v1/hooks/{path}?query).
async fn hook_req(
    app: &Router,
    uri: &str,
    body: Option<serde_json::Value>,
) -> axum::response::Response {
    let builder = Request::builder().method("POST").uri(uri);
    let req_body = if let Some(b) = body {
        builder
            .header("Content-Type", "application/json")
            .body(Body::from(serde_json::to_vec(&b).unwrap()))
            .unwrap()
    } else {
        builder.body(Body::empty()).unwrap()
    };
    app.clone().oneshot(req_body).await.unwrap()
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
async fn create_webhook_pipeline(app: &Router, jwt: &str, environments: &[&str]) -> String {
    let pipe = json_req(
        app,
        "POST",
        "/api/v1/pipelines",
        jwt,
        Some(serde_json::json!({
            "name": "hook-pipe",
            "trigger_type": "webhook",
            "webhook_path": "deploy-app",
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
    let pid = create_webhook_pipeline(&app, &admin, &["staging", "production"]).await;

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
    let pid = create_webhook_pipeline(&app, &admin, &["staging", "production"]).await;

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
    let pid = create_webhook_pipeline(&app, &admin, &["staging", "production"]).await;

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
    let pid = create_webhook_pipeline(&app, &admin, &["staging", "yaml-default"]).await;
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
    let pid = create_webhook_pipeline(&app, &admin, &["staging", "yaml-default"]).await;
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
    let pid = create_webhook_pipeline(&app, &admin, &["staging", "production"]).await;
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
    let pipe = json_req(
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
    let pid = read_body(pipe).await["data"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    // Missing on.cron.environment (empty cron_environment): the scheduler skips
    // these; do_trigger itself rejects them as defense in depth.
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
