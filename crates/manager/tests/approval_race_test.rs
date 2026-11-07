//! Approve / cancel / reject races on a `waiting_approval` job: conditional
//! state transitions must have exactly one winner, and a late approve/reject
//! after a cancel (or reject) must fail with Conflict — never overwrite the
//! recorded outcome or report success.

use axum::{
    body::Body,
    http::{header::AUTHORIZATION, Request, StatusCode},
    Router,
};
use http_body_util::BodyExt;
use shared::auth::{Role, TokenManager};
use sqlx::sqlite::{SqlitePool, SqlitePoolOptions};
use tower::ServiceExt;

use manager::api;
use manager::config::AuthConfig;
use manager::config::Config;
use manager::config::PipelineConfig;
use manager::config::RunnerConfig;
use manager::config::ServerConfig;
use manager::db::run_migrations;
use manager::services::log_broadcast::LogBroadcaster;
use manager::state::AppState;

const JWT_SECRET: &str = "test-secret-0123456789-abcdefghijklmnop";
const SUPER_ADMIN_ID: &str = "admin-1";
const DEV_ID: &str = "dev-1";
const MAINTAINER_ID: &str = "maint-1";

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

    for (id, name, admin_flag) in [
        (SUPER_ADMIN_ID, "boss", 1),
        (DEV_ID, "dev", 0),
        (MAINTAINER_ID, "maint", 0),
    ] {
        sqlx::query(
            "INSERT INTO users (id, username, password_hash, display_name, is_platform_admin, is_super_admin, is_active)
             VALUES (?, ?, 'hash', ?, ?, ?, 1)",
        )
        .bind(id)
        .bind(name)
        .bind(name)
        .bind(admin_flag)
        .bind(admin_flag)
        .execute(&pool)
        .await
        .expect("seed user");
    }

    sqlx::query("INSERT INTO teams (id, name, slug) VALUES ('team-1', 'Default', 'default')")
        .execute(&pool)
        .await
        .expect("seed team");

    for (uid, role) in [(DEV_ID, "developer"), (MAINTAINER_ID, "maintainer")] {
        sqlx::query("INSERT INTO team_members (team_id, user_id, role) VALUES ('team-1', ?, ?)")
            .bind(uid)
            .bind(role)
            .execute(&pool)
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

async fn job_snapshot_outcome(pool: &SqlitePool, job_id: &str) -> Option<String> {
    let raw: String = sqlx::query_scalar("SELECT snapshot FROM jobs WHERE id = ?")
        .bind(job_id)
        .fetch_one(pool)
        .await
        .expect("load snapshot");
    let snap: serde_json::Value = serde_json::from_str(&raw).unwrap();
    snap["approval"]["outcome"].as_str().map(str::to_string)
}

async fn approval_actions(pool: &SqlitePool, job_id: &str) -> Vec<String> {
    sqlx::query_scalar("SELECT action FROM job_approvals WHERE job_id = ? ORDER BY created_at, id")
        .bind(job_id)
        .fetch_all(pool)
        .await
        .expect("load approval trail")
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

    let status: String = sqlx::query_scalar("SELECT status FROM jobs WHERE id = ?")
        .bind(&job_id)
        .fetch_one(&pool)
        .await
        .unwrap();
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

    let status: String = sqlx::query_scalar("SELECT status FROM jobs WHERE id = ?")
        .bind(&job_id)
        .fetch_one(&pool)
        .await
        .unwrap();
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

    let status: String = sqlx::query_scalar("SELECT status FROM jobs WHERE id = ?")
        .bind(&job_id)
        .fetch_one(&pool)
        .await
        .unwrap();
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

    let status: String = sqlx::query_scalar("SELECT status FROM jobs WHERE id = ?")
        .bind(&job_id)
        .fetch_one(&pool)
        .await
        .unwrap();
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

    let status: String = sqlx::query_scalar("SELECT status FROM jobs WHERE id = ?")
        .bind(&job_id)
        .fetch_one(&pool)
        .await
        .unwrap();
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

    let status: String = sqlx::query_scalar("SELECT status FROM jobs WHERE id = ?")
        .bind(&job_id)
        .fetch_one(&pool)
        .await
        .unwrap();
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

    let (status, received): (String, i64) =
        sqlx::query_as("SELECT status, snapshot FROM jobs WHERE id = ?")
            .bind(&job_id)
            .fetch_one(&pool)
            .await
            .map(|(status, snapshot): (String, String)| {
                let snap: serde_json::Value = serde_json::from_str(&snapshot).unwrap();
                (status, snap["approval"]["received"].as_i64().unwrap_or(0))
            })
            .unwrap();
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
    let mut guard = pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .expect("hold writer lock");

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
    sqlx::query(
        "INSERT INTO job_approvals (id, job_id, user_id, username, action, comment)
         VALUES ('approval-concurrent-1', ?, 'approver-2', 'approver2', 'approve', '')",
    )
    .bind(&job_id)
    .execute(&mut *guard)
    .await
    .expect("record concurrent approval");
    sqlx::query(
        "UPDATE jobs SET snapshot = json_set(snapshot, '$.approval.received', 1),
         updated_at = datetime('now') WHERE id = ?",
    )
    .bind(&job_id)
    .execute(&mut *guard)
    .await
    .expect("bump approval received");
    guard.commit().await.expect("release writer lock");

    let reject = reject_handle.await.expect("join reject task");
    assert_eq!(reject.status(), StatusCode::OK, "reject succeeds");

    let status: String = sqlx::query_scalar("SELECT status FROM jobs WHERE id = ?")
        .bind(&job_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(status, "cancelled");
    assert_eq!(
        job_snapshot_outcome(&pool, &job_id).await.as_deref(),
        Some("rejected")
    );

    let snapshot_raw: String = sqlx::query_scalar("SELECT snapshot FROM jobs WHERE id = ?")
        .bind(&job_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    let snap: serde_json::Value = serde_json::from_str(&snapshot_raw).unwrap();
    assert_eq!(
        snap["approval"]["received"], 1,
        "approval recorded during the race must not be erased by reject"
    );
    let mut trail = approval_actions(&pool, &job_id).await;
    trail.sort();
    assert_eq!(trail, vec!["approve", "reject"]);
}
