//! Ops API integration: sandbox host enrollment, config versioning with
//! If-Match, dry-run validation, rollback, two-person approval, and
//! resource-profile grants. Access control: admin or ops only.

use axum::{
    body::Body,
    http::{Request, StatusCode},
    Router,
};
use http_body_util::BodyExt;
use shared::auth::{Role, TokenManager};

use sqlx::sqlite::SqlitePoolOptions;
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
        clock: Default::default(),
        logging: Default::default(),
        marketplace_url: String::new(),
    }
}

/// App + pool seeded with admin-1 (platform admin), ops-1/ops-2 (ops), user-1
/// (regular), plus one team.
async fn test_app() -> (Router, sqlx::SqlitePool) {
    let pool = SqlitePoolOptions::new()
        .max_connections(2)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    run_migrations(&pool, JWT_SECRET).await.unwrap();
    for (id, username, admin, ops) in [
        ("admin-1", "boss", 1, 0),
        ("ops-1", "opsone", 0, 1),
        ("ops-2", "opstwo", 0, 1),
        ("user-1", "member", 0, 0),
    ] {
        sqlx::query(
            "INSERT INTO users (id, username, password_hash, display_name, is_platform_admin, is_super_admin, is_ops, is_active)
             VALUES (?, ?, 'hash', ?, ?, 0, ?, 1)",
        )
        .bind(id)
        .bind(username)
        .bind(username)
        .bind(admin)
        .bind(ops)
        .execute(&pool)
        .await
        .unwrap();
    }
    sqlx::query("INSERT INTO teams (id, name, slug) VALUES ('team-1', 'Alpha', 'alpha')")
        .execute(&pool)
        .await
        .unwrap();
    let state = AppState {
        pool: pool.clone(),
        config: test_config(),
        log_broadcast: LogBroadcaster::new(),
    };
    (Router::new().nest("/api/v1", api::router(state)), pool)
}

fn jwt(sub: &str, role: Role) -> String {
    TokenManager::new(JWT_SECRET)
        .create_token(sub, role, 24)
        .unwrap()
}

async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    bearer: Option<&str>,
    if_match: Option<&str>,
    body: Option<serde_json::Value>,
) -> (StatusCode, serde_json::Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(b) = bearer {
        builder = builder.header("Authorization", format!("Bearer {b}"));
    }
    if let Some(v) = if_match {
        builder = builder.header("If-Match", v);
    }
    let req = builder
        .header("Content-Type", "application/json")
        .body(Body::from(body.map(|b| b.to_string()).unwrap_or_default()))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json = serde_json::from_slice(&bytes).unwrap_or_default();
    (status, json)
}

fn data(json: &serde_json::Value) -> serde_json::Value {
    json.get("data").cloned().unwrap_or_default()
}

fn valid_document() -> serde_json::Value {
    serde_json::json!({
        "profiles": {
            "default": { "memory": "2g", "cpu": "2", "pids": 256, "slots": 1 },
            "large":   { "memory": "8g", "cpu": "4", "pids": 512, "slots": 4 }
        },
        "tiers": {
            "container": { "image": "repo@sha256:abc", "runtime": "" }
        },
        "network": "bridge"
    })
}

async fn enrolled_host(app: &Router) {
    let (status, _) = call(
        app,
        "POST",
        "/api/v1/ops/sandbox-hosts",
        Some(&jwt("ops-1", Role::Ops)),
        None,
        Some(serde_json::json!({
            "host_id": "host-a", "instance_id": "sandboxd-01", "max_resource_slots": 8
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "enroll failed");
}

#[tokio::test]
async fn ops_user_can_enroll_and_receives_a_one_time_token() {
    let (app, pool) = test_app().await;
    let ops = jwt("ops-1", Role::Ops);
    let (status, resp) = call(
        &app,
        "POST",
        "/api/v1/ops/sandbox-hosts",
        Some(&ops),
        None,
        Some(serde_json::json!({
            "host_id": "host-a", "instance_id": "sandboxd-01", "max_resource_slots": 8
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{resp}");
    let body = data(&resp);
    let token = body["token"].as_str().unwrap().to_string();
    assert!(token.starts_with("buzsd_"), "token prefix: {token}");
    // Only the hash is stored.
    let (stored, count): (String, i64) =
        sqlx::query_as("SELECT token_hash, COUNT(*) FROM sandbox_hosts WHERE host_id = 'host-a'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(count, 1);
    assert_ne!(stored, token);
    assert_eq!(stored, shared::api_token::hash_token(&token));
}

#[tokio::test]
async fn regular_user_is_forbidden_from_the_ops_namespace() {
    let (app, _pool) = test_app().await;
    let (status, _) = call(
        &app,
        "GET",
        "/api/v1/ops/sandbox-hosts",
        Some(&jwt("user-1", Role::User)),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    // And without any credential.
    let (status, _) = call(&app, "GET", "/api/v1/ops/sandbox-hosts", None, None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn admin_can_use_the_ops_namespace() {
    let (app, _pool) = test_app().await;
    let (status, _) = call(
        &app,
        "GET",
        "/api/v1/ops/sandbox-hosts",
        Some(&jwt("admin-1", Role::Admin)),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn config_versions_bump_and_if_match_is_enforced() {
    let (app, pool) = test_app().await;
    enrolled_host(&app).await;
    let ops = jwt("ops-1", Role::Ops);

    // v1
    let (status, resp) = call(
        &app,
        "PUT",
        "/api/v1/ops/sandbox-hosts/host-a/sandboxd-01/config",
        Some(&ops),
        None,
        Some(valid_document()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{resp}");
    assert_eq!(data(&resp)["version"], 1);
    assert_eq!(data(&resp)["pending"], false);

    // v2 (If-Match: 1 accepted)
    let (status, resp) = call(
        &app,
        "PUT",
        "/api/v1/ops/sandbox-hosts/host-a/sandboxd-01/config",
        Some(&ops),
        Some("1"),
        Some(valid_document()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{resp}");
    assert_eq!(data(&resp)["version"], 2);

    // stale If-Match → conflict
    let (status, _) = call(
        &app,
        "PUT",
        "/api/v1/ops/sandbox-hosts/host-a/sandboxd-01/config",
        Some(&ops),
        Some("1"),
        Some(valid_document()),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);

    let desired: i64 =
        sqlx::query_scalar("SELECT desired_version FROM sandbox_hosts WHERE host_id = 'host-a'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(desired, 2);
}

#[tokio::test]
async fn invalid_document_is_rejected_without_bumping_the_version() {
    let (app, pool) = test_app().await;
    enrolled_host(&app).await;
    let ops = jwt("ops-1", Role::Ops);
    let (status, _) = call(
        &app,
        "PUT",
        "/api/v1/ops/sandbox-hosts/host-a/sandboxd-01/config",
        Some(&ops),
        None,
        Some(valid_document()),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let mut bad = valid_document();
    bad["network"] = serde_json::json!("");
    bad["profiles"]["default"]["memory"] = serde_json::json!("eight-gigs");
    let (status, resp) = call(
        &app,
        "PUT",
        "/api/v1/ops/sandbox-hosts/host-a/sandboxd-01/config",
        Some(&ops),
        None,
        Some(bad),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{resp}");
    let desired: i64 =
        sqlx::query_scalar("SELECT desired_version FROM sandbox_hosts WHERE host_id = 'host-a'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(desired, 1, "invalid document must not bump the version");
}

#[tokio::test]
async fn validate_endpoint_dry_runs_without_writing() {
    let (app, pool) = test_app().await;
    enrolled_host(&app).await;
    let ops = jwt("ops-1", Role::Ops);
    let (status, resp) = call(
        &app,
        "POST",
        "/api/v1/ops/sandbox-hosts/host-a/sandboxd-01/validate",
        Some(&ops),
        None,
        Some(valid_document()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{resp}");
    assert_eq!(data(&resp)["valid"], true);
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sandbox_host_configs")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0, "validate must not write a version");
}

#[tokio::test]
async fn versions_history_and_rollback() {
    let (app, _pool) = test_app().await;
    enrolled_host(&app).await;
    let ops = jwt("ops-1", Role::Ops);
    for _ in 0..2 {
        call(
            &app,
            "PUT",
            "/api/v1/ops/sandbox-hosts/host-a/sandboxd-01/config",
            Some(&ops),
            None,
            Some(valid_document()),
        )
        .await;
    }
    let (status, resp) = call(
        &app,
        "GET",
        "/api/v1/ops/sandbox-hosts/host-a/sandboxd-01/versions",
        Some(&ops),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let body = data(&resp);
    let versions = body["versions"].as_array().unwrap();
    assert_eq!(versions.len(), 2);

    // Rollback to v1 → new version 3 with v1's content.
    let (status, resp) = call(
        &app,
        "POST",
        "/api/v1/ops/sandbox-hosts/host-a/sandboxd-01/rollback",
        Some(&ops),
        None,
        Some(serde_json::json!({ "version": 1 })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{resp}");
    assert_eq!(data(&resp)["restored_from"], 1);
    assert_eq!(data(&resp)["version"], 3);
}

#[tokio::test]
async fn approval_flow_requires_a_second_user() {
    let (app, pool) = test_app().await;
    enrolled_host(&app).await;
    sqlx::query(
        "INSERT INTO platform_settings (key, value, updated_by)
         VALUES ('sandbox_config_require_approval', 'true', 'boss')",
    )
    .execute(&pool)
    .await
    .unwrap();
    let ops1 = jwt("ops-1", Role::Ops);

    // PUT creates a pending change, no version bump.
    let (status, resp) = call(
        &app,
        "PUT",
        "/api/v1/ops/sandbox-hosts/host-a/sandboxd-01/config",
        Some(&ops1),
        None,
        Some(valid_document()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{resp}");
    assert_eq!(data(&resp)["pending"], true);
    let change_id = data(&resp)["change_id"].as_str().unwrap().to_string();
    let desired: i64 =
        sqlx::query_scalar("SELECT desired_version FROM sandbox_hosts WHERE host_id = 'host-a'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(desired, 0, "approval pending: no version yet");

    // Self-approval is refused.
    let (status, _) = call(
        &app,
        "POST",
        &format!("/api/v1/ops/sandbox-changes/{change_id}/approve"),
        Some(&ops1),
        None,
        Some(serde_json::json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);

    // A different ops user approves → version 1.
    let (status, resp) = call(
        &app,
        "POST",
        &format!("/api/v1/ops/sandbox-changes/{change_id}/approve"),
        Some(&jwt("ops-2", Role::Ops)),
        None,
        Some(serde_json::json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{resp}");
    assert_eq!(data(&resp)["version"], 1);
}

#[tokio::test]
async fn grants_crud_and_default_profile_is_implicit() {
    let (app, _pool) = test_app().await;
    let ops = jwt("ops-1", Role::Ops);

    // 'default' can never be granted (it is implicit).
    let (status, _) = call(
        &app,
        "PUT",
        "/api/v1/ops/resource-grants",
        Some(&ops),
        None,
        Some(serde_json::json!({ "team_id": "team-1", "profile": "default" })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // Unknown team rejected.
    let (status, _) = call(
        &app,
        "PUT",
        "/api/v1/ops/resource-grants",
        Some(&ops),
        None,
        Some(serde_json::json!({ "team_id": "missing", "profile": "large" })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    let (status, _) = call(
        &app,
        "PUT",
        "/api/v1/ops/resource-grants",
        Some(&ops),
        None,
        Some(serde_json::json!({ "team_id": "team-1", "profile": "large" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, resp) = call(
        &app,
        "GET",
        "/api/v1/ops/resource-grants",
        Some(&ops),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let body = data(&resp);
    let grants = body["grants"].as_array().unwrap();
    assert_eq!(grants.len(), 1);
    assert_eq!(grants[0]["profile"], "large");

    let (status, _) = call(
        &app,
        "DELETE",
        "/api/v1/ops/resource-grants/team-1/large",
        Some(&ops),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn get_host_reports_document_and_attached_runners() {
    let (app, pool) = test_app().await;
    enrolled_host(&app).await;
    // A runner reporting host_id 'host-a' counts as attached.
    let created = manager::api::runners::create_runner_record(
        &pool,
        &shared::protocol::CreateRunnerRequest {
            name: "attached-runner".into(),
            labels: None,
            team_id: None,
            execution_mode: None,
            version: None,
            max_concurrent_jobs: None,
            clean_workspace_after_job: None,
            shell_enabled: None,
            ssh_enabled: None,
            docker_enabled: None,
            wasm_enabled: None,
            ssh_host_key_policy: None,
            claim_strategy: None,
        },
    )
    .await
    .unwrap();
    sqlx::query("UPDATE runners SET host_id = 'host-a' WHERE id = ?")
        .bind(&created.id)
        .execute(&pool)
        .await
        .unwrap();

    // PUT a config so the document is non-null.
    let ops = jwt("ops-1", Role::Ops);
    let (status, _) = call(
        &app,
        "PUT",
        "/api/v1/ops/sandbox-hosts/host-a/sandboxd-01/config",
        Some(&ops),
        None,
        Some(valid_document()),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, resp) = call(
        &app,
        "GET",
        "/api/v1/ops/sandbox-hosts/host-a/sandboxd-01",
        Some(&ops),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{resp}");
    let body = data(&resp);
    assert_eq!(body["desired_version"], 1);
    assert_eq!(
        body["pushed"], false,
        "published but sandboxd has not applied it yet"
    );
    assert!(body["document"]["profiles"]["default"].is_object());
    let runners = body["attached_runners"].as_array().unwrap();
    assert!(runners.iter().any(|r| r == "attached-runner"));
}

#[tokio::test]
async fn token_rotate_replaces_the_hash() {
    let (app, pool) = test_app().await;
    enrolled_host(&app).await;
    let before: String =
        sqlx::query_scalar("SELECT token_hash FROM sandbox_hosts WHERE host_id = 'host-a'")
            .fetch_one(&pool)
            .await
            .unwrap();
    let (status, resp) = call(
        &app,
        "POST",
        "/api/v1/ops/sandbox-hosts/host-a/sandboxd-01/token/rotate",
        Some(&jwt("ops-1", Role::Ops)),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{resp}");
    assert!(data(&resp)["token"].as_str().unwrap().starts_with("buzsd_"));
    let after: String =
        sqlx::query_scalar("SELECT token_hash FROM sandbox_hosts WHERE host_id = 'host-a'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_ne!(before, after);
}

#[tokio::test]
async fn delete_removes_unbound_enrollment_and_cascades_history() {
    let (app, pool) = test_app().await;
    enrolled_host(&app).await;
    let ops = jwt("ops-1", Role::Ops);

    // Give the enrollment one config version so the cascade has something to
    // remove.
    let (status, resp) = call(
        &app,
        "PUT",
        "/api/v1/ops/sandbox-hosts/host-a/sandboxd-01/config",
        Some(&ops),
        None,
        Some(valid_document()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{resp}");

    let (status, resp) = call(
        &app,
        "DELETE",
        "/api/v1/ops/sandbox-hosts/host-a/sandboxd-01",
        Some(&ops),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{resp}");

    // Enrollment and its config history are gone.
    let (status, _) = call(
        &app,
        "GET",
        "/api/v1/ops/sandbox-hosts/host-a/sandboxd-01",
        Some(&ops),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let configs: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sandbox_host_configs
         WHERE host_id = 'host-a' AND instance_id = 'sandboxd-01'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(configs, 0);
}

#[tokio::test]
async fn delete_is_refused_while_runners_are_bound() {
    let (app, pool) = test_app().await;
    enrolled_host(&app).await;
    let ops = jwt("ops-1", Role::Ops);

    sqlx::query(
        "INSERT INTO runners (id, name, register_token, host_id, sandbox_instance_id)
         VALUES ('r-1', 'runner-a', 'tok', 'host-a', 'sandboxd-01')",
    )
    .execute(&pool)
    .await
    .unwrap();

    let (status, resp) = call(
        &app,
        "DELETE",
        "/api/v1/ops/sandbox-hosts/host-a/sandboxd-01",
        Some(&ops),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{resp}");

    // The enrollment survives the refused delete.
    let (status, _) = call(
        &app,
        "GET",
        "/api/v1/ops/sandbox-hosts/host-a/sandboxd-01",
        Some(&ops),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn ops_can_update_the_host_slot_budget() {
    let (app, pool) = test_app().await;
    enrolled_host(&app).await;
    let ops = jwt("ops-1", Role::Ops);

    let (status, resp) = call(
        &app,
        "PUT",
        "/api/v1/ops/sandbox-hosts/host-a/sandboxd-01/budget",
        Some(&ops),
        None,
        Some(serde_json::json!({ "max_resource_slots": 16 })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{resp}");
    let stored: i64 =
        sqlx::query_scalar("SELECT max_resource_slots FROM sandbox_hosts WHERE host_id = 'host-a'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(stored, 16);

    // 0 = inherit the platform default budget again.
    let (status, _) = call(
        &app,
        "PUT",
        "/api/v1/ops/sandbox-hosts/host-a/sandboxd-01/budget",
        Some(&ops),
        None,
        Some(serde_json::json!({ "max_resource_slots": 0 })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let stored: i64 =
        sqlx::query_scalar("SELECT max_resource_slots FROM sandbox_hosts WHERE host_id = 'host-a'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(stored, 0);

    // Out of range is rejected and leaves the stored value untouched.
    let (status, resp) = call(
        &app,
        "PUT",
        "/api/v1/ops/sandbox-hosts/host-a/sandboxd-01/budget",
        Some(&ops),
        None,
        Some(serde_json::json!({ "max_resource_slots": 20_000 })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{resp}");
    let stored: i64 =
        sqlx::query_scalar("SELECT max_resource_slots FROM sandbox_hosts WHERE host_id = 'host-a'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(stored, 0);

    // Unknown host pairs are 404, not silently created.
    let (status, _) = call(
        &app,
        "PUT",
        "/api/v1/ops/sandbox-hosts/host-x/sandboxd-x/budget",
        Some(&ops),
        None,
        Some(serde_json::json!({ "max_resource_slots": 4 })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "unknown pair must be 404");
}

// ── grants approval flow ────────────────────────────────────────────

async fn set_grants_approval(pool: &sqlx::SqlitePool, on: bool) {
    sqlx::query(
        "INSERT INTO platform_settings (key, value, updated_by)
         VALUES ('grants_require_approval', ?, 'boss')
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
    )
    .bind(if on { "true" } else { "false" })
    .execute(pool)
    .await
    .unwrap();
}

/// Approval on: a grant lands as a pending change (nothing applied), and
/// approving it applies the row — with blast radius surfaced. A different
/// approver is required; the requester cannot self-approve.
#[tokio::test]
async fn grants_require_approval_and_approve_applies_them() {
    let (app, pool) = test_app().await;
    set_grants_approval(&pool, true).await;
    let ops1 = jwt("ops-1", Role::Ops);
    let ops2 = jwt("ops-2", Role::Ops);

    // PUT grant: pending, not applied.
    let (status, resp) = call(
        &app,
        "PUT",
        "/api/v1/ops/resource-grants",
        Some(&ops1),
        None,
        Some(serde_json::json!({ "team_id": "team-1", "profile": "large" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{resp}");
    assert_eq!(resp["data"]["pending"], true, "{resp}");
    let change_id = resp["data"]["change_id"].as_str().unwrap().to_string();
    let applied: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM team_resource_grants WHERE team_id = 'team-1' AND profile = 'large'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(applied, 0, "grant must not apply before approval");
    let pending: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sandbox_config_changes WHERE id = ? AND status = 'pending'",
    )
    .bind(&change_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(pending, 1);

    // The requester cannot self-approve; a different ops user can.
    let (status, resp) = call(
        &app,
        "POST",
        &format!("/api/v1/ops/sandbox-changes/{change_id}/approve"),
        Some(&ops1),
        None,
        Some(serde_json::json!({ "comment": "self" })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "self-approval: {resp}");

    let (status, resp) = call(
        &app,
        "POST",
        &format!("/api/v1/ops/sandbox-changes/{change_id}/approve"),
        Some(&ops2),
        None,
        Some(serde_json::json!({ "comment": "ok" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "approve: {resp}");
    assert_eq!(resp["data"]["granted"], true, "{resp}");

    let applied: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM team_resource_grants WHERE team_id = 'team-1' AND profile = 'large'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(applied, 1, "approved grant must exist");

    let audit_actions: Vec<String> = sqlx::query_scalar(
        "SELECT action FROM audit_logs WHERE action = 'resource_grant.update' ORDER BY rowid",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert!(!audit_actions.is_empty());
}

/// Approval on: revoke takes effect immediately (fail-closed direction) but
/// leaves a pending record; rejecting that record restores the grant.
#[tokio::test]
async fn revocation_applies_immediately_and_rejection_restores_the_grant() {
    let (app, pool) = test_app().await;
    set_grants_approval(&pool, true).await;
    let ops1 = jwt("ops-1", Role::Ops);
    let ops2 = jwt("ops-2", Role::Ops);

    // Seed a grant directly (approval was off when it was granted).
    sqlx::query(
        "INSERT INTO team_resource_grants (team_id, profile, granted_by) VALUES ('team-1', 'large', 'boss')",
    )
    .execute(&pool)
    .await
    .unwrap();

    let (status, resp) = call(
        &app,
        "DELETE",
        "/api/v1/ops/resource-grants/team-1/large",
        Some(&ops1),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{resp}");
    assert_eq!(resp["data"]["pending"], true, "{resp}");
    let change_id = resp["data"]["change_id"].as_str().unwrap().to_string();

    // Fail-closed: the revoke itself already bit.
    let applied: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM team_resource_grants WHERE team_id = 'team-1' AND profile = 'large'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(applied, 0, "revocation must apply immediately");

    // Rejecting the paperwork restores the grant.
    let (status, _) = call(
        &app,
        "POST",
        &format!("/api/v1/ops/sandbox-changes/{change_id}/reject"),
        Some(&ops2),
        None,
        Some(serde_json::json!({ "comment": "was urgent, fine" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let restored: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM team_resource_grants WHERE team_id = 'team-1' AND profile = 'large'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(restored, 1, "rejection restores the grant row");
}

/// Approval off (default): grants apply directly as before.
#[tokio::test]
async fn grants_apply_directly_when_approval_is_off() {
    let (app, pool) = test_app().await;
    let ops1 = jwt("ops-1", Role::Ops);
    let (status, resp) = call(
        &app,
        "PUT",
        "/api/v1/ops/resource-grants",
        Some(&ops1),
        None,
        Some(serde_json::json!({ "team_id": "team-1", "profile": "large" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{resp}");
    assert_eq!(
        resp["data"].get("pending"),
        None,
        "no pending in direct mode"
    );
    let applied: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM team_resource_grants WHERE team_id = 'team-1' AND profile = 'large'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(applied, 1);
}
