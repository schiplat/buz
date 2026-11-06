//! sandboxd config delivery: per-instance token auth, immediate return on a
//! version mismatch, long-poll that wakes on an ops change, status reporting,
//! and the database-restore reconciliation guard.

use axum::{
    body::Body,
    http::{Request, StatusCode},
    Router,
};
use http_body_util::BodyExt;
use shared::auth::{Role, TokenManager};
use sqlx::sqlite::SqlitePoolOptions;
use std::time::{Duration, Instant};
use tower::ServiceExt;

use manager::api;
use manager::config::{
    AuthConfig, Config, PipelineConfig, RunnerConfig, ServerConfig, StandaloneConfig,
};
use manager::db::run_migrations;
use manager::services::log_broadcast::LogBroadcaster;
use manager::state::AppState;

const JWT_SECRET: &str = "test-secret-0123456789-abcdefghijklmnop";

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
        standalone: StandaloneConfig {
            enabled: false,
            auto_approve: vec![],
        },
        plugin_index: Default::default(),
        storage: Default::default(),
        logging: Default::default(),
        marketplace_url: String::new(),
    }
}

/// App + pool with an ops user and one enrolled host; returns the sandboxd token.
async fn app_with_host() -> (Router, sqlx::SqlitePool, String) {
    let pool = SqlitePoolOptions::new()
        .max_connections(4)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    run_migrations(&pool, JWT_SECRET).await.unwrap();
    sqlx::query(
        "INSERT INTO users (id, username, password_hash, display_name, is_platform_admin, is_super_admin, is_ops, is_active)
         VALUES ('ops-1', 'opsone', 'hash', 'opsone', 0, 0, 1, 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    let state = AppState {
        pool: pool.clone(),
        config: test_config(),
        log_broadcast: LogBroadcaster::new(),
    };
    let app = Router::new().nest("/api/v1", api::router(state));

    let (status, resp) = call(
        &app,
        "POST",
        "/api/v1/ops/sandbox-hosts",
        Some(&ops_jwt()),
        None,
        Some(serde_json::json!({
            "host_id": "host-a", "instance_id": "sandboxd-01", "max_resource_slots": 8
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "enroll: {resp}");
    let token = resp["data"]["token"].as_str().unwrap().to_string();
    (app, pool, token)
}

fn ops_jwt() -> String {
    TokenManager::new(JWT_SECRET)
        .create_token("ops-1", Role::Ops, 24)
        .unwrap()
}

async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    bearer: Option<&str>,
    prefer: Option<&str>,
    body: Option<serde_json::Value>,
) -> (StatusCode, serde_json::Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(b) = bearer {
        builder = builder.header("Authorization", format!("Bearer {b}"));
    }
    if let Some(p) = prefer {
        builder = builder.header("Prefer", p);
    }
    let req = builder
        .header("Content-Type", "application/json")
        .body(Body::from(body.map(|b| b.to_string()).unwrap_or_default()))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or_default())
}

fn valid_document() -> serde_json::Value {
    serde_json::json!({
        "profiles": {
            "default": { "memory": "2g", "cpu": "2", "pids": 256, "slots": 1 }
        },
        "tiers": {},
        "network": "bridge"
    })
}

async fn put_config(app: &Router) -> i64 {
    let (status, resp) = call(
        app,
        "PUT",
        "/api/v1/ops/sandbox-hosts/host-a/sandboxd-01/config",
        Some(&ops_jwt()),
        None,
        Some(valid_document()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "put: {resp}");
    resp["data"]["version"].as_i64().unwrap()
}

#[tokio::test]
async fn rejects_non_sandboxd_credentials() {
    let (app, _pool, _token) = app_with_host().await;
    let (status, _) = call(&app, "GET", "/api/v1/sandboxd/config", None, None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // A user JWT is the wrong credential type for this namespace.
    let (status, _) = call(
        &app,
        "GET",
        "/api/v1/sandboxd/config",
        Some(&ops_jwt()),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    // A well-formed but unknown token.
    let (status, _) = call(
        &app,
        "GET",
        "/api/v1/sandboxd/config",
        Some("buzsd_deadbeef"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn first_fetch_returns_the_current_document_immediately() {
    let (app, _pool, token) = app_with_host().await;
    put_config(&app).await;
    let started = Instant::now();
    let (status, resp) = call(
        &app,
        "GET",
        "/api/v1/sandboxd/config",
        Some(&token),
        Some("wait=30"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{resp}");
    assert!(started.elapsed() < Duration::from_secs(2), "must not block");
    let data = &resp["data"];
    assert_eq!(data["version"], 1);
    assert_eq!(data["changed"], true);
    assert_eq!(data["host_id"], "host-a");
    assert_eq!(data["sandboxd_instance_id"], "sandboxd-01");
    assert!(data["config"]["profiles"]["default"].is_object());
    assert_eq!(data["sha256"].as_str().unwrap().len(), 64);
}

#[tokio::test]
async fn up_to_date_fetch_holds_until_timeout_and_reports_unchanged() {
    let (app, _pool, token) = app_with_host().await;
    put_config(&app).await;
    let started = Instant::now();
    let (status, resp) = call(
        &app,
        "GET",
        "/api/v1/sandboxd/config?applied_version=1",
        Some(&token),
        Some("wait=1"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{resp}");
    assert!(
        started.elapsed() >= Duration::from_millis(900),
        "should have waited ~1s, took {:?}",
        started.elapsed()
    );
    assert_eq!(resp["data"]["changed"], false);
    assert!(
        resp["data"]["config"].is_null(),
        "unchanged carries no document"
    );
}

#[tokio::test]
async fn long_poll_wakes_when_ops_publishes_a_new_version() {
    let (app, _pool, token) = app_with_host().await;
    put_config(&app).await; // v1

    let waiter_app = app.clone();
    let waiter_token = token.clone();
    let waiter = tokio::spawn(async move {
        let started = Instant::now();
        let (status, resp) = call(
            &waiter_app,
            "GET",
            "/api/v1/sandboxd/config?applied_version=1",
            Some(&waiter_token),
            Some("wait=20"),
            None,
        )
        .await;
        (status, resp, started.elapsed())
    });

    // Give the waiter time to subscribe, then publish v2.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let v2 = put_config(&app).await;
    assert_eq!(v2, 2);

    let (status, resp, elapsed) = waiter.await.unwrap();
    assert_eq!(status, StatusCode::OK, "{resp}");
    assert!(
        elapsed < Duration::from_secs(5),
        "long-poll should wake promptly, took {elapsed:?}"
    );
    assert_eq!(resp["data"]["version"], 2);
    assert_eq!(resp["data"]["changed"], true);
}

#[tokio::test]
async fn status_report_updates_applied_state() {
    let (app, pool, token) = app_with_host().await;
    put_config(&app).await;
    let (status, _) = call(
        &app,
        "POST",
        "/api/v1/sandboxd/status",
        Some(&token),
        None,
        Some(serde_json::json!({
            "applied_version": 1,
            "applied_sha256": "abc",
            "last_error": "",
            "consecutive_failures": 0
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (applied, host_status, err): (i64, String, Option<String>) = sqlx::query_as(
        "SELECT applied_version, status, last_fetch_error FROM sandbox_hosts WHERE host_id = 'host-a'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(applied, 1);
    assert_eq!(host_status, "insync");
    assert!(err.is_none());

    // An error report is surfaced and the host falls to pending when behind.
    put_config(&app).await; // v2 desired
    let (status, _) = call(
        &app,
        "POST",
        "/api/v1/sandboxd/status",
        Some(&token),
        None,
        Some(serde_json::json!({
            "applied_version": 1,
            "last_error": "docker: image not found",
            "consecutive_failures": 3
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (host_status, err, failures): (String, Option<String>, i64) = sqlx::query_as(
        "SELECT status, last_fetch_error, consecutive_fetch_failures FROM sandbox_hosts WHERE host_id = 'host-a'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(host_status, "pending");
    assert_eq!(err.as_deref(), Some("docker: image not found"));
    assert_eq!(failures, 3);
}

#[tokio::test]
async fn applied_at_is_the_push_date_and_survives_heartbeats() {
    let (app, pool, token) = app_with_host().await;
    put_config(&app).await;

    // The first report of v1 lands the push date.
    let (status, _) = call(
        &app,
        "POST",
        "/api/v1/sandboxd/status",
        Some(&token),
        None,
        Some(serde_json::json!({ "applied_version": 1, "applied_sha256": "abc" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let pushed_at: String =
        sqlx::query_scalar("SELECT applied_at FROM sandbox_hosts WHERE host_id = 'host-a'")
            .fetch_one(&pool)
            .await
            .unwrap();

    // Heartbeats of the SAME version must never rewrite when it landed.
    // datetime('now') has 1s resolution, hence the sleep.
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let (status, _) = call(
        &app,
        "POST",
        "/api/v1/sandboxd/status",
        Some(&token),
        None,
        Some(serde_json::json!({ "applied_version": 1, "applied_sha256": "abc" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let same: String =
        sqlx::query_scalar("SELECT applied_at FROM sandbox_hosts WHERE host_id = 'host-a'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(same, pushed_at, "same-version reports keep the push date");

    // The ops API marks the host pushed once applied == desired, with the
    // push date visible.
    let (status, resp) = call(
        &app,
        "GET",
        "/api/v1/ops/sandbox-hosts/host-a/sandboxd-01",
        Some(&ops_jwt()),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{resp}");
    assert_eq!(resp["data"]["pushed"], true);
    assert_eq!(resp["data"]["applied_at"], pushed_at);

    // A newly published version flips to not-pushed until the host applies it.
    put_config(&app).await; // v2 desired
    let (status, resp) = call(
        &app,
        "GET",
        "/api/v1/ops/sandbox-hosts/host-a/sandboxd-01",
        Some(&ops_jwt()),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{resp}");
    assert_eq!(resp["data"]["pushed"], false);

    // Its first report records a NEW push date.
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let (status, _) = call(
        &app,
        "POST",
        "/api/v1/sandboxd/status",
        Some(&token),
        None,
        Some(serde_json::json!({ "applied_version": 2, "applied_sha256": "def" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let next: String =
        sqlx::query_scalar("SELECT applied_at FROM sandbox_hosts WHERE host_id = 'host-a'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_ne!(next, pushed_at, "a new version records a new push date");
}

#[tokio::test]
async fn db_restore_bumps_desired_past_a_higher_applied_version() {
    let (app, pool, token) = app_with_host().await;
    put_config(&app).await; // desired = 1

    // Simulate a restored database: the host says it already applied v7.
    let (status, resp) = call(
        &app,
        "GET",
        "/api/v1/sandboxd/config?applied_version=7",
        Some(&token),
        Some("wait=0"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{resp}");
    // The Manager must never serve a version <= 7 again; it bumps to 8 and
    // re-serves the last stored document under that version.
    assert_eq!(resp["data"]["version"], 8);
    assert_eq!(resp["data"]["changed"], true);
    assert!(resp["data"]["config"]["profiles"]["default"].is_object());

    let (desired, applied): (i64, i64) = sqlx::query_as(
        "SELECT desired_version, applied_version FROM sandbox_hosts WHERE host_id = 'host-a'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(desired, 8);
    assert_eq!(applied, 7);
    let history: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sandbox_host_configs WHERE host_id = 'host-a' AND version = 8",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(history, 1, "reconciled version is recorded in history");
}

#[tokio::test]
async fn rotated_token_invalidates_the_old_one() {
    let (app, _pool, token) = app_with_host().await;
    let (status, resp) = call(
        &app,
        "POST",
        "/api/v1/ops/sandbox-hosts/host-a/sandboxd-01/token/rotate",
        Some(&ops_jwt()),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let new_token = resp["data"]["token"].as_str().unwrap().to_string();

    let (status, _) = call(
        &app,
        "GET",
        "/api/v1/sandboxd/config",
        Some(&token),
        None,
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "old token must be rejected"
    );
    let (status, _) = call(
        &app,
        "GET",
        "/api/v1/sandboxd/config",
        Some(&new_token),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "new token must work");
}

const DEFAULT_IMAGE: &str =
    "ghcr.io/buz/envelope@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

#[tokio::test]
async fn platform_default_tiers_fill_undefined_tiers_and_propagate() {
    let (app, pool, token) = app_with_host().await;
    put_config(&app).await;
    let before: i64 =
        sqlx::query_scalar("SELECT desired_version FROM sandbox_hosts WHERE host_id = 'host-a'")
            .fetch_one(&pool)
            .await
            .unwrap();

    let (status, resp) = call(
        &app,
        "PUT",
        "/api/v1/ops/sandbox-defaults",
        Some(&ops_jwt()),
        None,
        Some(serde_json::json!({
            "tiers": { "container": { "image": DEFAULT_IMAGE, "runtime": "" } },
            "profiles": {
                "default": { "memory": "2g", "cpu": "2", "pids": 256, "slots": 1 },
                "small": { "memory": "2g", "cpu": "2", "pids": 256, "slots": 1 }
            }
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{resp}");
    assert_eq!(
        resp["data"]["bumped_hosts"].as_array().unwrap().len(),
        1,
        "the host inherits the tier, so its effective document changed"
    );

    // The desired version moved, which is what wakes the long-poll.
    let after: i64 =
        sqlx::query_scalar("SELECT desired_version FROM sandbox_hosts WHERE host_id = 'host-a'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(after > before, "desired version must advance");

    // sandboxd is served the effective document with the inherited tier.
    let (status, resp) = call(
        &app,
        "GET",
        "/api/v1/sandboxd/config",
        Some(&token),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{resp}");
    assert_eq!(
        resp["data"]["config"]["tiers"]["container"]["image"],
        DEFAULT_IMAGE
    );
}

#[tokio::test]
async fn host_defined_tiers_win_and_skip_the_bump() {
    let (app, pool, token) = app_with_host().await;
    let mut document = valid_document();
    document["tiers"] = serde_json::json!({
        "container": { "image": "ghcr.io/buz/own@sha256:abcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcd", "runtime": "" }
    });
    let (status, resp) = call(
        &app,
        "PUT",
        "/api/v1/ops/sandbox-hosts/host-a/sandboxd-01/config",
        Some(&ops_jwt()),
        None,
        Some(document),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{put:?}", put = resp);
    let before: i64 =
        sqlx::query_scalar("SELECT desired_version FROM sandbox_hosts WHERE host_id = 'host-a'")
            .fetch_one(&pool)
            .await
            .unwrap();

    let (status, resp) = call(
        &app,
        "PUT",
        "/api/v1/ops/sandbox-defaults",
        Some(&ops_jwt()),
        None,
        Some(serde_json::json!({
            "tiers": { "container": { "image": DEFAULT_IMAGE, "runtime": "" } },
            // Identical to the built-in floor, so the profile dimension is
            // bump-neutral and the test isolates the host-defined tier path.
            "profiles": {
                "default": { "memory": "2g", "cpu": "2", "pids": 256, "slots": 1 },
                "small": { "memory": "2g", "cpu": "2", "pids": 256, "slots": 1 },
                "medium": { "memory": "4g", "cpu": "4", "pids": 512, "slots": 2 },
                "large": { "memory": "8g", "cpu": "8", "pids": 1024, "slots": 4 }
            }
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{resp}");
    assert!(resp["data"]["bumped_hosts"].as_array().unwrap().is_empty());

    let after: i64 =
        sqlx::query_scalar("SELECT desired_version FROM sandbox_hosts WHERE host_id = 'host-a'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(before, after, "no bump when the host defines the tier");

    // The host's own definition is delivered, not the default.
    let (status, resp) = call(
        &app,
        "GET",
        "/api/v1/sandboxd/config",
        Some(&token),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{resp}");
    assert!(resp["data"]["config"]["tiers"]["container"]["image"]
        .as_str()
        .unwrap()
        .ends_with("abcd"));
}

#[tokio::test]
async fn unpinned_default_tier_is_rejected() {
    let (app, _pool, _token) = app_with_host().await;
    let (status, resp) = call(
        &app,
        "PUT",
        "/api/v1/ops/sandbox-defaults",
        Some(&ops_jwt()),
        None,
        Some(serde_json::json!({
            "tiers": { "container": { "image": "ghcr.io/buz/envelope:latest", "runtime": "" } }
        })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{resp}");
}

#[tokio::test]
async fn unknown_default_tier_name_is_rejected() {
    let (app, _pool, _token) = app_with_host().await;
    let (status, resp) = call(
        &app,
        "PUT",
        "/api/v1/ops/sandbox-defaults",
        Some(&ops_jwt()),
        None,
        Some(serde_json::json!({
            "tiers": { "chroot": { "image": DEFAULT_IMAGE, "runtime": "" } }
        })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{resp}");
}
