//! End-to-end test for the dispatch step payload's masking contract: after
//! the retirement removal of the kernel `config.source` / signed-URL path, the claim
//! payload must NOT carry a `config_auth` block, and `log_mask_values` (the
//! generic hook the Runner consumes to redact logs) stays empty for a
//! pipeline that only used the retired purpose slot.

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

async fn test_app() -> Router {
    let pool = SqlitePoolOptions::new()
        .max_connections(4)
        .connect("sqlite::memory:")
        .await
        .expect("open in-memory db");
    run_migrations(&pool, JWT_SECRET).await.expect("migrate db");

    // Platform admin.
    sqlx::query(
        "INSERT INTO users (id, username, password_hash, display_name, is_platform_admin, is_super_admin, is_active)
         VALUES (?, ?, 'hash', ?, 1, 1, 1)",
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

    // Runner with matching label.
    sqlx::query(
        "INSERT INTO runners (id, name, register_token, labels, status, max_concurrent_jobs)
         VALUES ('runner-1', 'r1', 'tok', '[\"linux-x64\"]', 'online', 2)",
    )
    .execute(&pool)
    .await
    .expect("seed runner");

    // Pipeline with one shell step that uses the config.source purpose slot.
    sqlx::query(
        "INSERT INTO pipelines (id, name, trigger_type, runner_labels, input_defs, is_active, team_id)
         VALUES ('p1', 'dispatch-mask', 'manual', '[\"linux-x64\"]', '[]', 1, 'team-1')",
    )
    .execute(&pool)
    .await
    .expect("seed pipeline");

    // No step_secrets / pipeline_secrets — purpose slot alone is the allowlist (GHA-style).
    sqlx::query(
        "INSERT INTO tasks (id, pipeline_id, step_order, name, exec_type, command_template,
                            step_secrets, config_source, config_key_path)
         VALUES ('t1', 'p1', 0, 'echo', 'shell', 'echo hi', '', '${secrets.ossbox}', 'oss-cfg')",
    )
    .execute(&pool)
    .await
    .expect("seed task");

    // A legacy pre-retirement OSS secret row (same team), value encrypted at rest
    // like production. The claim must ignore it entirely — no signing, no
    // mask registration, and the secret value must never appear.
    let oss_json = r#"{"endpoint":"oss.example.com","bucket":"buz-test","access_key_id":"akid-test","access_key_secret":"p4ssw0rd-secret"}"#;
    let sealed = crypto::encrypt(oss_json, JWT_SECRET).expect("encrypt oss secret");
    sqlx::query(
        "INSERT INTO secrets (id, name, value, team_id, kind) VALUES ('s1', 'ossbox', ?, 'team-1', 'aliyun_oss_auth')",
    )
    .bind(sealed)
    .execute(&pool)
    .await
    .expect("seed secret");

    let state = AppState {
        pool,
        config: test_config(),
        log_broadcast: LogBroadcaster::new(),
    };
    Router::new().nest("/api/v1", api::router(state))
}

fn bearer(token: &str) -> String {
    format!("Bearer {token}")
}

fn admin_jwt() -> String {
    TokenManager::new(JWT_SECRET)
        .create_token(ADMIN_ID, Role::Admin, 1)
        .expect("mint admin jwt")
}

fn runner_jwt() -> String {
    TokenManager::new(JWT_SECRET)
        .create_token("runner-1", Role::Runner, 1)
        .expect("mint runner jwt")
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

#[tokio::test]
async fn dispatch_puts_signed_config_url_in_log_mask_values() {
    let app = test_app().await;

    // Trigger the pipeline → creates a pending job.
    let trigger = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/pipelines/p1/trigger")
                .header(AUTHORIZATION, bearer(&admin_jwt()))
                .header("Content-Type", "application/json")
                .body(Body::from(r#"{"inputs":{}}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(trigger.status(), StatusCode::OK, "trigger succeeds");

    // Runner claims the pending job → dispatch payload.
    let poll = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v1/runners/poll")
                .header(AUTHORIZATION, bearer(&runner_jwt()))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(poll.status(), StatusCode::OK, "poll succeeds");
    let body = read_body(poll).await;

    let tasks = body["data"]["tasks"].as_array().expect("tasks array");
    assert_eq!(tasks.len(), 1);

    // GHA-style build numbers flow trigger → claim metadata → BUZ_* env.
    let metadata = &body["data"]["metadata"];
    assert_eq!(metadata["run_number"], 1, "first trigger of the pipeline");
    assert_eq!(metadata["run_attempt"], 1);

    // Phase 3 lock: no signed-URL attachment, no config_auth block.
    assert!(
        tasks[0].get("config_auth").is_none(),
        "config_auth must be absent after the OSS-delivery removal: {:?}",
        tasks[0]
    );

    let mask_values = tasks[0]["log_mask_values"]
        .as_array()
        .expect("log_mask_values");
    let values: Vec<&str> = mask_values.iter().filter_map(|v| v.as_str()).collect();
    assert!(
        values.is_empty(),
        "no mask values are produced anymore (signed URLs are gone): {values:?}"
    );
    assert!(
        !body.to_string().contains("p4ssw0rd-secret"),
        "raw access key secret value never appears in the claim payload"
    );
}
