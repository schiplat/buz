//! Approve / cancel / reject races on a `waiting_approval` job: conditional
//! state transitions must have exactly one winner, and a late approve/reject
//! after a cancel (or reject) must fail with Conflict — never overwrite the
//! recorded outcome or report success.

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
use manager::config::AuthConfig;
use manager::config::Config;
use manager::config::PipelineConfig;
use manager::config::RunnerConfig;
use manager::config::ServerConfig;
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
const DEV_ID: &str = "dev-1";
const MAINTAINER_ID: &str = "maint-1";

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

async fn test_app() -> (Router, exec::DbPool, AppState) {
    let pool = common::test_db(4).await;

    for (id, name, admin_flag) in [
        (SUPER_ADMIN_ID, "boss", 1),
        (DEV_ID, "dev", 0),
        (MAINTAINER_ID, "maint", 0),
    ] {
        exec::execute(&pool,
        "INSERT INTO users (id, username, password_hash, display_name, is_platform_admin, is_super_admin, is_active)
             VALUES (?, ?, 'hash', ?, ?, ?, 1)",
        &[id.into(), name.into(), name.into(), admin_flag.into(), admin_flag.into()],
    )
        .await
        .expect("seed user");
    }

    exec::execute(
        &pool,
        "INSERT INTO teams (id, name, slug) VALUES ('team-1', 'Default', 'default')",
        &[],
    )
    .await
    .expect("seed team");

    for (uid, role) in [(DEV_ID, "developer"), (MAINTAINER_ID, "maintainer")] {
        exec::execute(
            &pool,
            "INSERT INTO team_members (team_id, user_id, role) VALUES ('team-1', ?, ?)",
            &[uid.into(), role.into()],
        )
        .await
        .expect("seed membership");
    }

    let state = AppState {
        pool: pool.clone(),
        config: test_config(),
        log_broadcast: LogBroadcaster::new(),
    };
    let app = Router::new().nest("/api/v1", api::router(state.clone()));
    (app, pool, state)
}

fn bearer(token: &str) -> String {
    format!("Bearer {token}")
}

fn admin_jwt() -> String {
    TokenManager::new(JWT_SECRET)
        .create_token(SUPER_ADMIN_ID, Role::Admin, 1)
        .expect("mint admin jwt")
}

fn dev_jwt() -> String {
    TokenManager::new(JWT_SECRET)
        .create_token(DEV_ID, Role::User, 0)
        .expect("mint developer jwt")
}

fn maintainer_jwt() -> String {
    TokenManager::new(JWT_SECRET)
        .create_token(MAINTAINER_ID, Role::User, 0)
        .expect("mint maintainer jwt")
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
        .header(AUTHORIZATION, bearer(jwt));
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

/// Protected env (self-approve forbidden) + pipeline, configurable threshold.
async fn setup_waiting_job_at(
    app: &Router,
    jwt: &str,
    slug: &str,
    require_approvals: i64,
) -> String {
    let create = json_req(
        app,
        "POST",
        "/api/v1/environments",
        jwt,
        Some(serde_json::json!({
            "name": slug,
            "slug": slug,
            "protection_enabled": true,
            "min_trigger_role": "developer",
            "min_approve_role": "maintainer",
            "require_approvals": require_approvals,
            "forbid_self_approve": true,
            "approval_timeout_seconds": 86400,
        })),
    )
    .await;
    assert_eq!(create.status(), StatusCode::OK, "create protected env");

    let pipe = json_req(
        app,
        "POST",
        "/api/v1/pipelines",
        jwt,
        Some(serde_json::json!({
            "name": format!("race-{slug}"),
            "trigger_type": "manual",
            "runner_labels": ["linux-x64"],
            "team_id": "team-1",
            "environments": [slug],
            "env": [],
            "steps": [{ "name": "echo", "run": "echo hi", "engine": "shell" }]
        })),
    )
    .await;
    assert_eq!(pipe.status(), StatusCode::OK, "create pipeline");
    let pid = read_body(pipe).await["data"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    let trig = json_req(
        app,
        "POST",
        &format!("/api/v1/pipelines/{pid}/trigger"),
        &dev_jwt(),
        Some(serde_json::json!({ "inputs": {}, "environment": slug })),
    )
    .await;
    assert_eq!(trig.status(), StatusCode::OK, "trigger");
    let job_id = read_body(trig).await["data"]["job_id"]
        .as_str()
        .unwrap()
        .to_string();
    job_id
}

/// Protected env (1 approval required, self-approve forbidden) + pipeline.
async fn setup_waiting_job(app: &Router, jwt: &str, slug: &str) -> String {
    setup_waiting_job_at(app, jwt, slug, 1).await
}

async fn job_snapshot_outcome(pool: &exec::DbPool, job_id: &str) -> Option<String> {
    let raw: String = exec::fetch_optional(
        pool,
        "SELECT snapshot FROM jobs WHERE id = ?",
        &[job_id.into()],
    )
    .await
    .unwrap()
    .expect("load snapshot")
    .text(0);
    let snap: serde_json::Value = serde_json::from_str(&raw).unwrap();
    snap["approval"]["outcome"].as_str().map(str::to_string)
}

async fn approval_actions(pool: &exec::DbPool, job_id: &str) -> Vec<String> {
    exec::fetch_all(
        pool,
        "SELECT action FROM job_approvals WHERE job_id = ? ORDER BY created_at, id",
        &[job_id.into()],
    )
    .await
    .unwrap()
    .iter()
    .map(|r| r.text(0))
    .collect()
}

// ── Sequential: a finished decision must not be overwritten ────────────────

#[tokio::test]
async fn late_approve_after_cancel_conflicts_and_keeps_outcome() {
    let (app, pool, _state) = test_app().await;
    let admin = admin_jwt();
    let job_id = setup_waiting_job(&app, &admin, "late-approve").await;

    let cancel = json_req(
        &app,
        "POST",
        &format!("/api/v1/jobs/{job_id}/cancel"),
        &admin,
        None,
    )
    .await;
    assert_eq!(cancel.status(), StatusCode::OK, "cancel waiting job");

    let approve = json_req(
        &app,
        "POST",
        &format!("/api/v1/jobs/{job_id}/approve"),
        &maintainer_jwt(),
        Some(serde_json::json!({ "comment": "too late" })),
    )
    .await;
    assert_eq!(
        approve.status(),
        StatusCode::CONFLICT,
        "late approve must conflict, not succeed silently"
    );

    let status: String = exec::fetch_optional(
        &pool,
        "SELECT status FROM jobs WHERE id = ?",
        &[job_id.as_str().into()],
    )
    .await
    .unwrap()
    .expect("job row")
    .text(0);
    assert_eq!(status, "cancelled");
    assert_eq!(
        job_snapshot_outcome(&pool, &job_id).await.as_deref(),
        Some("cancelled"),
        "cancel outcome must not be overwritten by approve"
    );
    assert_eq!(
        approval_actions(&pool, &job_id).await,
        vec!["cancel"],
        "no approve trail row may appear after cancel"
    );
}

#[tokio::test]
async fn late_reject_after_cancel_conflicts_and_keeps_outcome() {
    let (app, pool, _state) = test_app().await;
    let admin = admin_jwt();
    let job_id = setup_waiting_job(&app, &admin, "late-reject").await;

    let cancel = json_req(
        &app,
        "POST",
        &format!("/api/v1/jobs/{job_id}/cancel"),
        &admin,
        None,
    )
    .await;
    assert_eq!(cancel.status(), StatusCode::OK);

    let reject = json_req(
        &app,
        "POST",
        &format!("/api/v1/jobs/{job_id}/reject"),
        &maintainer_jwt(),
        Some(serde_json::json!({ "comment": "too late" })),
    )
    .await;
    assert_eq!(
        reject.status(),
        StatusCode::CONFLICT,
        "late reject must conflict, not succeed silently"
    );

    let status: String = exec::fetch_optional(
        &pool,
        "SELECT status FROM jobs WHERE id = ?",
        &[job_id.as_str().into()],
    )
    .await
    .unwrap()
    .expect("job row")
    .text(0);
    assert_eq!(status, "cancelled");
    assert_eq!(
        job_snapshot_outcome(&pool, &job_id).await.as_deref(),
        Some("cancelled"),
        "cancel outcome must not be overwritten by reject"
    );
    assert_eq!(
        approval_actions(&pool, &job_id).await,
        vec!["cancel"],
        "no reject trail row may appear after cancel"
    );
}

#[tokio::test]
async fn late_approve_after_reject_conflicts_and_keeps_outcome() {
    let (app, pool, _state) = test_app().await;
    let admin = admin_jwt();
    let job_id = setup_waiting_job(&app, &admin, "reject-then-approve").await;

    let reject = json_req(
        &app,
        "POST",
        &format!("/api/v1/jobs/{job_id}/reject"),
        &maintainer_jwt(),
        Some(serde_json::json!({ "comment": "nope" })),
    )
    .await;
    assert_eq!(reject.status(), StatusCode::OK);

    let approve = json_req(
        &app,
        "POST",
        &format!("/api/v1/jobs/{job_id}/approve"),
        &admin,
        Some(serde_json::json!({})),
    )
    .await;
    assert_eq!(approve.status(), StatusCode::CONFLICT);

    let status: String = exec::fetch_optional(
        &pool,
        "SELECT status FROM jobs WHERE id = ?",
        &[job_id.as_str().into()],
    )
    .await
    .unwrap()
    .expect("job row")
    .text(0);
    assert_eq!(status, "cancelled");
    assert_eq!(
        job_snapshot_outcome(&pool, &job_id).await.as_deref(),
        Some("rejected"),
        "reject outcome must not be overwritten by approve"
    );
    assert_eq!(
        approval_actions(&pool, &job_id).await,
        vec!["reject"],
        "no approve trail row may appear after reject"
    );
}

#[tokio::test]
async fn late_reject_after_approved_promotion_conflicts_and_keeps_outcome() {
    let (app, pool, _state) = test_app().await;
    let admin = admin_jwt();
    let job_id = setup_waiting_job(&app, &admin, "approve-then-reject").await;

    let approve = json_req(
        &app,
        "POST",
        &format!("/api/v1/jobs/{job_id}/approve"),
        &maintainer_jwt(),
        Some(serde_json::json!({})),
    )
    .await;
    assert_eq!(approve.status(), StatusCode::OK);
    let approved = read_body(approve).await;
    assert_eq!(
        approved["data"]["status"], "pending",
        "threshold met → pending"
    );

    let reject = json_req(
        &app,
        "POST",
        &format!("/api/v1/jobs/{job_id}/reject"),
        &admin,
        Some(serde_json::json!({})),
    )
    .await;
    assert_eq!(reject.status(), StatusCode::CONFLICT);

    let status: String = exec::fetch_optional(
        &pool,
        "SELECT status FROM jobs WHERE id = ?",
        &[job_id.as_str().into()],
    )
    .await
    .unwrap()
    .expect("job row")
    .text(0);
    assert_eq!(
        status, "pending",
        "promoted job must not be cancelled by late reject"
    );
    assert_eq!(
        job_snapshot_outcome(&pool, &job_id).await.as_deref(),
        Some("approved"),
        "approved outcome must survive the late reject"
    );
}

// ── Concurrent: exactly one side of the race wins ──────────────────────────

#[tokio::test]
async fn approve_and_cancel_race_has_single_winner() {
    let (app, pool, _state) = test_app().await;
    let admin = admin_jwt();
    let job_id = setup_waiting_job(&app, &admin, "race-cancel").await;

    let approve_uri = format!("/api/v1/jobs/{job_id}/approve");
    let cancel_uri = format!("/api/v1/jobs/{job_id}/cancel");
    let approver = maintainer_jwt();
    let approve_req = json_req(
        &app,
        "POST",
        &approve_uri,
        &approver,
        Some(serde_json::json!({})),
    );
    let cancel_req = json_req(&app, "POST", &cancel_uri, &admin, None);
    let (approve, cancel) = tokio::join!(approve_req, cancel_req);
    let approve_ok = approve.status() == StatusCode::OK;
    let cancel_ok = cancel.status() == StatusCode::OK;
    // approve only applies to `waiting_approval`; cancel also accepts `pending`.
    // So either cancel wins outright (approve conflicts), or approve promotes
    // first and the cancel legitimately takes the now-pending job down. Both
    // orders must leave a consistent outcome/trail — never a corrupted state.
    assert!(
        cancel_ok,
        "cancel must never lose this race (approve={}, cancel={})",
        approve.status(),
        cancel.status()
    );

    let status: String = exec::fetch_optional(
        &pool,
        "SELECT status FROM jobs WHERE id = ?",
        &[job_id.as_str().into()],
    )
    .await
    .unwrap()
    .expect("job row")
    .text(0);
    let outcome = job_snapshot_outcome(&pool, &job_id).await;
    let trail = approval_actions(&pool, &job_id).await;
    assert_eq!(status, "cancelled", "cancel won → cancelled");
    if approve_ok {
        // Approve promoted first; the later cancel of the pending job must not
        // rewrite the approval decision (and adds no approval-trail row).
        assert_eq!(outcome.as_deref(), Some("approved"));
        assert_eq!(trail, vec!["approve"]);
    } else {
        assert_eq!(approve.status(), StatusCode::CONFLICT);
        assert_eq!(outcome.as_deref(), Some("cancelled"));
        assert_eq!(trail, vec!["cancel"]);
    }
}

#[tokio::test]
async fn approve_and_reject_race_has_single_winner() {
    let (app, pool, _state) = test_app().await;
    let admin = admin_jwt();
    let job_id = setup_waiting_job(&app, &admin, "race-reject").await;

    let approve_uri = format!("/api/v1/jobs/{job_id}/approve");
    let reject_uri = format!("/api/v1/jobs/{job_id}/reject");
    let approver = maintainer_jwt();
    let approve_req = json_req(
        &app,
        "POST",
        &approve_uri,
        &approver,
        Some(serde_json::json!({})),
    );
    let reject_req = json_req(
        &app,
        "POST",
        &reject_uri,
        &admin,
        Some(serde_json::json!({})),
    );
    let (approve, reject) = tokio::join!(approve_req, reject_req);
    let approve_ok = approve.status() == StatusCode::OK;
    let reject_ok = reject.status() == StatusCode::OK;
    assert!(
        approve_ok ^ reject_ok,
        "exactly one of approve/reject must win (approve={}, reject={})",
        approve.status(),
        reject.status()
    );

    let status: String = exec::fetch_optional(
        &pool,
        "SELECT status FROM jobs WHERE id = ?",
        &[job_id.as_str().into()],
    )
    .await
    .unwrap()
    .expect("job row")
    .text(0);
    let outcome = job_snapshot_outcome(&pool, &job_id).await;
    if approve_ok {
        assert_eq!(status, "pending");
        assert_eq!(outcome.as_deref(), Some("approved"));
    } else {
        assert_eq!(status, "cancelled");
        assert_eq!(outcome.as_deref(), Some("rejected"));
    }
}

#[tokio::test]
async fn concurrent_approvals_with_threshold_one_have_single_winner() {
    let (app, pool, _state) = test_app().await;
    let admin = admin_jwt();
    let job_id = setup_waiting_job(&app, &admin, "race-double").await;

    let approve_uri = format!("/api/v1/jobs/{job_id}/approve");
    let first_approver = maintainer_jwt();
    let first_req = json_req(
        &app,
        "POST",
        &approve_uri,
        &first_approver,
        Some(serde_json::json!({})),
    );
    let second_req = json_req(
        &app,
        "POST",
        &approve_uri,
        &admin,
        Some(serde_json::json!({})),
    );
    let (first, second) = tokio::join!(first_req, second_req);
    let first_ok = first.status() == StatusCode::OK;
    let second_ok = second.status() == StatusCode::OK;
    assert!(
        first_ok ^ second_ok,
        "threshold 1: exactly one concurrent approve may win ({} vs {})",
        first.status(),
        second.status()
    );

    let (status, received): (String, i64) = {
        let row = exec::fetch_optional(
            &pool,
            "SELECT status, snapshot FROM jobs WHERE id = ?",
            &[job_id.as_str().into()],
        )
        .await
        .unwrap()
        .expect("job row");
        let snapshot: String = row.text(1);
        let snap: serde_json::Value = serde_json::from_str(&snapshot).unwrap();
        (
            row.text(0),
            snap["approval"]["received"].as_i64().unwrap_or(0),
        )
    };
    assert_eq!(status, "pending");
    assert_eq!(received, 1, "exactly one approval counted");
    assert_eq!(approval_actions(&pool, &job_id).await.len(), 1);
}

// ── Snapshot freshness: reject must re-read inside its transaction ─────────

/// An approval recorded after reject's pre-transaction read must survive the
/// reject. Deterministic interleaving: hold the SQLite writer lock so the
/// reject handler parks at `BEGIN IMMEDIATE` (its first read already done),
/// record a below-threshold approval (exactly what `approve()` writes), then
/// release. The reject's snapshot write must carry the fresh `received`.
#[tokio::test]
async fn reject_rereads_snapshot_in_transaction_preserves_concurrent_approval() {
    let (app, pool, _state) = test_app().await;
    let admin = admin_jwt();
    let job_id = setup_waiting_job_at(&app, &admin, "reject-fresh", 2).await;

    // Park the reject handler at BEGIN IMMEDIATE.
    let mut guard = pool.begin_immediate().await.expect("hold writer lock");

    let reject_uri = format!("/api/v1/jobs/{job_id}/reject");
    let rejector = maintainer_jwt();
    let reject_app = app.clone();
    let reject_handle = tokio::spawn(async move {
        json_req(
            &reject_app,
            "POST",
            &reject_uri,
            &rejector,
            Some(serde_json::json!({})),
        )
        .await
    });

    // Let the reject handler finish its pre-transaction read.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    // Concurrent below-threshold approval: approve row + snapshot received 1.
    guard
        .execute(
            "INSERT INTO job_approvals (id, job_id, user_id, username, action, comment)
         VALUES ('approval-concurrent-1', ?, 'approver-2', 'approver2', 'approve', '')",
            &[job_id.as_str().into()],
        )
        .await
        .expect("record concurrent approval");
    // json_set is SQLite-only: read, rewrite in Rust, write back.
    let raw = guard
        .fetch_optional(
            "SELECT snapshot FROM jobs WHERE id = ?",
            &[job_id.as_str().into()],
        )
        .await
        .unwrap()
        .expect("job row")
        .text(0);
    let mut snap: serde_json::Value = serde_json::from_str(&raw).unwrap();
    snap["approval"]["received"] = serde_json::json!(1);
    guard
        .execute(
            "UPDATE jobs SET snapshot = ?, updated_at = datetime('now') WHERE id = ?",
            &[
                serde_json::to_string(&snap).unwrap().into(),
                job_id.as_str().into(),
            ],
        )
        .await
        .expect("bump approval received");
    guard.commit().await.expect("release writer lock");

    let reject = reject_handle.await.expect("join reject task");
    assert_eq!(reject.status(), StatusCode::OK, "reject succeeds");

    let status: String = exec::fetch_optional(
        &pool,
        "SELECT status FROM jobs WHERE id = ?",
        &[job_id.as_str().into()],
    )
    .await
    .unwrap()
    .expect("job row")
    .text(0);
    assert_eq!(status, "cancelled");
    assert_eq!(
        job_snapshot_outcome(&pool, &job_id).await.as_deref(),
        Some("rejected")
    );

    let snapshot_raw: String = exec::fetch_optional(
        &pool,
        "SELECT snapshot FROM jobs WHERE id = ?",
        &[job_id.as_str().into()],
    )
    .await
    .unwrap()
    .expect("job row")
    .text(0);
    let snap: serde_json::Value = serde_json::from_str(&snapshot_raw).unwrap();
    assert_eq!(
        snap["approval"]["received"], 1,
        "approval recorded during the race must not be erased by reject"
    );
    let mut trail = approval_actions(&pool, &job_id).await;
    trail.sort();
    assert_eq!(trail, vec!["approve", "reject"]);
}
