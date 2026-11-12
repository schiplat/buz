//! Slot-aware scheduling at claim: host-mode refusal of non-default profiles,
//! the per-host slot budget (summed across Runners sharing one sandboxd),
//! backfill vs fifo queue walking, desired∩applied profile availability, and
//! the visible dispatch reason on pending jobs.

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

/// Applied/desired document with `default` (1 slot) and `large` (4 slots).
fn document(with_large: bool) -> String {
    let mut profiles = serde_json::json!({
        "default": { "memory": "2g", "cpu": "2", "pids": 256, "slots": 1 }
    });
    if with_large {
        profiles["large"] =
            serde_json::json!({ "memory": "8g", "cpu": "4", "pids": 512, "slots": 4 });
    }
    serde_json::json!({
        "profiles": profiles,
        "tiers": { "container": { "image": "repo@sha256:abc", "runtime": "" } },
        "network": "bridge"
    })
    .to_string()
}

async fn test_app() -> (Router, SqlitePool) {
    let pool = SqlitePoolOptions::new()
        .max_connections(4)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    run_migrations(&pool, JWT_SECRET).await.unwrap();
    sqlx::query(
        "INSERT INTO users (id, username, password_hash, display_name, is_platform_admin, is_super_admin, is_ops, is_active)
         VALUES ('admin-1', 'boss', 'hash', 'boss', 1, 1, 1, 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO teams (id, name, slug) VALUES ('team-1', 'Default', 'default')")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO team_resource_grants (team_id, profile, granted_by) VALUES ('team-1', 'large', 'boss')",
    )
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

/// A sandboxed runner attached to sandbox host `host-a/sandboxd-01` (as the
/// heartbeat would have derived it), with a given strategy.
async fn seed_runner(pool: &SqlitePool, id: &str, mode: &str, strategy: &str) {
    sqlx::query(
        "INSERT INTO runners (id, name, register_token, labels, status, max_concurrent_jobs,
                              execution_mode, host_id, sandbox_instance_id, claim_strategy)
         VALUES (?, ?, 'tok', '[\"linux-x64\"]', 'online', 8, ?, 'host-a', 'sandboxd-01', ?)",
    )
    .bind(id)
    .bind(id)
    .bind(mode)
    .bind(strategy)
    .execute(pool)
    .await
    .unwrap();
}

/// Enrolled host with `budget` slots; desired and applied both at the given
/// documents/versions (sandboxd "has applied" what the test says).
async fn seed_host(
    pool: &SqlitePool,
    budget: i64,
    applied: (i64, &str),
    desired: Option<(i64, &str)>,
) {
    let (applied_v, applied_doc) = applied;
    let (desired_v, desired_doc) = desired.unwrap_or(applied);
    sqlx::query(
        "INSERT INTO sandbox_hosts (host_id, instance_id, status, max_resource_slots, desired_version, applied_version)
         VALUES ('host-a', 'sandboxd-01', 'insync', ?, ?, ?)",
    )
    .bind(budget)
    .bind(desired_v)
    .bind(applied_v)
    .execute(pool)
    .await
    .unwrap();
    for (v, doc) in [(applied_v, applied_doc), (desired_v, desired_doc)] {
        sqlx::query(
            "INSERT OR IGNORE INTO sandbox_host_configs (host_id, instance_id, version, document, document_sha256, created_by, created_at)
             VALUES ('host-a', 'sandboxd-01', ?, ?, 'sha', 'boss', datetime('now', '-1 hour'))",
        )
        .bind(v)
        .bind(doc)
        .execute(pool)
        .await
        .unwrap();
    }
}

fn jwt(sub: &str, role: Role) -> String {
    TokenManager::new(JWT_SECRET)
        .create_token(sub, role, 1)
        .unwrap()
}

async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    token: &str,
    body: Option<String>,
) -> (StatusCode, serde_json::Value) {
    let req = Request::builder()
        .method(method)
        .uri(uri)
        .header(AUTHORIZATION, format!("Bearer {token}"))
        .header("Content-Type", "application/json")
        .body(Body::from(body.unwrap_or_default()))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or_default())
}

async fn create_pipeline(app: &Router, pool: &SqlitePool, name: &str, profile: &str) -> String {
    let (status, created) = call(
        app,
        "POST",
        "/api/v1/pipelines",
        &jwt(ADMIN_ID, Role::Admin),
        Some(format!(
            r#"{{"name":"{name}","trigger_type":"manual","environments":["default"],
                "runner_labels":["linux-x64"],"team_id":"team-1","env":[],
                "steps":[{{"name":"echo","run":"echo hi","engine":"shell"}}]}}"#
        )),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "create: {created}");
    let id = created["data"]["id"].as_str().unwrap().to_string();
    sqlx::query("UPDATE pipelines SET resource_profile = ? WHERE id = ?")
        .bind(profile)
        .bind(&id)
        .execute(pool)
        .await
        .unwrap();
    id
}

async fn trigger(app: &Router, pipeline_id: &str) -> String {
    let (status, resp) = call(
        app,
        "POST",
        &format!("/api/v1/pipelines/{pipeline_id}/trigger"),
        &jwt(ADMIN_ID, Role::Admin),
        Some(r#"{"environment":"default","inputs":{}}"#.into()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "trigger: {resp}");
    resp["data"]["job_id"].as_str().unwrap().to_string()
}

/// One pipeline per job: the existing "one running job per pipeline and
/// environment" rule would otherwise mask the slot gate under test.
async fn job(app: &Router, pool: &SqlitePool, name: &str, profile: &str) -> String {
    let pipeline = create_pipeline(app, pool, name, profile).await;
    trigger(app, &pipeline).await
}

/// Poll as `runner`; returns the claimed job id if any.
async fn poll(app: &Router, runner: &str) -> Option<String> {
    let (status, body) = call(
        app,
        "GET",
        "/api/v1/runners/poll",
        &jwt(runner, Role::Runner),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "poll: {body}");
    body["data"]["job_id"].as_str().map(str::to_string)
}

async fn job_state(pool: &SqlitePool, job_id: &str) -> (String, i64, String, String) {
    sqlx::query_as(
        "SELECT status, resource_slots, sandbox_instance_id, dispatch_reason FROM jobs WHERE id = ?",
    )
    .bind(job_id)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn finish(pool: &SqlitePool, job_id: &str) {
    sqlx::query("UPDATE jobs SET status = 'success', end_time = datetime('now') WHERE id = ?")
        .bind(job_id)
        .execute(pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn host_mode_runner_refuses_non_default_profiles_but_runs_default() {
    let (app, pool) = test_app().await;
    seed_runner(&pool, "runner-host", "host", "backfill").await;
    let large = create_pipeline(&app, &pool, "p-large", "large").await;
    let plain = create_pipeline(&app, &pool, "p-plain", "").await;
    let large_job = trigger(&app, &large).await;
    let plain_job = trigger(&app, &plain).await;

    // Backfill: the large job is not eligible here; the default one dispatches.
    assert_eq!(
        poll(&app, "runner-host").await.as_deref(),
        Some(plain_job.as_str())
    );
    let (status, _, _, reason) = job_state(&pool, &large_job).await;
    assert_eq!(status, "pending");
    assert!(reason.contains("host mode"), "reason: {reason}");
    assert!(poll(&app, "runner-host").await.is_none());
}

#[tokio::test]
async fn slot_budget_caps_concurrent_large_jobs_and_frees_on_completion() {
    let (app, pool) = test_app().await;
    seed_runner(&pool, "runner-1", "container", "backfill").await;
    seed_host(&pool, 8, (1, &document(true)), None).await;
    let j1 = job(&app, &pool, "p-large-1", "large").await;
    let j2 = job(&app, &pool, "p-large-2", "large").await;
    let j3 = job(&app, &pool, "p-large-3", "large").await;

    assert_eq!(poll(&app, "runner-1").await.as_deref(), Some(j1.as_str()));
    assert_eq!(poll(&app, "runner-1").await.as_deref(), Some(j2.as_str()));
    // 8/8 slots in use → the third waits.
    assert!(poll(&app, "runner-1").await.is_none());
    let (status, _, _, reason) = job_state(&pool, &j3).await;
    assert_eq!(status, "pending");
    assert!(reason.contains("waiting for 4 slot"), "reason: {reason}");
    assert!(reason.contains("8/8"), "reason: {reason}");

    let (_, slots, key, _) = job_state(&pool, &j1).await;
    assert_eq!(slots, 4);
    assert_eq!(key, "host-a/sandboxd-01");

    finish(&pool, &j1).await;
    assert_eq!(poll(&app, "runner-1").await.as_deref(), Some(j3.as_str()));
    let (_, _, _, reason) = job_state(&pool, &j3).await;
    assert_eq!(reason, "", "claim clears the dispatch reason");
}

#[tokio::test]
async fn fifo_claims_nothing_while_the_oldest_eligible_job_does_not_fit() {
    let (app, pool) = test_app().await;
    seed_runner(&pool, "runner-bf", "container", "backfill").await;
    seed_runner(&pool, "runner-fifo", "container", "fifo").await;
    // Budget 4 = exactly one large job; a default job needs 1 slot.
    seed_host(&pool, 4, (1, &document(true)), None).await;
    let l1 = job(&app, &pool, "p-large-1", "large").await;
    let l2 = job(&app, &pool, "p-large-2", "large").await;
    let small = job(&app, &pool, "p-plain", "").await;

    // l1 fills the whole budget.
    assert_eq!(poll(&app, "runner-bf").await.as_deref(), Some(l1.as_str()));
    // fifo: the oldest eligible (l2) does not fit → claim nothing this round.
    assert!(poll(&app, "runner-fifo").await.is_none());
    // backfill: skips l2, but the small job does not fit either (4/4).
    assert!(poll(&app, "runner-bf").await.is_none());
    let (_, _, _, reason) = job_state(&pool, &small).await;
    assert!(reason.contains("waiting for 1 slot"), "reason: {reason}");

    // Once l1 finishes, order is preserved when things fit: l2 (oldest) first.
    finish(&pool, &l1).await;
    assert_eq!(
        poll(&app, "runner-fifo").await.as_deref(),
        Some(l2.as_str())
    );
    assert_eq!(job_state(&pool, &small).await.0, "pending");
}

#[tokio::test]
async fn backfill_skips_a_too_large_job_to_run_a_fitting_one() {
    let (app, pool) = test_app().await;
    seed_runner(&pool, "runner-bf", "container", "backfill").await;
    seed_runner(&pool, "runner-fifo", "container", "fifo").await;
    // Budget 5: one large (4) + one default (1) fit; two larges do not.
    seed_host(&pool, 5, (1, &document(true)), None).await;
    let l1 = job(&app, &pool, "p-large-1", "large").await;
    let l2 = job(&app, &pool, "p-large-2", "large").await;
    let small = job(&app, &pool, "p-plain", "").await;

    assert_eq!(poll(&app, "runner-bf").await.as_deref(), Some(l1.as_str()));
    // fifo runner: oldest eligible l2 needs 4, only 1 free → nothing.
    assert!(poll(&app, "runner-fifo").await.is_none());
    // backfill runner: skips l2, dispatches the small job into the free slot.
    assert_eq!(
        poll(&app, "runner-bf").await.as_deref(),
        Some(small.as_str())
    );
    let (_, _, _, reason) = job_state(&pool, &l2).await;
    assert!(reason.contains("waiting for 4 slot"), "reason: {reason}");
}

#[tokio::test]
async fn two_runners_on_one_host_share_the_slot_budget() {
    let (app, pool) = test_app().await;
    seed_runner(&pool, "runner-1", "container", "backfill").await;
    seed_runner(&pool, "runner-2", "container", "backfill").await;
    seed_host(&pool, 4, (1, &document(true)), None).await;
    let l1 = job(&app, &pool, "p-large-1", "large").await;
    let l2 = job(&app, &pool, "p-large-2", "large").await;

    assert_eq!(poll(&app, "runner-1").await.as_deref(), Some(l1.as_str()));
    // runner-2 shares host-a/sandboxd-01: its own job count is 0 but the host
    // budget is full.
    assert!(poll(&app, "runner-2").await.is_none());
    assert_eq!(job_state(&pool, &l2).await.0, "pending");
}

#[tokio::test]
async fn concurrent_polls_cannot_oversell_the_budget() {
    let (app, pool) = test_app().await;
    seed_runner(&pool, "runner-1", "container", "backfill").await;
    seed_runner(&pool, "runner-2", "container", "backfill").await;
    seed_host(&pool, 4, (1, &document(true)), None).await;
    job(&app, &pool, "p-large-1", "large").await;
    job(&app, &pool, "p-large-2", "large").await;

    let (a, b) = tokio::join!(poll(&app, "runner-1"), poll(&app, "runner-2"));
    let claimed = [a, b].into_iter().flatten().count();
    assert_eq!(claimed, 1, "exactly one large job may run on a 4-slot host");
    let running: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(resource_slots), 0) FROM jobs WHERE status = 'running'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(running, 4);
}

#[tokio::test]
async fn profile_removed_from_desired_stops_dispatch_before_the_host_applies_it() {
    let (app, pool) = test_app().await;
    seed_runner(&pool, "runner-1", "container", "backfill").await;
    // applied v1 still offers `large`; desired v2 (not yet applied) dropped it.
    seed_host(&pool, 8, (1, &document(true)), Some((2, &document(false)))).await;
    let large = create_pipeline(&app, &pool, "p-large", "large").await;
    let job = trigger(&app, &large).await;
    assert!(poll(&app, "runner-1").await.is_none());
    let (_, _, _, reason) = job_state(&pool, &job).await;
    // Dispatch is refused while the host config is stale (desired ≠ applied),
    // which also covers the removed-profile case before the host applies v2.
    assert!(reason.contains("stale"), "reason: {reason}");
}

#[tokio::test]
async fn profile_larger_than_the_host_budget_is_never_dispatched() {
    let (app, pool) = test_app().await;
    seed_runner(&pool, "runner-1", "container", "backfill").await;
    seed_host(&pool, 2, (1, &document(true)), None).await;
    let large = create_pipeline(&app, &pool, "p-large", "large").await;
    let job = trigger(&app, &large).await;
    assert!(poll(&app, "runner-1").await.is_none());
    let (_, _, _, reason) = job_state(&pool, &job).await;
    assert!(reason.contains("needs 4 slots"), "reason: {reason}");
    assert!(reason.contains("budgets 2"), "reason: {reason}");
}

#[tokio::test]
async fn sandboxed_runner_without_enrolled_host_runs_default_only() {
    let (app, pool) = test_app().await;
    seed_runner(&pool, "runner-1", "container", "backfill").await;
    // No sandbox_hosts row at all.
    let large = create_pipeline(&app, &pool, "p-large", "large").await;
    let plain = create_pipeline(&app, &pool, "p-plain", "").await;
    let large_job = trigger(&app, &large).await;
    let plain_job = trigger(&app, &plain).await;
    assert_eq!(
        poll(&app, "runner-1").await.as_deref(),
        Some(plain_job.as_str())
    );
    let (status, _, _, reason) = job_state(&pool, &large_job).await;
    assert_eq!(status, "pending");
    assert!(reason.contains("not enrolled"), "reason: {reason}");
}

#[tokio::test]
async fn claim_strategy_is_a_validated_runner_policy_field() {
    let (app, pool) = test_app().await;
    seed_runner(&pool, "runner-1", "container", "backfill").await;
    let admin = jwt(ADMIN_ID, Role::Admin);
    let (status, _) = call(
        &app,
        "PUT",
        "/api/v1/runners/runner-1",
        &admin,
        Some(r#"{"claim_strategy":"sideways"}"#.into()),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, resp) = call(
        &app,
        "PUT",
        "/api/v1/runners/runner-1",
        &admin,
        Some(r#"{"claim_strategy":"fifo"}"#.into()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{resp}");
    assert_eq!(resp["data"]["claim_strategy"], "fifo");
    // Heartbeat policy carries it to the Runner.
    let (status, hb) = call(
        &app,
        "POST",
        "/api/v1/runners/heartbeat",
        &jwt("runner-1", Role::Runner),
        Some(r#"{"status":"online"}"#.into()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{hb}");
    assert_eq!(hb["data"]["policy"]["claim_strategy"], "fifo");
}

/// The platform catalog as `platform_settings.sandbox_default_profiles` stores it.
fn catalog_json() -> String {
    serde_json::json!({
        "default": { "memory": "2g", "cpu": "2", "pids": 256, "slots": 1 },
        "large":   { "memory": "8g", "cpu": "4", "pids": 512, "slots": 4 }
    })
    .to_string()
}

async fn seed_catalog(pool: &SqlitePool) {
    sqlx::query(
        "INSERT INTO platform_settings (key, value) VALUES ('sandbox_default_profiles', ?)",
    )
    .bind(catalog_json())
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn bare_host_document_dispatches_inherited_catalog_profiles() {
    let (app, pool) = test_app().await;
    seed_runner(&pool, "runner-1", "container", "backfill").await;
    // Host document carries only `default` — `large` arrives via the platform
    // catalog (what a defaults publish delivers to every host).
    seed_host(&pool, 8, (1, &document(false)), None).await;
    seed_catalog(&pool).await;
    let large = create_pipeline(&app, &pool, "p-large", "large").await;
    let job = trigger(&app, &large).await;
    assert_eq!(
        poll(&app, "runner-1").await.as_deref(),
        Some(job.as_str()),
        "an inherited catalog profile must dispatch without a host-document save"
    );
}

#[tokio::test]
async fn host_exclusions_stop_inherited_catalog_profiles_at_dispatch() {
    let (app, pool) = test_app().await;
    seed_runner(&pool, "runner-1", "container", "backfill").await;
    // Host explicitly excludes `large` from its inherited catalog; desired ==
    // applied, so the applied-side effective document must honor the exclusion.
    let excluded = serde_json::json!({
        "profiles": { "default": { "memory": "2g", "cpu": "2", "pids": 256, "slots": 1 } },
        "tiers": { "container": { "image": "repo@sha256:abc", "runtime": "" } },
        "network": "bridge",
        "exclude_profiles": ["large"]
    })
    .to_string();
    seed_host(&pool, 8, (1, &excluded), None).await;
    seed_catalog(&pool).await;
    let large = create_pipeline(&app, &pool, "p-large", "large").await;
    let job = trigger(&app, &large).await;
    assert!(poll(&app, "runner-1").await.is_none());
    let (_, _, _, reason) = job_state(&pool, &job).await;
    assert!(
        reason.contains("does not offer resource profile 'large'"),
        "reason: {reason}"
    );
}

#[tokio::test]
async fn defaults_only_host_dispatches_the_default_profile() {
    let (app, pool) = test_app().await;
    seed_runner(&pool, "runner-1", "container", "backfill").await;
    // What a defaults publish leaves behind: a config version whose document
    // carries no profiles at all (pure inheritance).
    seed_host(&pool, 8, (1, r#"{"network":""}"#), None).await;
    seed_catalog(&pool).await;
    let plain = create_pipeline(&app, &pool, "p-plain", "").await;
    let job = trigger(&app, &plain).await;
    assert_eq!(
        poll(&app, "runner-1").await.as_deref(),
        Some(job.as_str()),
        "the implicit default must dispatch on a bare, catalog-inheriting host"
    );
}

// ── starvation aging: backfill stops overtaking a job past the threshold ──

async fn set_starvation(pool: &SqlitePool, seconds: i64) {
    sqlx::query(
        "INSERT INTO platform_settings (key, value) VALUES ('scheduler_starvation_seconds', ?)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
    )
    .bind(seconds.to_string())
    .execute(pool)
    .await
    .unwrap();
}

async fn backdate_job(pool: &SqlitePool, job_id: &str, seconds_ago: i64) {
    sqlx::query("UPDATE jobs SET created_at = datetime('now', ? || ' seconds') WHERE id = ?")
        .bind(format!("-{seconds_ago}"))
        .bind(job_id)
        .execute(pool)
        .await
        .unwrap();
}

/// Budget 8; 4 slots in use → a large job (4) waits while a small one (1)
/// fits. Backfill normally claims the small job; once the large one has
/// waited past the threshold the round ends without claiming anything, so
/// the next freed capacity goes to the large job first.
#[tokio::test]
async fn backfill_stops_overtaking_once_a_job_passes_the_starvation_threshold() {
    let (app, pool) = test_app().await;
    seed_runner(&pool, "runner-1", "container", "backfill").await;
    seed_host(&pool, 6, (1, &document(true)), None).await;
    set_starvation(&pool, 600).await;

    // Occupy 4 of 6 slots: a large job (4) no longer fits, a small one (1) does.
    let holder = job(&app, &pool, "p-large-0", "large").await;
    assert_eq!(
        poll(&app, "runner-1").await.as_deref(),
        Some(holder.as_str())
    );

    // The starved-to-be large job, backdated past the threshold.
    let large = job(&app, &pool, "p-large-1", "large").await;
    backdate_job(&pool, &large, 700).await;
    // A fresh small job, created after the large one.
    let small = job(&app, &pool, "p-small-1", "").await;

    // Without aging this poll claims the small job; with it, nothing is
    // claimed — the large job blocks the round.
    assert!(
        poll(&app, "runner-1").await.is_none(),
        "aging must suspend backfill"
    );
    let (status, _, _, reason) = job_state(&pool, &small).await;
    assert_eq!(status, "pending", "small job must not overtake: {reason}");
    assert!(
        reason.is_empty(),
        "the small job was never walked: {reason:?}"
    );
    let (status, _, _, reason) = job_state(&pool, &large).await;
    assert_eq!(status, "pending");
    assert!(reason.contains("waiting for 4 slot"), "reason: {reason}");

    // Capacity frees → the large job claims first (it is oldest).
    finish(&pool, &holder).await;
    assert_eq!(
        poll(&app, "runner-1").await.as_deref(),
        Some(large.as_str())
    );
}

/// threshold = 0 disables aging: the small job overtakes as before.
#[tokio::test]
async fn starvation_disabled_keeps_pure_backfill() {
    let (app, pool) = test_app().await;
    seed_runner(&pool, "runner-1", "container", "backfill").await;
    seed_host(&pool, 6, (1, &document(true)), None).await;
    set_starvation(&pool, 0).await;

    let holder = job(&app, &pool, "p-large-0", "large").await;
    assert_eq!(
        poll(&app, "runner-1").await.as_deref(),
        Some(holder.as_str())
    );
    let large = job(&app, &pool, "p-large-1", "large").await;
    backdate_job(&pool, &large, 700).await;
    let small = job(&app, &pool, "p-small-1", "").await;

    assert_eq!(
        poll(&app, "runner-1").await.as_deref(),
        Some(small.as_str()),
        "with aging disabled backfill claims the fitting small job"
    );
    let (status, _, _, _) = job_state(&pool, &large).await;
    assert_eq!(status, "pending");
}

/// A job younger than the threshold does not suspend backfill.
#[tokio::test]
async fn backfill_keeps_overtaking_jobs_below_the_threshold() {
    let (app, pool) = test_app().await;
    seed_runner(&pool, "runner-1", "container", "backfill").await;
    seed_host(&pool, 6, (1, &document(true)), None).await;
    set_starvation(&pool, 600).await;

    let holder = job(&app, &pool, "p-large-0", "large").await;
    assert_eq!(
        poll(&app, "runner-1").await.as_deref(),
        Some(holder.as_str())
    );
    let large = job(&app, &pool, "p-large-1", "large").await;
    backdate_job(&pool, &large, 60).await; // waited 1 min < 10 min threshold
    let small = job(&app, &pool, "p-small-1", "").await;

    assert_eq!(
        poll(&app, "runner-1").await.as_deref(),
        Some(small.as_str()),
        "below the threshold backfill still fills gaps"
    );
}
