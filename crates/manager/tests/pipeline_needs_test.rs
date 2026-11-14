//! `needs:` DAG round-trip: create persists the declared edges, get_one
//! returns them, and the trigger-time job snapshot freezes them — so the
//! Runner's claim payload can schedule concurrently even for historical jobs.

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

async fn test_app() -> (Router, exec::DbPool) {
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

    let state = AppState {
        pool: pool.clone(),
        config: test_config(),
        log_broadcast: LogBroadcaster::new(),
    };
    (Router::new().nest("/api/v1", api::router(state)), pool)
}

fn bearer() -> String {
    format!(
        "Bearer {}",
        TokenManager::new(JWT_SECRET)
            .create_token(ADMIN_ID, Role::Admin, 1)
            .expect("mint jwt")
    )
}

async fn json_response(
    app: &Router,
    method: &str,
    uri: &str,
    body: Option<String>,
) -> (StatusCode, serde_json::Value) {
    let builder = Request::builder().method(method).uri(uri);
    let request = match body {
        Some(text) => builder
            .header(AUTHORIZATION, bearer())
            .header("content-type", "application/json")
            .body(Body::from(text))
            .unwrap(),
        None => builder
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

fn create_payload() -> String {
    r#"{
        "name": "dag-roundtrip",
        "trigger_type": "manual",
        "environments": ["default"],
        "runner_labels": [],
        "team_id": "team-1",
        "steps": [
            { "name": "build", "run": "echo build", "engine": "shell" },
            { "name": "lint", "needs": ["build"], "run": "echo lint", "engine": "shell" },
            { "name": "test", "needs": ["build"], "run": "echo test", "engine": "shell" },
            { "name": "deploy", "needs": ["lint", "test"], "run": "echo deploy", "engine": "shell" }
        ]
    }"#
    .to_string()
}

#[tokio::test]
async fn needs_survive_create_get_and_trigger_snapshot() {
    let (app, _pool) = test_app().await;

    let (status, created) =
        json_response(&app, "POST", "/api/v1/pipelines", Some(create_payload())).await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let pipeline_id = created["data"]["id"]
        .as_str()
        .expect("pipeline id")
        .to_string();

    // get_one returns the edges per task.
    let (get_status, one) = json_response(
        &app,
        "GET",
        &format!("/api/v1/pipelines/{pipeline_id}"),
        None,
    )
    .await;
    assert_eq!(get_status, StatusCode::OK, "{one}");
    let tasks = one["data"]["tasks"].as_array().expect("tasks");
    let needs_of = |name: &str| -> Vec<String> {
        tasks
            .iter()
            .find(|task| task["name"] == name)
            .map(|task| {
                task["needs"]
                    .as_array()
                    .map(|list| {
                        list.iter()
                            .map(|value| value.as_str().unwrap_or_default().to_string())
                            .collect()
                    })
                    .unwrap_or_default()
            })
            .unwrap_or_default()
    };
    assert!(needs_of("build").is_empty(), "{}", one);
    assert_eq!(needs_of("lint"), vec!["build".to_string()]);
    assert_eq!(needs_of("test"), vec!["build".to_string()]);
    assert_eq!(
        needs_of("deploy"),
        vec!["lint".to_string(), "test".to_string()]
    );

    // Trigger freezes the edges into the job snapshot (the Runner's frozen
    // claim path reads exactly these).
    let (trig_status, triggered) = json_response(
        &app,
        "POST",
        &format!("/api/v1/pipelines/{pipeline_id}/trigger"),
        Some(r#"{"inputs": {}, "environment": "default"}"#.to_string()),
    )
    .await;
    assert_eq!(trig_status, StatusCode::OK, "{triggered}");
    let job_id = triggered["data"]["job_id"]
        .as_str()
        .expect("job id")
        .to_string();

    let (job_status, job) =
        json_response(&app, "GET", &format!("/api/v1/jobs/{job_id}"), None).await;
    assert_eq!(job_status, StatusCode::OK, "{job}");
    let snapshot_tasks = job["data"]["snapshot"]["tasks"]
        .as_array()
        .expect("snapshot tasks");
    let deploy = snapshot_tasks
        .iter()
        .find(|task| task["name"] == "deploy")
        .expect("deploy in snapshot");
    let deploy_needs: Vec<&str> = deploy["needs"]
        .as_array()
        .expect("needs in snapshot")
        .iter()
        .map(|value| value.as_str().expect("string"))
        .collect();
    assert_eq!(deploy_needs, vec!["lint", "test"]);
}

#[tokio::test]
async fn job_detail_reports_truncated_steps() {
    let (app, pool) = test_app().await;

    let (status, created) =
        json_response(&app, "POST", "/api/v1/pipelines", Some(create_payload())).await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let pipeline_id = created["data"]["id"].as_str().unwrap().to_string();

    let (_, triggered) = json_response(
        &app,
        "POST",
        &format!("/api/v1/pipelines/{pipeline_id}/trigger"),
        Some(r#"{"inputs": {}, "environment": "default"}"#.to_string()),
    )
    .await;
    let job_id = triggered["data"]["job_id"].as_str().unwrap().to_string();

    // No truncation reported -> empty list.
    let (_, job) = json_response(&app, "GET", &format!("/api/v1/jobs/{job_id}"), None).await;
    assert_eq!(job["data"]["truncated_steps"], serde_json::json!([]));

    // The Runner's terminal report carries the step orders that overflowed
    // (see `UpdateJobStatusRequest::truncated_steps`); the Manager stores them
    // on the job row as a union, so a DAG reporting several steps keeps them
    // all. The status endpoint itself requires Runner credentials — writing
    // the column here isolates the read side this test is about.
    exec::execute(
        &pool,
        "UPDATE jobs SET truncated_steps = ? WHERE id = ?",
        &[r#"[1,2]"#.into(), job_id.as_str().into()],
    )
    .await
    .unwrap();

    let (get_status, job) =
        json_response(&app, "GET", &format!("/api/v1/jobs/{job_id}"), None).await;
    assert_eq!(get_status, StatusCode::OK, "{job}");
    assert_eq!(
        job["data"]["truncated_steps"],
        serde_json::json!([1, 2]),
        "union of reported steps: {job}"
    );

    // The fact lives on the job row, so retention sweeping the log rows (the
    // legacy 9991 marker included) must not erase it.
    exec::execute(
        &pool,
        "DELETE FROM job_logs WHERE job_id = ?",
        &[job_id.as_str().into()],
    )
    .await
    .unwrap();
    let (_, job) = json_response(&app, "GET", &format!("/api/v1/jobs/{job_id}"), None).await;
    assert_eq!(
        job["data"]["truncated_steps"],
        serde_json::json!([1, 2]),
        "survives log retention: {job}"
    );
}

/// The union write is read-merge-write inside the status transaction. Two
/// concurrent DAG steps truncating must both survive; a late report must not
/// erase an earlier one. (The HTTP endpoint needs Runner credentials and a
/// claimed job, so the SQL shape is exercised directly here.)
#[tokio::test]
async fn truncated_steps_union_not_overwrite() {
    let (_app, pool) = test_app().await;
    exec::execute(
        &pool,
        "INSERT INTO pipelines (id, name, team_id) VALUES ('p-union', 'union', 'team-1')",
        &[],
    )
    .await
    .unwrap();
    exec::execute(
        &pool,
        "INSERT INTO jobs (id, pipeline_id, status, trigger_by)
         VALUES ('j-union', 'p-union', 'running', 'test')",
        &[],
    )
    .await
    .unwrap();

    for incoming in [vec![0i32], vec![1], vec![0, 2]] {
        let existing: String = exec::fetch_optional(
            &pool,
            "SELECT truncated_steps FROM jobs WHERE id = 'j-union'",
            &[],
        )
        .await
        .unwrap()
        .unwrap()
        .text(0);
        let mut merged: Vec<i32> = serde_json::from_str(&existing).unwrap_or_default();
        merged.extend(incoming);
        merged.sort_unstable();
        merged.dedup();
        exec::execute(
            &pool,
            "UPDATE jobs SET truncated_steps = ? WHERE id = 'j-union'",
            &[serde_json::to_string(&merged).unwrap().into()],
        )
        .await
        .unwrap();
    }

    let stored: String = exec::fetch_optional(
        &pool,
        "SELECT truncated_steps FROM jobs WHERE id = 'j-union'",
        &[],
    )
    .await
    .unwrap()
    .unwrap()
    .text(0);
    assert_eq!(stored, "[0,1,2]");
}
