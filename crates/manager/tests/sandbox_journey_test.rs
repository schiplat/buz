//! One full sandbox-fleet journey across the Manager API, composing the steps
//! the per-feature suites assert in isolation:
//! enroll → publish platform defaults → sandboxd fetch/report → grant gate at
//! trigger → claim carries the profile → revoke fails the queued job → the
//! audit trail records every step.

mod common;

use axum::http::StatusCode;
use common::{data, TestApp, MEMBER, RUNNER, TEAM};
use serde_json::{json, Value};
use shared::auth::Role;

/// The standard ladder the Platform defaults dialog saves; `default` aliases
/// `small` and is written in lockstep by the UI.
fn ladder_profiles() -> Value {
    json!({
        "default": { "memory": "2g", "cpu": "2", "pids": 256, "slots": 1 },
        "small":   { "memory": "2g", "cpu": "2", "pids": 256, "slots": 1 },
        "medium":  { "memory": "4g", "cpu": "4", "pids": 512, "slots": 2 },
        "large":   { "memory": "8g", "cpu": "8", "pids": 1024, "slots": 4 }
    })
}

/// Create a pipeline through the API, then set `resource_profile` the way the
/// YAML scanner does for `runner.resources`.
async fn pipeline_with_profile(app: &TestApp, name: &str, profile: &str) -> String {
    let (status, created) = app
        .post(
            "/api/v1/pipelines",
            Some(&app.admin_jwt()),
            json!({
                "name": name,
                "trigger_type": "manual",
                "environments": ["default"],
                "runner_labels": ["linux-x64"],
                "team_id": TEAM,
                "env": [],
                "steps": [{ "name": "echo", "run": "echo hi", "engine": "shell" }]
            }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "create pipeline: {created}");
    let id = data(&created)["id"].as_str().unwrap().to_string();
    sqlx::query("UPDATE pipelines SET resource_profile = ? WHERE id = ?")
        .bind(profile)
        .bind(&id)
        .execute(&app.pool)
        .await
        .unwrap();
    id
}

async fn trigger(app: &TestApp, pipeline_id: &str) -> (StatusCode, Value) {
    app.post(
        &format!("/api/v1/pipelines/{pipeline_id}/trigger"),
        Some(&app.jwt(MEMBER, Role::User)),
        json!({"environment": "default", "inputs": {}}),
    )
    .await
}

async fn poll(app: &TestApp) -> (StatusCode, Value) {
    app.get("/api/v1/runners/poll", Some(&app.jwt(RUNNER, Role::Runner)))
        .await
}

#[tokio::test]
async fn enroll_publish_fetch_grant_trigger_claim_revoke_audits() {
    let app = TestApp::boot().await;
    let ops = app.ops_jwt();

    // ── 1. Enroll: a one-time buzsd_ token, only its hash is stored. ──
    let (status, resp) = app
        .post(
            "/api/v1/ops/sandbox-hosts",
            Some(&ops),
            json!({
                "host_id": "host-a",
                "instance_id": "sandboxd-01",
                "max_resource_slots": 8
            }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "enroll: {resp}");
    let device_token = data(&resp)["token"].as_str().unwrap().to_string();
    assert!(
        device_token.starts_with("buzsd_"),
        "token prefix: {device_token}"
    );
    let (stored_hash, hosts): (String, i64) =
        sqlx::query_as("SELECT token_hash, COUNT(*) FROM sandbox_hosts WHERE host_id = 'host-a'")
            .fetch_one(&app.pool)
            .await
            .unwrap();
    assert_eq!(hosts, 1);
    assert_ne!(stored_hash, device_token, "only the hash may be stored");
    assert_eq!(stored_hash, shared::api_token::hash_token(&device_token));

    // ── 2. Publish platform defaults: applies directly (approval off) and
    //       bumps the enrolled host to a new desired version. ──
    let (status, resp) = app
        .put(
            "/api/v1/ops/sandbox-defaults",
            Some(&app.admin_jwt()),
            json!({
                "tiers": { "container": { "image": "repo@sha256:abc", "runtime": "" } },
                "profiles": ladder_profiles(),
                "network": "bridge",
                "max_slots": 8
            }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "publish defaults: {resp}");
    let bumped = data(&resp)["bumped_hosts"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        !bumped.is_empty(),
        "the enrolled host must be bumped to a new desired version: {resp}"
    );

    // ── 3. sandboxd fetch: the effective document carries the ladder — the
    //       host document stays bare and inherits the catalog — and a
    //       caught-up long-poll reports unchanged. ──
    let (status, resp) = app
        .get("/api/v1/sandboxd/config", Some(&device_token))
        .await;
    assert_eq!(status, StatusCode::OK, "sandboxd fetch: {resp}");
    let fetched = data(&resp);
    let version = fetched["version"].as_i64().unwrap();
    assert!(version >= 1);
    assert_eq!(fetched["changed"], true);
    assert!(fetched["config"]["profiles"]["small"].is_object());
    assert!(fetched["config"]["profiles"]["default"].is_object());
    assert!(fetched["config"]["profiles"]["medium"].is_object());
    assert!(fetched["config"]["tiers"]["container"].is_object());
    let sha = fetched["sha256"].as_str().unwrap().to_string();
    // An up-to-date fetch answers immediately (no `Prefer: wait` needed);
    // the blocking long-poll paths are covered in sandboxd_config_test.
    let (status, resp) = app
        .get(
            &format!("/api/v1/sandboxd/config?applied_version={version}"),
            Some(&device_token),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(data(&resp)["changed"], false, "up to date: {resp}");

    // sandboxd reports the applied version — the host turns insync.
    let (status, _) = app
        .post(
            "/api/v1/sandboxd/status",
            Some(&device_token),
            json!({
                "applied_version": version,
                "applied_sha256": sha,
                "last_error": "",
                "consecutive_failures": 0
            }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let host_status: String =
        sqlx::query_scalar("SELECT status FROM sandbox_hosts WHERE host_id = 'host-a'")
            .fetch_one(&app.pool)
            .await
            .unwrap();
    assert_eq!(host_status, "insync");

    // ── 4. Trigger gate: ungranted `medium` fails fast, no job is created. ──
    let pipeline_id = pipeline_with_profile(&app, "journey-main", "medium").await;
    let (status, resp) = trigger(&app, &pipeline_id).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{resp}");
    assert!(
        resp["message"]
            .as_str()
            .unwrap_or_default()
            .contains("not authorized to request resource profile 'medium'"),
        "{resp}"
    );
    let jobs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM jobs")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(jobs, 0, "an unauthorized request must not create a job");

    // ── 4. Grant: the trigger passes, the profile is frozen into the job and
    //       dispatched to the sandboxed runner on the enrolled host. ──
    let (status, resp) = app
        .put(
            "/api/v1/ops/resource-grants",
            Some(&ops),
            json!({ "team_id": TEAM, "profile": "medium" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "grant: {resp}");
    let (status, resp) = trigger(&app, &pipeline_id).await;
    assert_eq!(status, StatusCode::OK, "{resp}");
    let job1 = data(&resp)["job_id"].as_str().unwrap().to_string();
    let (stored_profile, snapshot_raw): (String, String) =
        sqlx::query_as("SELECT resource_profile, snapshot FROM jobs WHERE id = ?")
            .bind(&job1)
            .fetch_one(&app.pool)
            .await
            .unwrap();
    assert_eq!(stored_profile, "medium");
    let snapshot: Value = serde_json::from_str(&snapshot_raw).unwrap();
    assert_eq!(snapshot["resource_profile"], "medium");

    // The runner runs containerized on the enrolled host, as a sandbox job must.
    sqlx::query(
        "UPDATE runners SET execution_mode = 'container', host_id = 'host-a',
         sandbox_instance_id = 'sandboxd-01' WHERE id = ?",
    )
    .bind(RUNNER)
    .execute(&app.pool)
    .await
    .unwrap();
    let (status, resp) = poll(&app).await;
    assert_eq!(status, StatusCode::OK, "{resp}");
    assert_eq!(data(&resp)["job_id"], job1);
    assert_eq!(data(&resp)["resource_profile"], "medium");

    // ── 5. Revoke while queued: the claim refuses and the job fails terminally.
    //       A second pipeline keeps job2 clear of the same-pipeline running
    //       gate (job1 is still running on journey-main). ──
    let pipeline_b = pipeline_with_profile(&app, "journey-second", "medium").await;
    let (status, resp) = trigger(&app, &pipeline_b).await;
    assert_eq!(status, StatusCode::OK, "{resp}");
    let job2 = data(&resp)["job_id"].as_str().unwrap().to_string();
    assert_ne!(job1, job2);
    let (status, _) = app
        .delete(
            &format!("/api/v1/ops/resource-grants/{TEAM}/medium"),
            Some(&ops),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, resp) = poll(&app).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        data(&resp).get("job_id").is_none(),
        "the revoked job must not dispatch: {resp}"
    );
    let job2_status: String = sqlx::query_scalar("SELECT status FROM jobs WHERE id = ?")
        .bind(&job2)
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(job2_status, "failed", "revocation is terminal");

    // ── 6. Audit trail records the whole journey. ──
    let actions: Vec<String> = sqlx::query_scalar("SELECT action FROM audit_logs ORDER BY rowid")
        .fetch_all(&app.pool)
        .await
        .unwrap();
    for expected in [
        "sandbox_host.enroll",
        "sandbox_host.defaults_update",
        "resource_grant.update",
    ] {
        assert!(
            actions.iter().any(|a| a == expected),
            "missing audit action '{expected}' in {actions:?}"
        );
    }
    let grant_events: Vec<(String, String)> = sqlx::query_as(
        "SELECT after_json, actor_username FROM audit_logs
         WHERE action = 'resource_grant.update' ORDER BY rowid",
    )
    .fetch_all(&app.pool)
    .await
    .unwrap();
    assert_eq!(grant_events.len(), 2, "one audit row per grant and revoke");
    let granted: Value = serde_json::from_str(&grant_events[0].0).unwrap();
    assert_eq!(granted["team_id"], TEAM);
    assert_eq!(granted["profile"], "medium");
    assert_eq!(granted["granted"], true);
    assert_eq!(grant_events[0].1, "opsone", "ops user is the audit actor");
    let revoked: Value = serde_json::from_str(&grant_events[1].0).unwrap();
    assert_eq!(revoked["profile"], "medium");
    assert_eq!(revoked["granted"], false);
}
