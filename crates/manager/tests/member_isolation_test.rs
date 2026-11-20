//! Least-privilege regression suite for plain members (developer on team-1).
//!
//! Pins the resource surface verified in the 2026-11-20 audit:
//! - lists and direct reads are team-scoped;
//! - platform-admin surfaces and privileged creates are refused;
//! - platform variables cannot be bound by non-admins (their values are
//!   admin-only but were previously injected into any pipeline that bound them);
//! - environment metadata (secret names / host allowlist) is admin-only even
//!   though environments themselves are global.

mod common;

use axum::http::StatusCode;
use common::{data, TestApp};
use manager::store::exec;
use serde_json::{json, Value};

async fn seed_other_team(app: &TestApp) {
    for sql in [
        "INSERT INTO teams (id, name, slug) VALUES ('team-2', 'Beta', 'beta')",
        "INSERT INTO pipelines (id, name, team_id) VALUES ('p2', 'beta-pipe', 'team-2')",
        "INSERT INTO secrets (id, name, value, owner_group, team_id) VALUES ('s2', 'BETA_SECRET', 'x', 'beta', 'team-2')",
        "INSERT INTO hosts (id, name, host, username, team_id) VALUES ('h2', 'beta-host', '10.0.0.9', 'root', 'team-2')",
        "INSERT INTO environments (id, name, slug) VALUES ('env-1', 'Production', 'production')",
        "INSERT INTO environment_secret_overrides (environment_id, secret_id, value) VALUES ('env-1', 's2', 'x')",
        "INSERT INTO environment_hosts (environment_id, kind, value) VALUES ('env-1', 'name', 'beta-host')",
        "INSERT INTO platform_vars (id, name, value) VALUES ('v1', 'PLATFORM_VAR', 'super-secret')",
        "INSERT INTO jobs (id, pipeline_id, status) VALUES ('j2', 'p2', 'success')",
        "INSERT INTO job_logs (id, job_id, chunk_index, log_content) VALUES ('l2', 'j2', 0, 'beta log line')",
    ] {
        exec::execute(&app.pool, sql, &[]).await.expect(sql);
    }
}

fn ids(resp: &Value) -> Vec<String> {
    data(resp)
        .as_array()
        .expect("array payload")
        .iter()
        .filter_map(|row| row["id"].as_str().map(str::to_string))
        .collect()
}

#[tokio::test]
async fn lists_and_reads_are_team_scoped() {
    let app = TestApp::boot().await;
    seed_other_team(&app).await;
    let member = app.member_jwt();

    for (uri, forbidden) in [
        ("/api/v1/pipelines", "p2"),
        ("/api/v1/secrets", "s2"),
        ("/api/v1/hosts", "h2"),
        ("/api/v1/jobs", "j2"),
    ] {
        let (status, resp) = app.get(uri, Some(&member)).await;
        assert_eq!(status, StatusCode::OK, "{uri}: {resp}");
        assert!(
            !ids(&resp).iter().any(|id| id == forbidden),
            "{uri} leaked {forbidden}: {resp}"
        );
    }

    let (status, teams) = app.get("/api/v1/teams", Some(&member)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(ids(&teams), vec!["team-1".to_string()], "only own teams");

    // Direct reads of another team's resources fail closed.
    for uri in [
        "/api/v1/pipelines/p2",
        "/api/v1/secrets/s2",
        "/api/v1/hosts/h2",
        "/api/v1/teams/team-2",
        "/api/v1/teams/team-2/members",
        "/api/v1/jobs/j2",
        "/api/v1/jobs/j2/logs",
        "/api/v1/pipelines/p2/trigger-events",
    ] {
        let (status, resp) = app.get(uri, Some(&member)).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{uri} must be 403: {resp}");
    }

    // Platform-admin surfaces.
    for uri in [
        "/api/v1/users",
        "/api/v1/audit-logs",
        "/api/v1/github-sync/connections",
        "/api/v1/runners",
        "/api/v1/api-tokens",
    ] {
        let (status, resp) = app.get(uri, Some(&member)).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{uri} must be 403: {resp}");
    }
}

#[tokio::test]
async fn privileged_creates_are_refused_but_own_team_pipeline_is_allowed() {
    let app = TestApp::boot().await;
    seed_other_team(&app).await;
    let member = app.member_jwt();

    for (uri, body) in [
        (
            "/api/v1/secrets",
            json!({"name": "MEMBER_SECRET", "value": "x", "team_id": "team-1"}),
        ),
        (
            "/api/v1/hosts",
            json!({"name": "member-host", "host": "127.0.0.1", "username": "u", "team_id": "team-1"}),
        ),
        (
            "/api/v1/variables",
            json!({"name": "MEMBER_VAR", "value": "x"}),
        ),
        (
            "/api/v1/environments",
            json!({"name": "Member Env", "slug": "member-env"}),
        ),
        (
            "/api/v1/teams",
            json!({"name": "Member Team", "slug": "member-team"}),
        ),
        ("/api/v1/api-tokens", json!({"name": "member-token"})),
    ] {
        let (status, resp) = app.post(uri, Some(&member), body).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{uri} must be 403: {resp}");
    }

    // The role matrix lets a developer create pipelines in their own team.
    let own = json!({
        "name": "member-pipe", "trigger_type": "manual", "runner_labels": ["linux-x64"],
        "team_id": "team-1", "environments": ["default"], "env": [],
        "steps": [{ "name": "echo", "run": "echo hi", "engine": "shell" }]
    });
    let (status, resp) = app.post("/api/v1/pipelines", Some(&member), own).await;
    assert_eq!(status, StatusCode::OK, "own-team pipeline: {resp}");

    let cross = json!({
        "name": "cross-pipe", "trigger_type": "manual", "runner_labels": ["linux-x64"],
        "team_id": "team-2", "environments": ["default"], "env": [],
        "steps": [{ "name": "echo", "run": "echo hi", "engine": "shell" }]
    });
    let (status, resp) = app.post("/api/v1/pipelines", Some(&member), cross).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "cross-team pipeline: {resp}");

    let (status, resp) = app
        .post("/api/v1/pipelines/p2/trigger", Some(&member), json!({}))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "cross-team trigger: {resp}");

    // Cross-team member management.
    let (status, resp) = app
        .put(
            "/api/v1/teams/team-2/members",
            Some(&member),
            json!({"user_id": "user-1", "role": "owner"}),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{resp}");
    let (status, _) = app.delete("/api/v1/teams/team-2", Some(&member)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn platform_variables_cannot_be_bound_by_members() {
    let app = TestApp::boot().await;
    seed_other_team(&app).await;
    let member = app.member_jwt();

    let body = json!({
        "name": "var-pipe", "trigger_type": "manual", "runner_labels": ["linux-x64"],
        "team_id": "team-1", "environments": ["default"], "env": [],
        "var_ids": ["v1"],
        "steps": [{ "name": "echo", "run": "echo hi", "engine": "shell" }]
    });
    let (status, resp) = app
        .post("/api/v1/pipelines", Some(&member), body.clone())
        .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "member must not bind platform vars: {resp}"
    );

    let pipeline_rows = exec::fetch_scalar_i64(
        &app.pool,
        "SELECT COUNT(*) FROM pipelines WHERE name = 'var-pipe'",
        &[],
    )
    .await
    .unwrap();
    let binding_rows = exec::fetch_scalar_i64(
        &app.pool,
        "SELECT COUNT(*) FROM pipeline_vars WHERE var_id = 'v1'",
        &[],
    )
    .await
    .unwrap();
    assert_eq!(pipeline_rows, 0, "rejected create must not persist");
    assert_eq!(binding_rows, 0, "rejected create must not bind");

    // An admin may still bind it.
    let admin = app.admin_jwt();
    let (status, resp) = app.post("/api/v1/pipelines", Some(&admin), body).await;
    assert_eq!(status, StatusCode::OK, "admin binding: {resp}");
    let bound = exec::fetch_scalar_i64(
        &app.pool,
        "SELECT COUNT(*) FROM pipeline_vars WHERE var_id = 'v1'",
        &[],
    )
    .await
    .unwrap();
    assert_eq!(bound, 1);
}

#[tokio::test]
async fn environment_metadata_is_admin_only() {
    let app = TestApp::boot().await;
    seed_other_team(&app).await;
    let member = app.member_jwt();
    let admin = app.admin_jwt();

    // The environment itself is global (trigger dropdown), but the metadata
    // that names another team's secret/host must not be.
    let (status, resp) = app.get("/api/v1/environments/env-1", Some(&member)).await;
    assert_eq!(status, StatusCode::OK, "{resp}");
    let detail = data(&resp);
    assert_eq!(detail["name"], "Production");
    assert!(
        detail["secret_overrides"]
            .as_array()
            .is_some_and(|v| v.is_empty()),
        "member must not see secret overrides: {detail}"
    );
    assert!(
        detail["host_allowlist"]
            .as_array()
            .is_some_and(|v| v.is_empty()),
        "member must not see host allowlist: {detail}"
    );
    let text = detail.to_string();
    assert!(!text.contains("BETA_SECRET"), "secret name leaked: {text}");
    assert!(!text.contains("beta-host"), "host name leaked: {text}");

    for uri in [
        "/api/v1/environments/env-1/secret-overrides",
        "/api/v1/environments/env-1/host-allowlist",
    ] {
        let (status, resp) = app.get(uri, Some(&member)).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{uri} must be 403: {resp}");
    }

    // Admins keep the full working view.
    let (status, resp) = app.get("/api/v1/environments/env-1", Some(&admin)).await;
    assert_eq!(status, StatusCode::OK);
    let detail = data(&resp);
    assert_eq!(
        detail["secret_overrides"][0]["secret_name"].as_str(),
        Some("BETA_SECRET")
    );
    assert_eq!(
        detail["host_allowlist"][0]["value"].as_str(),
        Some("beta-host")
    );
    let (status, resp) = app
        .get("/api/v1/environments/env-1/secret-overrides", Some(&admin))
        .await;
    assert_eq!(status, StatusCode::OK, "{resp}");
}
