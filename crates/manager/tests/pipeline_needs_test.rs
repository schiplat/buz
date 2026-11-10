//! `needs:` DAG round-trip: create persists the declared edges, get_one
//! returns them, and the trigger-time job snapshot freezes them — so the
//! Runner's claim payload can schedule concurrently even for historical jobs.

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
use manager::config::{AuthConfig, Config, PipelineConfig, RunnerConfig, ServerConfig};
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
const ADMIN_ID: &str = "admin-1";

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

async fn test_app() -> (Router, sqlx::SqlitePool) {
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

    // No truncation marker -> empty list.
    let (_, job) = json_response(&app, "GET", &format!("/api/v1/jobs/{job_id}"), None).await;
    assert_eq!(job["data"]["truncated_steps"], serde_json::json!([]));

    // Simulate a streamed step that overflowed: chunk_index 6 (stream) and
    // 9991 (truncation marker) within step 1's 10_000-wide window.
    for (chunk, content) in [(6, "output"), (9991, "==> OUTPUT TRUNCATED")] {
        sqlx::query(
            "INSERT INTO job_logs (id, job_id, chunk_index, step_order, log_content)
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(format!("log-{chunk}"))
        .bind(&job_id)
        .bind(chunk)
        .bind(1)
        .bind(content)
        .execute(&pool)
        .await
        .unwrap();
    }
    // The runner banner (-2) never matches; a marker inside step 2's own
    // window (20000 + 9991 = 19991) is step 2's truncation, so both flag.
    for (chunk, step) in [(-2, 0), (19991, 2)] {
        sqlx::query(
            "INSERT INTO job_logs (id, job_id, chunk_index, step_order, log_content)
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(format!("log-x-{chunk}"))
        .bind(&job_id)
        .bind(chunk)
        .bind(step)
        .bind("x")
        .execute(&pool)
        .await
        .unwrap();
    }

    let (_, job) = json_response(&app, "GET", &format!("/api/v1/jobs/{job_id}"), None).await;
    assert_eq!(
        job["data"]["truncated_steps"],
        serde_json::json!([1, 2]),
        "{job}"
    );
}
