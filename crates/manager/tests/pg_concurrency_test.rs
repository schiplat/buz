//! Postgres concurrency gates.
//!
//! Every invariant SQLite gets for free from `BEGIN IMMEDIATE` needs an
//! explicit Postgres lock. These tests exercise the three that are easiest to
//! get wrong and hardest to see in single-threaded suites:
//!
//! - first-admin `/setup`: concurrent requests must create exactly one admin
//!   (transaction-scoped advisory lock);
//! - `jobs.seq` / `audit_logs.seq`: concurrent inserts must receive distinct
//!   sequence numbers (advisory lock inside the AFTER INSERT trigger);
//! - `reserve_job` vs `disable`: a trigger that starts before the disable
//!   commits must see the disabled row and be refused (pipeline row lock).
//!
//! Runs only when `BUZ_TEST_PG_URL` points at a reachable Postgres 14+ and the
//! binary was built with the `postgres` feature; each test creates and drops
//! its own schema.

#![cfg(feature = "postgres")]

use std::time::Duration;

use futures_util::future::join_all;
use manager::store::exec::{self, DbPool};
use manager::store::trigger_reserve::{self, ReservedJob};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;

fn pg_url() -> Option<String> {
    std::env::var("BUZ_TEST_PG_URL")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

struct Fixture {
    admin: PgPool,
    pg: PgPool,
    schema: String,
}

async fn fixture(url: &str) -> Fixture {
    let schema = format!("buz_conc_{}", uuid::Uuid::new_v4().simple());
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(url)
        .await
        .expect("connect admin pool");
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(&admin)
        .await
        .expect("create schema");
    let options = url
        .parse::<sqlx::postgres::PgConnectOptions>()
        .expect("parse url")
        .options([("search_path", schema.as_str())]);
    let pg = PgPoolOptions::new()
        .max_connections(12)
        .connect_with(options)
        .await
        .expect("connect schema pool");
    manager::db::migrate_pg::run_pg_migrations(&pg)
        .await
        .expect("apply PG baseline");
    Fixture { admin, pg, schema }
}

impl Fixture {
    async fn finish(self) {
        let _ = sqlx::query(&format!("DROP SCHEMA {} CASCADE", self.schema))
            .execute(&self.admin)
            .await;
        let _ = self.pg.close().await;
        let _ = self.admin.close().await;
    }
}

async fn seed_pipeline(db: &DbPool) {
    exec::execute(
        db,
        "INSERT INTO teams (id, name, slug) VALUES ('t1', 'Alpha', 'alpha')",
        &[],
    )
    .await
    .unwrap();
    exec::execute(
        db,
        "INSERT INTO pipelines (id, name, team_id, is_active) \
         VALUES ('p1', 'alpha', 't1', 1)",
        &[],
    )
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_setup_creates_exactly_one_admin() {
    let Some(url) = pg_url() else {
        eprintln!("BUZ_TEST_PG_URL unset — skipping");
        return;
    };
    let fx = fixture(&url).await;
    let db = DbPool::Pg(fx.pg.clone());

    let mut tasks = Vec::new();
    for i in 0..8 {
        let db = db.clone();
        tasks.push(tokio::spawn(async move {
            let mut tx = db.begin_immediate().await.unwrap();
            manager::store::locks::lock_first_admin_setup_tx(&mut tx)
                .await
                .unwrap();
            let count = manager::store::auth::count_users_tx(&mut tx).await.unwrap();
            if count > 0 {
                return false;
            }
            manager::store::auth::insert_first_admin_tx(
                &mut tx,
                &format!("u{i}"),
                &format!("user{i}"),
                "hash",
            )
            .await
            .unwrap();
            tx.commit().await.unwrap();
            true
        }));
    }

    let winners = join_all(tasks)
        .await
        .into_iter()
        .filter(|result| matches!(result, Ok(true)))
        .count();
    assert_eq!(
        winners, 1,
        "exactly one concurrent /setup may create the first admin"
    );
    let users = exec::fetch_scalar_i64(&db, "SELECT COUNT(*) FROM users", &[])
        .await
        .unwrap();
    assert_eq!(users, 1);
    let supers = exec::fetch_scalar_i64(
        &db,
        "SELECT COUNT(*) FROM users WHERE is_platform_admin = 1 AND is_super_admin = 1",
        &[],
    )
    .await
    .unwrap();
    assert_eq!(supers, 1);

    fx.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn seq_allocation_stays_unique_under_concurrency() {
    let Some(url) = pg_url() else {
        eprintln!("BUZ_TEST_PG_URL unset — skipping");
        return;
    };
    let fx = fixture(&url).await;
    let db = DbPool::Pg(fx.pg.clone());
    seed_pipeline(&db).await;

    // Two rounds so a late writer still lands after the first burst settles.
    for round in 0..2 {
        let mut jobs = Vec::new();
        let mut audits = Vec::new();
        for i in 0..40 {
            let db_jobs = db.clone();
            jobs.push(tokio::spawn(async move {
                exec::execute(
                    &db_jobs,
                    "INSERT INTO jobs (id, pipeline_id, status, trigger_by) \
                     VALUES (?, 'p1', 'pending', 'manual')",
                    &[format!("j{round}_{i}").into()],
                )
                .await
                .unwrap();
            }));
            let db_audit = db.clone();
            audits.push(tokio::spawn(async move {
                exec::execute(
                    &db_audit,
                    "INSERT INTO audit_logs (id, action) VALUES (?, 'job.trigger')",
                    &[format!("a{round}_{i}").into()],
                )
                .await
                .unwrap();
            }));
        }
        join_all(jobs).await.into_iter().for_each(|r| r.unwrap());
        join_all(audits).await.into_iter().for_each(|r| r.unwrap());
    }

    for table in ["jobs", "audit_logs"] {
        let total = exec::fetch_scalar_i64(&db, &format!("SELECT COUNT(*) FROM {table}"), &[])
            .await
            .unwrap();
        let distinct = exec::fetch_scalar_i64(
            &db,
            &format!("SELECT COUNT(DISTINCT seq) FROM {table}"),
            &[],
        )
        .await
        .unwrap();
        assert_eq!(total, 80, "{table}: all rows inserted");
        assert_eq!(
            distinct, total,
            "{table}: concurrent inserts must receive distinct seq values"
        );
    }

    fx.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_triggers_obey_the_concurrency_group() {
    let Some(url) = pg_url() else {
        eprintln!("BUZ_TEST_PG_URL unset — skipping");
        return;
    };
    let fx = fixture(&url).await;
    let db = DbPool::Pg(fx.pg.clone());
    seed_pipeline(&db).await;
    exec::execute(
        &db,
        "UPDATE pipelines SET concurrency_group = 'prod', \
         concurrency_mode = 'discard-new', concurrency_max_queue = 3 WHERE id = 'p1'",
        &[],
    )
    .await
    .unwrap();

    // Eight simultaneous triggers: the decision and the insert share one
    // transaction (pipeline row lock), so exactly one job may pass.
    let mut tasks = Vec::new();
    for i in 0..8 {
        let db = db.clone();
        tasks.push(tokio::spawn(async move {
            let job_id = format!("j{i}");
            let snapshot = serde_json::json!({});
            trigger_reserve::reserve_job(
                &db,
                &ReservedJob {
                    job_id: &job_id,
                    pipeline_id: "p1",
                    status: "pending",
                    concurrency_group: Some("prod"),
                    concurrency_mode: "discard-new",
                    concurrency_max_queue: 3,
                    triggered_by: "webhook",
                    trigger_params_json: "{}",
                    snapshot: &snapshot,
                    debug_unredact: false,
                    verbose: false,
                    environment_id: None,
                    trigger_user_id: None,
                    resource_profile: "",
                },
            )
            .await
        }));
    }

    let successes = join_all(tasks)
        .await
        .into_iter()
        .filter(|result| matches!(result, Ok(Ok(_))))
        .count();
    assert_eq!(
        successes, 1,
        "discard-new must admit exactly one of eight concurrent triggers"
    );
    let jobs = exec::fetch_scalar_i64(&db, "SELECT COUNT(*) FROM jobs", &[])
        .await
        .unwrap();
    assert_eq!(jobs, 1);

    fx.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reserve_waits_for_disable_and_then_sees_it() {
    let Some(url) = pg_url() else {
        eprintln!("BUZ_TEST_PG_URL unset — skipping");
        return;
    };
    let fx = fixture(&url).await;
    let db = DbPool::Pg(fx.pg.clone());
    seed_pipeline(&db).await;

    // Hold the pipeline row lock and flip is_active without committing, so a
    // trigger that starts now must wait — then observe the committed disable.
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let disable_db = db.clone();
    let disable = tokio::spawn(async move {
        let mut tx = disable_db.begin_immediate().await.unwrap();
        manager::store::locks::lock_pipeline_tx(&mut tx, "p1")
            .await
            .unwrap();
        tx.execute(
            "UPDATE pipelines SET is_active = 0 WHERE id = ?",
            &["p1".into()],
        )
        .await
        .unwrap();
        ready_tx.send(()).unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        tx.commit().await.unwrap();
    });

    ready_rx.await.unwrap();
    let snapshot = serde_json::json!({});
    let job = ReservedJob {
        job_id: "j1",
        pipeline_id: "p1",
        status: "pending",
        concurrency_group: None,
        concurrency_mode: "",
        concurrency_max_queue: 3,
        triggered_by: "manual",
        trigger_params_json: "{}",
        snapshot: &snapshot,
        debug_unredact: false,
        verbose: false,
        environment_id: None,
        trigger_user_id: None,
        resource_profile: "",
    };
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        trigger_reserve::reserve_job(&db, &job),
    )
    .await
    .expect("reserve must not hang behind the pipeline lock");
    let error = result.expect_err("a disabled pipeline must refuse the trigger");
    assert!(
        error.to_string().contains("disabled"),
        "reserve must observe the committed disable: {error}"
    );
    disable.await.unwrap();

    let jobs = exec::fetch_scalar_i64(&db, "SELECT COUNT(*) FROM jobs", &[])
        .await
        .unwrap();
    assert_eq!(jobs, 0, "no job may be inserted after the disable commits");

    fx.finish().await;
}
