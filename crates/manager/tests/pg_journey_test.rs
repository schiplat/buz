//! End-to-end store journeys on a real Postgres server — the paths the
//! statement-level gates cannot see: upsert semantics, environment
//! concurrency gates, approval flow, trigger idempotency, retention purges,
//! heartbeat/monitor bookkeeping.
//!
//! Each journey runs in its own schema (applied from the PG baseline) against
//! the same `DbPool`-level store functions the handlers use, so the coverage
//! is SQL-behaviour, not logic: SQLite-side unit tests already own the logic.
//! Runs only when `BUZ_TEST_PG_URL` is set (the CI Postgres job sets it).

#![cfg(feature = "postgres")]

use sqlx::postgres::PgPoolOptions;

use manager::rbac::AuthUser;
use manager::store::exec::DbPool;
use manager::store::{
    github, github_binds, jobs, logs, ops_sandbox, pipelines, plugin_pool, plugin_sources, runners,
    sandboxd_config, scheduler, trigger_events, trigger_reserve,
};
use shared::rbac::TeamRole;

fn pg_url() -> Option<String> {
    std::env::var("BUZ_TEST_PG_URL")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

async fn setup() -> Option<(String, DbPool, sqlx::PgPool, sqlx::PgPool)> {
    let url = pg_url()?;
    let schema = format!("buz_journey_{}", uuid::Uuid::new_v4().simple());

    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .expect("connect admin pool");
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(&admin)
        .await
        .expect("create schema");

    let options = url
        .parse::<sqlx::postgres::PgConnectOptions>()
        .expect("parse pg url")
        .options([("search_path", schema.as_str())]);
    let pg = PgPoolOptions::new()
        .max_connections(4)
        .connect_with(options)
        .await
        .expect("connect schema pool");
    manager::db::migrate_pg::run_pg_migrations(&pg)
        .await
        .expect("apply pg baseline");

    Some((schema, DbPool::Pg(pg.clone()), pg, admin))
}

fn admin_auth() -> AuthUser {
    AuthUser {
        id: "user-admin".into(),
        username: "admin".into(),
        is_platform_admin: true,
        is_super_admin: true,
        is_ops: false,
        memberships: vec![manager::rbac::TeamMembership {
            team_id: "team-a".into(),
            team_slug: "team-a".into(),
            team_name: "Team A".into(),
            role: TeamRole::Owner,
        }],
        api_token_scope: None,
    }
}

async fn seed_team(pg: &sqlx::PgPool) {
    sqlx::query("INSERT INTO teams (id, name, slug) VALUES ('team-a', 'Team A', 'team-a')")
        .execute(pg)
        .await
        .expect("seed team");
}

async fn seed_pipeline(pg: &sqlx::PgPool, id: &str, team: &str, env: Option<&str>) {
    sqlx::query(&format!(
        "INSERT INTO pipelines (id, name, team_id, is_active, run_counter, environments)
         VALUES ('{id}', 'pipeline-{id}', '{team}', 1, 0, '{}')",
        env.unwrap_or("")
    ))
    .execute(pg)
    .await
    .expect("seed pipeline");
}

async fn seed_environment(pg: &sqlx::PgPool, id: &str, concurrency: &str) {
    sqlx::query(&format!(
        "INSERT INTO environments (id, name, slug, deployment_concurrency)
         VALUES ('{id}', 'Env {id}', 'env-{id}', '{concurrency}')"
    ))
    .execute(pg)
    .await
    .expect("seed environment");
}

fn reserved_job<'a>(
    job_id: &'a str,
    pipeline_id: &'a str,
    status: &'a str,
    env: Option<&'a str>,
    snapshot: &'a serde_json::Value,
) -> trigger_reserve::ReservedJob<'a> {
    trigger_reserve::ReservedJob {
        job_id,
        pipeline_id,
        status,
        triggered_by: "admin",
        trigger_params_json: "{}",
        snapshot,
        debug_unredact: false,
        verbose: false,
        environment_id: env,
        trigger_user_id: None,
        resource_profile: "default",
    }
}

// ───────────────────────── webhook trigger + reserve ─────────────────────────

/// Webhook idempotency (the ON CONFLICT DO NOTHING serialisation point) and
/// the atomic run-number reservation.
#[tokio::test]
async fn webhook_trigger_and_reserve() {
    let Some((schema, db, pg, admin)) = setup().await else {
        eprintln!("BUZ_TEST_PG_URL unset — skipping");
        return;
    };
    seed_team(&pg).await;
    seed_pipeline(&pg, "p1", "team-a", None).await;

    let event = |delivery: Option<String>| trigger_events::TriggerEventWrite {
        pipeline_id: "p1".into(),
        source: "git".into(),
        provider: Some("github".into()),
        event_type: "push".into(),
        delivery_id: delivery,
        repo_full_name: "org/repo".into(),
        git_ref: "refs/heads/main".into(),
        branch: "main".into(),
        tag: String::new(),
        commit_sha: "abc123".into(),
        commit_message: "commit".into(),
        sender: "alice".into(),
        compare_url: String::new(),
        decision: String::new(),
        skip_reason: String::new(),
        job_id: None,
    };

    // First delivery claims; a concurrent retry is deduplicated.
    let first = trigger_events::claim_delivery(&db, &event(Some("d-1".into())))
        .await
        .expect("first claim");
    let manager::store::trigger_events::ClaimResult::Claimed { event_id } = first else {
        panic!("first claim must win, got {first:?}");
    };
    let second = trigger_events::claim_delivery(&db, &event(Some("d-1".into())))
        .await
        .expect("duplicate claim");
    let manager::store::trigger_events::ClaimResult::Existing { event_id: dup, .. } = second else {
        panic!("duplicate delivery must dedupe, got {second:?}");
    };
    assert_eq!(
        event_id, dup,
        "duplicate resolves to the original event row"
    );

    // Finalize the claimed event (webhook → pipeline trigger hand-off).
    trigger_events::finalize_event(&db, &event_id, "triggered", "", Some("job-1"))
        .await
        .expect("finalize");
    let stored = trigger_events::find_by_delivery(&db, "p1", "d-1")
        .await
        .expect("find_by_delivery")
        .expect("event row");
    assert_eq!(stored.1.as_deref(), Some("job-1"), "finalized job id");

    // Delivery-less trigger (cron / manual) + the pipeline-scoped listing.
    trigger_events::record_simple_trigger(&db, "p1", "cron", "nightly scheduler", "").await;
    let events = trigger_events::list_events(
        &db,
        "p1",
        trigger_events::ListEventsFilter {
            source: None,
            decision: None,
        },
        true,
        10,
        0,
    )
    .await
    .expect("list_events");
    assert_eq!(events.0, 2, "delivery + simple events");

    // Reserve allocates gapless run numbers and stores the snapshot.
    let snapshot = serde_json::json!({"team_id": "team-a", "tasks": []});
    let n1 = trigger_reserve::reserve_job(
        &db,
        &reserved_job("job-r1", "p1", "pending", None, &snapshot),
    )
    .await
    .expect("reserve 1");
    let n2 = trigger_reserve::reserve_job(
        &db,
        &reserved_job("job-r2", "p1", "pending", None, &snapshot),
    )
    .await
    .expect("reserve 2");
    assert_eq!((n1, n2), (1, 2), "gapless run numbers");

    let _ = sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&admin)
        .await;
}

// ───────────────────────── approval flow ─────────────────────────

/// `waiting_approval` → approve (record + job flips to pending) → reject path
/// on a fresh job flips it to `cancelled`.
#[tokio::test]
async fn approval_flow() {
    let Some((schema, db, pg, admin)) = setup().await else {
        eprintln!("BUZ_TEST_PG_URL unset — skipping");
        return;
    };
    seed_team(&pg).await;
    seed_pipeline(&pg, "p1", "team-a", None).await;
    let snapshot = serde_json::json!({
        "team_id": "team-a",
        "protection": {"require_approvals": 1, "min_approve_role": "owner"}
    });
    trigger_reserve::reserve_job(
        &db,
        &reserved_job("job-a1", "p1", "waiting_approval", None, &snapshot),
    )
    .await
    .expect("reserve waiting job");
    trigger_reserve::reserve_job(
        &db,
        &reserved_job("job-a2", "p1", "waiting_approval", None, &snapshot),
    )
    .await
    .expect("reserve second waiting job");

    let auth = admin_auth();
    let outcome = jobs::approve(&db, &auth, "job-a1", "ship it")
        .await
        .expect("approve");
    assert_eq!(outcome.new_status, "pending", "approval releases the job");
    assert_eq!(outcome.received, 1);
    assert_eq!(outcome.required, 1);

    let approvals = jobs::list_job_approvals(&db, "job-a1")
        .await
        .expect("list approvals");
    assert_eq!(approvals.len(), 1, "approval recorded");

    jobs::reject(&db, &auth, "job-a2", "not yet")
        .await
        .expect("reject");
    let status: String = sqlx::query_scalar("SELECT status FROM jobs WHERE id = 'job-a2'")
        .fetch_one(&pg)
        .await
        .expect("rejected status");
    assert_eq!(status, "cancelled", "reject cancels the waiting job");

    let _ = sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&admin)
        .await;
}

// ───────────────────────── environment gates ─────────────────────────

/// `cancel_in_progress` supersedes the running sibling; `queue` blocks while
/// a sibling runs. Both decide inside the claim transaction.
#[tokio::test]
async fn environment_concurrency_gates() {
    let Some((schema, db, pg, admin)) = setup().await else {
        eprintln!("BUZ_TEST_PG_URL unset — skipping");
        return;
    };
    seed_team(&pg).await;
    seed_pipeline(&pg, "p1", "team-a", Some("env-1")).await;
    seed_environment(&pg, "env-1", "cancel_in_progress").await;
    let snapshot = serde_json::json!({"team_id": "team-a"});

    trigger_reserve::reserve_job(
        &db,
        &reserved_job("job-running", "p1", "pending", Some("env-1"), &snapshot),
    )
    .await
    .expect("reserve running");
    sqlx::query("UPDATE jobs SET status = 'running', seq = 1 WHERE id = 'job-running'")
        .execute(&pg)
        .await
        .expect("mark running");
    trigger_reserve::reserve_job(
        &db,
        &reserved_job("job-new", "p1", "pending", Some("env-1"), &snapshot),
    )
    .await
    .expect("reserve new");

    {
        let mut tx = db.begin_immediate().await.expect("begin");
        let gate = runners::environment_gate_in_tx(&mut tx, "job-new", "p1", "env-1")
            .await
            .expect("cancel_in_progress gate");
        tx.commit().await.expect("commit");
        let manager::store::runners::EnvironmentGate::Proceed { superseded } = gate else {
            panic!("cancel_in_progress must proceed");
        };
        assert_eq!(superseded, vec!["job-running".to_string()]);
    }
    let status: String = sqlx::query_scalar("SELECT status FROM jobs WHERE id = 'job-running'")
        .fetch_one(&pg)
        .await
        .expect("superseded status");
    assert_eq!(status, "cancelled", "sibling was cancelled in the gate");

    sqlx::query("UPDATE environments SET deployment_concurrency = 'queue' WHERE id = 'env-1'")
        .execute(&pg)
        .await
        .expect("switch to queue");
    trigger_reserve::reserve_job(
        &db,
        &reserved_job("job-queued", "p1", "pending", Some("env-1"), &snapshot),
    )
    .await
    .expect("reserve queued");
    sqlx::query(
        "INSERT INTO jobs (id, pipeline_id, status, trigger_by, environment_id, seq) \
                 VALUES ('job-still-running', 'p1', 'running', 'admin', 'env-1', 2)",
    )
    .execute(&pg)
    .await
    .expect("insert second running");

    let mut tx = db.begin_immediate().await.expect("begin");
    let gate = runners::environment_gate_in_tx(&mut tx, "job-queued", "p1", "env-1")
        .await
        .expect("queue gate");
    tx.commit().await.expect("commit");
    assert!(
        matches!(gate, manager::store::runners::EnvironmentGate::Blocked),
        "queue mode must block while a sibling runs"
    );

    let _ = sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&admin)
        .await;
}

// ───────────────────────── secret injection binding ─────────────────────────

/// Pipeline secret bindings (portable ON CONFLICT DO NOTHING) and the
/// claim-path read that injects them.
#[tokio::test]
async fn secret_binding_read() {
    let Some((schema, db, pg, admin)) = setup().await else {
        eprintln!("BUZ_TEST_PG_URL unset — skipping");
        return;
    };
    seed_team(&pg).await;
    seed_pipeline(&pg, "p1", "team-a", None).await;
    sqlx::query(
        "INSERT INTO secrets (id, name, value, kind, team_id, team_shared) \
         VALUES ('s1', 'API_KEY', 'enc-value', 'kv', 'team-a', 0)",
    )
    .execute(&pg)
    .await
    .expect("seed secret");

    let mut tx = db.begin().await.expect("begin");
    pipelines::insert_secret_binding_tx(&mut tx, "p1", "s1")
        .await
        .expect("bind secret");
    pipelines::insert_secret_binding_tx(&mut tx, "p1", "s1")
        .await
        .expect("re-bind is a no-op");
    tx.commit().await.expect("commit");

    let bound = runners::bound_secret_values(&db, "p1")
        .await
        .expect("bound secrets");
    assert_eq!(
        bound,
        vec![("API_KEY".to_string(), "enc-value".to_string())],
        "claim payload injects the bound secret exactly once"
    );

    let _ = sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&admin)
        .await;
}

// ───────────────────────── github binds ─────────────────────────

/// Connection → repo → secret bind lifecycle (stale/override/push state
/// transitions all ride `datetime('now')` updates).
#[tokio::test]
async fn github_bind_lifecycle() {
    let Some((schema, db, pg, admin)) = setup().await else {
        eprintln!("BUZ_TEST_PG_URL unset — skipping");
        return;
    };
    seed_team(&pg).await;
    sqlx::query(
        "INSERT INTO secrets (id, name, value, kind, team_id, team_shared) \
                 VALUES ('s1', 'GH_TOKEN', 'enc', 'kv', 'team-a', 0)",
    )
    .execute(&pg)
    .await
    .expect("seed secret");

    let conn = github::insert_connection(
        &db,
        "conn-1",
        "https://api.github.com",
        "cipher",
        "org",
        None,
    )
    .await
    .expect("insert connection");
    github::insert_repo(&db, &conn, "org", "repo")
        .await
        .expect("insert repo");
    let repos = github::list_repos(&db).await.expect("list repos");
    assert_eq!(repos.len(), 1);
    let repo_id = repos[0].id.clone();

    assert!(
        !github_binds::secret_bind_exists(&db, "s1", &repo_id)
            .await
            .expect("bind absent"),
        "no bind yet"
    );
    github_binds::insert_secret_bind(&db, "bind-1", "s1", &repo_id, Some("override-cipher"))
        .await
        .expect("insert bind");
    let binds = github_binds::list_secret_binds(&db, "s1")
        .await
        .expect("list binds");
    assert_eq!(binds.len(), 1);
    assert_eq!(binds[0].repo_id, repo_id);

    let identity = github_binds::repo_identity(&db, &repo_id)
        .await
        .expect("repo identity")
        .expect("repo row");
    assert_eq!(
        identity,
        ("org".to_string(), "repo".to_string(), "conn-1".to_string())
    );

    // Push ok → override patch flips it stale → pushed ok again.
    sqlx::query("UPDATE secret_github_binds SET last_status = 'ok' WHERE id = 'bind-1'")
        .execute(&pg)
        .await
        .expect("simulate push ok");
    assert!(
        github_binds::mark_secret_bind_stale_if_ok(&db, "bind-1")
            .await
            .expect("stale flip"),
        "ok bind goes stale on override patch"
    );
    github_binds::set_secret_bind_override(&db, "bind-1", "s1", None)
        .await
        .expect("clear override");
    github_binds::mark_secret_bind_pushed_ok(&db, "bind-1")
        .await
        .expect("pushed ok");

    github::mark_auto_fetched(&db, &conn)
        .await
        .expect("auto fetch stamp");
    let candidates = github::fetch_candidates(&db).await.expect("candidates");
    assert!(
        candidates.is_empty(),
        "just-fetched connection is not a candidate"
    );

    let _ = sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&admin)
        .await;
}

// ───────────────────────── sandbox host config writes ─────────────────────────

/// Enrollment → desired bump → version history → fetch stamp → status report;
/// plus the restore reconcile upsert (`ON CONFLICT DO NOTHING`).
#[tokio::test]
async fn sandbox_host_config_writes() {
    let Some((schema, db, _pg, admin)) = setup().await else {
        eprintln!("BUZ_TEST_PG_URL unset — skipping");
        return;
    };
    ops_sandbox::enroll_host(&db, "host-1", "inst-1", 4, "token-hash")
        .await
        .expect("enroll");
    ops_sandbox::enroll_host(&db, "host-1", "inst-1", 8, "token-hash-2")
        .await
        .expect("re-enroll");
    let state = ops_sandbox::host_state_of(&db, "host-1", "inst-1")
        .await
        .expect("host state")
        .expect("enrolled");
    assert_eq!(state.max_resource_slots, 8, "re-enroll raises the slots");
    assert_eq!(state.desired_version, 0, "no config pushed yet");
    assert!(!state.live, "never fetched → not live");

    ops_sandbox::insert_config_version(&db, "host-1", "inst-1", 1, "{}", "sha-1", "admin")
        .await
        .expect("config v1");
    ops_sandbox::bump_desired_version(&db, "host-1", "inst-1", 1)
        .await
        .expect("bump desired");
    let doc = ops_sandbox::config_document_at(&db, "host-1", "inst-1", 1)
        .await
        .expect("document at v1")
        .expect("document row");
    assert_eq!(doc, "{}");

    // Restore reconcile: the same version inserts once, the second insert is
    // a no-op (portable ON CONFLICT DO NOTHING).
    sandboxd_config::insert_restore_config(&db, "host-1", "inst-1", 1, "{}", "sha-1")
        .await
        .expect("restore insert");
    sandboxd_config::insert_restore_config(&db, "host-1", "inst-1", 1, "{}", "sha-1")
        .await
        .expect("restore replay is a no-op");
    let versions = ops_sandbox::config_versions(&db, "host-1", "inst-1")
        .await
        .expect("config versions");
    assert_eq!(versions.len(), 1, "restore replay did not duplicate");

    sandboxd_config::stamp_fetch_ok(&db, "host-1", "inst-1")
        .await
        .expect("fetch ok");
    let state = ops_sandbox::host_state_of(&db, "host-1", "inst-1")
        .await
        .expect("host state")
        .expect("still enrolled");
    assert!(state.live, "recent fetch makes the host live");

    let _ = sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&admin)
        .await;
}

// ───────────────────────── plugin sync ─────────────────────────

/// Source insert → pool entry upsert (re-sync keeps the curator verdict) →
/// watermark advance → enable toggle.
#[tokio::test]
async fn plugin_sync_flow() {
    let Some((schema, db, pg, admin)) = setup().await else {
        eprintln!("BUZ_TEST_PG_URL unset — skipping");
        return;
    };
    plugin_sources::insert(
        &db,
        "src-1",
        "source",
        "https://plugins.example",
        None,
        true,
        "admin",
    )
    .await
    .expect("insert source");

    let entry = |file_path: &'static str| plugin_pool::PoolEntryWrite {
        namespace: "official",
        plugin_id: "demo",
        version: "1.0.0",
        sha256: "sha-1",
        source_id: "src-1",
        file_path,
        enabled: true,
        reviewed: false,
        added_by: "sync",
        requires_buz: None,
        buz_compatible: None,
        compat_error: None,
        compat_checked_against: None,
        meta_json: None,
    };
    plugin_pool::upsert_pool_entry(&db, &entry("/pool/a"))
        .await
        .expect("first sync");
    // Curator disables the entry; a re-sync must not re-enable it.
    plugin_sources::toggle_pool_entry(&db, "official", "demo", "1.0.0", false, &admin_auth())
        .await
        .expect("curator disable");
    plugin_pool::upsert_pool_entry(&db, &entry("/pool/b"))
        .await
        .expect("re-sync");
    let facts = plugin_pool::pool_entry_facts(&db, "official", "demo", "1.0.0")
        .await
        .expect("entry facts")
        .expect("entry exists");
    assert_eq!(facts.2, "/pool/b", "file path follows the re-sync");

    // Curator verdict survives: the row must still be disabled after re-sync.
    let still_disabled: i64 = sqlx::query_scalar(
        "SELECT enabled FROM plugin_pool WHERE namespace = 'official' \
                            AND plugin_id = 'demo' AND version = '1.0.0'",
    )
    .fetch_one(&pg)
    .await
    .expect("pool row");
    assert_eq!(still_disabled, 0, "re-sync preserves the disable verdict");

    plugin_pool::mark_source_synced(&db, "src-1", "2026-11-13 00:00:00", true, None)
        .await
        .expect("mark synced");
    plugin_pool::advance_index_watermark(&db, "src-1", 7, "idx-sha", None)
        .await
        .expect("advance watermark");
    assert_eq!(
        plugin_pool::index_watermark(&db, "src-1")
            .await
            .expect("watermark"),
        Some((Some(7), Some("idx-sha".to_string())))
    );

    let enabled = plugin_pool::enabled_entries(&db)
        .await
        .expect("enabled entries");
    assert!(enabled.is_empty(), "the only entry is disabled");

    let _ = sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&admin)
        .await;
}

// ───────────────────────── log windows + retention ─────────────────────────

/// Chunk upsert (retry overwrites), the three read windows, then the debug
/// and retention purges.
#[tokio::test]
async fn log_windows_and_retention() {
    let Some((schema, db, pg, admin)) = setup().await else {
        eprintln!("BUZ_TEST_PG_URL unset — skipping");
        return;
    };
    seed_team(&pg).await;
    seed_pipeline(&pg, "p1", "team-a", None).await;
    let snapshot = serde_json::json!({"team_id": "team-a"});
    trigger_reserve::reserve_job(
        &db,
        &reserved_job("job-l1", "p1", "pending", None, &snapshot),
    )
    .await
    .expect("reserve job");

    for chunk in 0..3 {
        logs::insert_chunk(
            &db,
            "job-l1",
            chunk,
            0,
            &format!("line-{chunk}"),
            Some("info"),
        )
        .await
        .expect("insert chunk");
    }
    logs::insert_chunk(&db, "job-l1", 1, 0, "line-1-retry", Some("warn"))
        .await
        .expect("retried chunk");

    let all = logs::read_all(&db, "job-l1").await.expect("read all");
    assert_eq!(all.len(), 3, "retry overwrote in place");
    assert_eq!(all[1]["content"], "line-1-retry");

    let page = logs::read_page(&db, "job-l1", 2, 1).await.expect("page");
    assert_eq!(page.len(), 2);
    assert_eq!(
        page[0]["chunk_index"], 1,
        "forward page starts at the offset"
    );

    let tail = logs::read_windowed(&db, "job-l1", Some(0), Some(3), 10)
        .await
        .expect("tail window");
    assert_eq!(tail.len(), 3, "cursor-bounded tail sees everything");
    assert_eq!(tail[0]["chunk_index"], 0, "tail returns ascending order");

    // Retention: a finished, non-debug job aged past the cutoff is a purge
    // candidate; fresh or debug jobs are not.
    sqlx::query(
        "UPDATE jobs SET status = 'success', created_at = '2020-01-01 00:00:00' \
         WHERE id = 'job-l1'",
    )
    .execute(&pg)
    .await
    .expect("age the job");
    let expired = jobs::expired_log_retention_job_ids(&db, 30, 100)
        .await
        .expect("retention candidates");
    assert_eq!(expired, vec!["job-l1".to_string()]);
    jobs::purge_job_logs(&db, "job-l1")
        .await
        .expect("purge logs");
    let all = logs::read_all(&db, "job-l1")
        .await
        .expect("post-purge read");
    assert!(all.is_empty(), "chunks dropped");
    let purged: i64 = sqlx::query_scalar("SELECT logs_purged FROM jobs WHERE id = 'job-l1'")
        .fetch_one(&pg)
        .await
        .expect("purge stamp");
    assert_eq!(purged, 1, "job stamped as purged");

    let _ = sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&admin)
        .await;
}

// ───────────────────────── heartbeat + monitor ─────────────────────────

/// Heartbeat updates (COALESCE/NULLIF path), staleness sweep, orphaned jobs,
/// and the cron queue read.
#[tokio::test]
async fn heartbeat_monitor_and_cron_queue() {
    let Some((schema, db, pg, admin)) = setup().await else {
        eprintln!("BUZ_TEST_PG_URL unset — skipping");
        return;
    };
    seed_team(&pg).await;
    seed_pipeline(&pg, "p1", "team-a", None).await;
    let snapshot = serde_json::json!({"team_id": "team-a"});
    trigger_reserve::reserve_job(
        &db,
        &reserved_job("job-m1", "p1", "pending", None, &snapshot),
    )
    .await
    .expect("reserve job");
    runners::insert(
        &db,
        &runners::NewRunner {
            id: "runner-x",
            name: "runner-x",
            register_token_hash: "hash",
            labels_json: "[]",
            version: Some("1.0"),
            team_id: Some("team-a"),
            execution_mode: "shared",
            max_concurrent_jobs: 2,
            clean_workspace: 0,
            shell: 1,
            ssh: 0,
            docker: 1,
            wasm: 0,
            ssh_host_key_policy: "accept_new",
            claim_strategy: "flexible",
        },
    )
    .await
    .expect("insert runner");

    // Register + heartbeat: the COALESCE/NULLIF spine of every poll.
    runners::mark_registered(
        &db,
        "runner-x",
        "hash",
        Some("1.0"),
        "host-1",
        "runner-inst",
    )
    .await
    .expect("register");
    runners::mark_heartbeat(
        &db,
        "runner-x",
        "online",
        "{\"os\":\"linux\"}",
        Some("1.1"),
        "host-1",
        "runner-inst",
        "sandbox-inst",
    )
    .await
    .expect("heartbeat");
    let row = sqlx::query_as::<_, (String, Option<String>)>(
        "SELECT status, version FROM runners WHERE id = 'runner-x'",
    )
    .fetch_one(&pg)
    .await
    .expect("runner row");
    assert_eq!(row, ("online".into(), Some("1.1".into())));

    // The freshness probe reads the just-written heartbeat: fresh now, stale
    // for an aged timestamp.
    let last_seen: String =
        sqlx::query_scalar("SELECT last_seen FROM runners WHERE id = 'runner-x'")
            .fetch_one(&pg)
            .await
            .expect("last seen");
    let mut tx = db.begin().await.expect("begin");
    assert!(
        runners::seen_within_90s_tx(&mut tx, Some(&last_seen))
            .await
            .expect("fresh probe"),
        "the heartbeat we just wrote is fresh"
    );
    assert!(
        !runners::seen_within_90s_tx(&mut tx, Some("2020-01-01 00:00:00"))
            .await
            .expect("aged probe"),
        "an aged last_seen is not fresh"
    );
    tx.rollback().await.expect("rollback");

    // Staleness: a fresh heartbeat keeps the runner online; an aged one is
    // swept and its dispatched jobs orphaned.
    sqlx::query("UPDATE runners SET last_seen = '2020-01-01 00:00:00' WHERE id = 'runner-x'")
        .execute(&pg)
        .await
        .expect("age the runner");
    sqlx::query("UPDATE jobs SET status = 'running', runner_id = 'runner-x' WHERE id = 'job-m1'")
        .execute(&pg)
        .await
        .expect("dispatch job");
    let stale = runners::stale_online_runners(&db, 90)
        .await
        .expect("stale list");
    assert_eq!(
        stale,
        vec![("runner-x".to_string(), "runner-x".to_string())]
    );
    runners::mark_stale_runners_offline(&db, 90)
        .await
        .expect("sweep");
    let orphans = runners::orphaned_job_ids(&db, "runner-x")
        .await
        .expect("orphans");
    assert_eq!(
        orphans,
        vec!["job-m1".to_string()],
        "dispatched job orphaned"
    );

    // Cron queue: pipelines with concurrency-deferred jobs, oldest first.
    sqlx::query("UPDATE jobs SET status = 'queued' WHERE id = 'job-m1'")
        .execute(&pg)
        .await
        .expect("defer the job");
    let queued = scheduler::queued_pipeline_ids(&db).await.expect("queue");
    assert_eq!(queued, vec!["p1".to_string()]);

    let _ = sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&admin)
        .await;
}

// ───────────────────────── job cancel + delete ─────────────────────────

/// Cancel through the public API path (audit row inside the transaction),
/// idempotence refusal, then the delete cascade.
#[tokio::test]
async fn job_cancel_and_delete() {
    let Some((schema, db, pg, admin)) = setup().await else {
        eprintln!("BUZ_TEST_PG_URL unset — skipping");
        return;
    };
    seed_team(&pg).await;
    seed_pipeline(&pg, "p1", "team-a", None).await;
    let snapshot = serde_json::json!({"team_id": "team-a"});
    trigger_reserve::reserve_job(
        &db,
        &reserved_job("job-c1", "p1", "pending", None, &snapshot),
    )
    .await
    .expect("reserve job");
    logs::insert_chunk(&db, "job-c1", 0, 0, "stale log", None)
        .await
        .expect("insert chunk");

    let old = jobs::cancel(
        &db,
        &admin_auth(),
        "job-c1",
        "p1",
        Some("pipeline-p1"),
        "team-a",
    )
    .await
    .expect("cancel");
    assert_eq!(old, "pending");
    assert!(
        jobs::cancel(
            &db,
            &admin_auth(),
            "job-c1",
            "p1",
            Some("pipeline-p1"),
            "team-a"
        )
        .await
        .is_err(),
        "cancelling a finished job is refused"
    );

    // Cancelling a waiting_approval job also writes the cancel approval row.
    trigger_reserve::reserve_job(
        &db,
        &reserved_job("job-c2", "p1", "waiting_approval", None, &snapshot),
    )
    .await
    .expect("reserve waiting job");
    let old = jobs::cancel(
        &db,
        &admin_auth(),
        "job-c2",
        "p1",
        Some("pipeline-p1"),
        "team-a",
    )
    .await
    .expect("cancel waiting job");
    assert_eq!(old, "waiting_approval");
    let approvals = jobs::list_job_approvals(&db, "job-c2")
        .await
        .expect("cancel approvals");
    assert_eq!(approvals.len(), 1, "cancel recorded an approval row");

    jobs::delete_job(&db, "job-c1").await.expect("delete job");
    let job_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM jobs WHERE id = 'job-c1'")
        .fetch_one(&pg)
        .await
        .expect("job count");
    assert_eq!(job_count, 0, "job row gone");
    let logs_left: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM job_logs WHERE job_id = 'job-c1'")
            .fetch_one(&pg)
            .await
            .expect("log count");
    assert_eq!(logs_left, 0, "log chunks cascaded");

    let _ = sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&admin)
        .await;
}
