//! Git hook update API: mutable settings change via PUT; provider / hook_key
//! / secret stay immutable (delete + recreate). Audit rows record before/after.

mod common;

use axum::{
    body::Body,
    http::{header::AUTHORIZATION, Request, StatusCode},
    Router,
};
use http_body_util::BodyExt;
use manager::store::exec;
use shared::auth::{Role, TokenManager};
use tower::ServiceExt;

use manager::api;
use manager::config::{AuthConfig, Config, PipelineConfig, RunnerConfig, ServerConfig};
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
const ADMIN_ID: &str = "admin-1";

fn test_config() -> Config {
    Config {
        server: ServerConfig::default(),
        database: Default::default(),
        cluster: Default::default(),
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

fn test_state(pool: exec::DbPool) -> std::sync::Arc<AppState> {
    std::sync::Arc::new(AppState {
        pool: pool.clone(),
        config: test_config(),
        log_broadcast: LogBroadcaster::new(),
    })
}

fn bearer() -> String {
    format!(
        "Bearer {}",
        TokenManager::new(JWT_SECRET)
            .create_token(ADMIN_ID, Role::Admin, 1)
            .expect("mint jwt")
    )
}

async fn json_request(
    app: &Router,
    method: &str,
    uri: &str,
    body: Option<String>,
) -> (StatusCode, serde_json::Value) {
    let request = match body {
        Some(text) => Request::builder()
            .method(method)
            .uri(uri)
            .header(AUTHORIZATION, bearer())
            .header("content-type", "application/json")
            .body(Body::from(text))
            .unwrap(),
        None => Request::builder()
            .method(method)
            .uri(uri)
            .header(AUTHORIZATION, bearer())
            .body(Body::empty())
            .unwrap(),
    };
    let response = app.clone().oneshot(request).await.expect("serve");
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    let parsed = if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
    };
    (status, parsed)
}

async fn seeded_pipeline_with_hook() -> (Router, std::sync::Arc<AppState>, String, String) {
    let pool = common::test_db(4).await;

    exec::execute(&pool,
        "INSERT INTO users (id, username, password_hash, display_name, is_platform_admin, is_super_admin, is_active)
         VALUES (?, ?, 'hash', ?, 1, 1, 1)",
        &[ADMIN_ID.into(), "boss".into(), "boss".into()],
    )
    .await
    .expect("seed user");
    exec::execute(
        &pool,
        "INSERT INTO teams (id, name, slug) VALUES ('team-1', 'Default', 'default')",
        &[],
    )
    .await
    .expect("seed team");

    let state = test_state(pool);
    let app = Router::new().nest("/api/v1", api::router((*state).clone()));
    let (_, created) = json_request(
        &app,
        "POST",
        "/api/v1/pipelines",
        Some(
            r#"{
                "name": "hook-update-demo",
                "trigger_type": "manual",
                "environments": ["default"],
                "runner_labels": [],
                "team_id": "team-1",
                "steps": [{ "name": "build", "run": "echo hi", "engine": "shell" }]
            }"#
            .to_string(),
        ),
    )
    .await;
    assert_eq!(created["code"], 0, "{created}");
    let pipeline_id = created["data"]["id"].as_str().unwrap().to_string();

    let (status, hook) = json_request(
        &app,
        "POST",
        &format!("/api/v1/pipelines/{pipeline_id}/git-hooks"),
        Some(
            r#"{
                "provider": "github",
                "label": "github-main",
                "branch_filter": "main",
                "event_filter": "push",
                "environment": "default",
                "enabled": true
            }"#
            .to_string(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{hook}");
    let hook_id = hook["data"]["id"].as_str().unwrap().to_string();
    (app, state, pipeline_id, hook_id)
}

#[tokio::test]
async fn update_changes_mutable_settings_and_audits() {
    let (app, state, pipeline_id, hook_id) = seeded_pipeline_with_hook().await;
    let uri = format!("/api/v1/pipelines/{pipeline_id}/git-hooks/{hook_id}");

    let (status, updated) = json_request(
        &app,
        "PUT",
        &uri,
        Some(
            r#"{
                "label": "github-pr-gate",
                "branch_filter": "release/*",
                "event_filter": "push,pull_request",
                "environment": "default",
                "enabled": false
            }"#
            .to_string(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    assert_eq!(updated["data"]["label"], "github-pr-gate");
    assert_eq!(updated["data"]["event_filter"], "push,pull_request");
    assert_eq!(updated["data"]["branch_filter"], "release/*");
    assert_eq!(updated["data"]["enabled"], false);
    // Immutable fields preserved.
    assert_eq!(updated["data"]["provider"], "github");
    assert_eq!(updated["data"]["has_secret"], true);

    // List reflects the update.
    let (_, list) = json_request(
        &app,
        "GET",
        &format!("/api/v1/pipelines/{pipeline_id}/git-hooks"),
        None,
    )
    .await;
    assert_eq!(list["data"]["items"][0]["label"], "github-pr-gate");
    assert_eq!(list["data"]["items"][0]["enabled"], false);

    // Audit row records the transition.
    let r = exec::fetch_optional(
        &state.pool,
        "SELECT action, after_json FROM audit_logs WHERE action = 'git_hook.update'",
        &[],
    )
    .await
    .unwrap()
    .expect("audit row");
    let row = (r.text(0), r.text(1));
    assert_eq!(row.0, "git_hook.update");
    assert!(row.1.contains("github-pr-gate"), "{}", row.1);
}

#[tokio::test]
async fn update_rejects_invalid_event_filter_and_missing_hook() {
    let (app, _state, pipeline_id, hook_id) = seeded_pipeline_with_hook().await;

    let (status, body) = json_request(
        &app,
        "PUT",
        &format!("/api/v1/pipelines/{pipeline_id}/git-hooks/{hook_id}"),
        Some(
            r#"{
                "label": "x",
                "branch_filter": "",
                "event_filter": "tag",
                "environment": "default"
            }"#
            .to_string(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(body["message"].as_str().unwrap().contains("not supported"));

    let (status, body) = json_request(
        &app,
        "PUT",
        &format!("/api/v1/pipelines/{pipeline_id}/git-hooks/no-such-hook"),
        Some(r#"{"environment": "default"}"#.to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");

    // Empty environment mirrors create's requirement.
    let (status, body) = json_request(
        &app,
        "PUT",
        &format!("/api/v1/pipelines/{pipeline_id}/git-hooks/{hook_id}"),
        Some(r#"{"environment": "  "}"#.to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
}
