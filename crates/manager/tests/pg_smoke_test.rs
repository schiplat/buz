//! Postgres smoke test for the six statements that carry hand-written PG
//! bodies (jsonb walks, epoch math, named-conflict upserts).
//!
//! Runs only when `BUZ_TEST_PG_URL` points at a reachable Postgres 14+ (the CI
//! Postgres job sets it); every run creates and drops its own schema, so
//! parallel runs never share tables. Without the variable the test is a no-op,
//! so default `cargo test` stays SQLite-only.

#![cfg(feature = "postgres")]

use sqlx::postgres::PgPoolOptions;

use manager::store::exec::DbPool;
use manager::store::{hosts, logs, ops_sandbox, runners};

fn pg_url() -> Option<String> {
    std::env::var("BUZ_TEST_PG_URL")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// Build a schema-scoped pool and apply the PG baseline into it.
async fn setup() -> Option<(String, DbPool, sqlx::PgPool, sqlx::PgPool)> {
    let url = pg_url()?;
    let schema = format!("buz_smoke_{}", uuid::Uuid::new_v4().simple());

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

async fn seed(pg: &sqlx::PgPool) {
    sqlx::query("INSERT INTO pipelines (id, name, team_id) VALUES ('p1', 'smoke', '')")
        .execute(pg)
        .await
        .expect("seed pipeline");
    sqlx::query(
        "INSERT INTO jobs (id, pipeline_id, status, trigger_by, snapshot) VALUES ('j1', 'p1', 'pending', 'manual', $1)",
    )
    .bind(
        r#"{"team_id":"team-x","resource_profile":"large","tasks":[{"id":"t1","step_order":1,"name":"build","exec_type":"shell"}]}"#,
    )
    .execute(pg)
    .await
    .expect("seed job");
    sqlx::query(
        "INSERT INTO hosts (id, name, host, username, labels) VALUES ('h1', 'smoke-host', '127.0.0.1', 'root', '[\"linux\",\"gpu\"]')",
    )
    .execute(pg)
    .await
    .expect("seed host");
}

#[tokio::test]
async fn pg_store_smoke() {
    let Some((schema, db, pg, admin)) = setup().await else {
        eprintln!("BUZ_TEST_PG_URL unset — skipping Postgres smoke test");
        return;
    };

    seed(&pg).await;

    // claim_candidates: jsonb team key + epoch-derived wait age.
    let candidates = runners::claim_candidates(&db, None)
        .await
        .expect("claim_candidates");
    assert_eq!(candidates.len(), 1, "one pending candidate");
    assert_eq!(candidates[0].team_id.as_deref(), Some("team-x"));
    assert!(candidates[0].waited_seconds >= 0);

    // frozen_task_rows: jsonb_array_elements walk.
    let tasks = runners::frozen_task_rows(
        &db,
        r#"{"tasks":[{"id":"t1","step_order":1,"name":"build","exec_type":"shell"}]}"#,
    )
    .await
    .expect("frozen_task_rows");
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].id, "t1");
    assert_eq!(tasks[0].step_order, 1);
    assert_eq!(tasks[0].name, "build");
    assert_eq!(tasks[0].exec_type, "shell");
    assert_eq!(tasks[0].timeout_seconds, 0);

    // visible_ids_by_label: jsonb_array_elements_text walk.
    let host_ids = hosts::visible_ids_by_label(&db, "gpu", None)
        .await
        .expect("visible_ids_by_label");
    assert_eq!(host_ids, vec!["h1".to_string()]);

    // queued_jobs_for_profile: jsonb ->> probes.
    let queued = ops_sandbox::queued_jobs_for_profile(&db, "team-x", "large")
        .await
        .expect("queued_jobs_for_profile");
    assert_eq!(queued, 1);

    // replace_grant: named-conflict upsert (second call overwrites).
    ops_sandbox::replace_grant(&db, "team-a", "large", "alice")
        .await
        .expect("first grant");
    ops_sandbox::replace_grant(&db, "team-a", "large", "bob")
        .await
        .expect("second grant");
    let granted_by: String =
        sqlx::query_scalar("SELECT granted_by FROM team_resource_grants WHERE team_id = 'team-a'")
            .fetch_one(&pg)
            .await
            .expect("grant row");
    assert_eq!(granted_by, "bob");
    let grant_rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM team_resource_grants")
        .fetch_one(&pg)
        .await
        .expect("grant count");
    assert_eq!(grant_rows, 1, "upsert must not duplicate the row");

    // insert_chunk: same (job_id, chunk_index) overwrites in place.
    logs::insert_chunk(&db, "j1", 0, 0, "first", None)
        .await
        .expect("first chunk");
    logs::insert_chunk(&db, "j1", 0, 0, "second", Some("info"))
        .await
        .expect("retried chunk");
    let chunks = logs::read_all(&db, "j1").await.expect("read logs");
    assert_eq!(chunks.len(), 1, "retried chunk must not duplicate");
    assert_eq!(chunks[0]["content"], "second");

    // Schema hygiene: this test owns its schema end to end.
    let _ = sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&admin)
        .await;
    let _ = pg.close().await;
}
