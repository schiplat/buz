//! SQLite → Postgres data migration (`migrate-sqlite-to-pg`): the tool is
//! exercised against a seeded, fully migrated SQLite source and a throwaway
//! Postgres schema — verifying the occupancy guard, the per-table copy, the
//! `jobs.seq`/`run_counter` preservation and the manager_meta carry-over.
//!
//! Runs only when `BUZ_TEST_PG_URL` points at a reachable Postgres 14+ and
//! the binary was built with the `postgres` feature; otherwise a no-op.

#![cfg(feature = "postgres")]

mod common;

use sqlx::postgres::PgPoolOptions;

use manager::store::exec::{self, DbPool};

fn pg_url() -> Option<String> {
    std::env::var("BUZ_TEST_PG_URL")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// A fully migrated SQLite source with two pipelines, three jobs (one with
/// an explicit seq), a user + team, and one audit row.
async fn seeded_sqlite() -> sqlx::SqlitePool {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    manager::db::run_migrations(&pool, common::JWT_SECRET)
        .await
        .unwrap();

    let db = DbPool::Sqlite(pool.clone());
    exec::execute(&db,
        "INSERT INTO users (id, username, password_hash, display_name, is_platform_admin, is_super_admin, is_active)
         VALUES ('u1', 'boss', 'hash', 'boss', 1, 1, 1)",
        &[],
    ).await.unwrap();
    exec::execute(
        &db,
        "INSERT INTO teams (id, name, slug) VALUES ('t1', 'Alpha', 'alpha')",
        &[],
    )
    .await
    .unwrap();
    exec::execute(&db,
        "INSERT INTO pipelines (id, name, team_id, is_active, run_counter, yaml_path, yaml_sha256, yaml_source, source)
         VALUES ('p1', 'alpha', 't1', 1, 2, 'alpha.yml', 'sha-alpha', 'name: alpha', 'yaml')",
        &[],).await.unwrap();
    exec::execute(&db,
        "INSERT INTO pipelines (id, name, team_id, is_active, run_counter, yaml_path, yaml_sha256, yaml_source, source)
         VALUES ('p2', 'beta', 't1', 1, 1, 'beta.yml', 'sha-beta', 'name: beta', 'yaml')",
        &[],).await.unwrap();
    exec::execute(
        &db,
        "INSERT INTO jobs (id, pipeline_id, status, trigger_by, run_number, run_attempt)
         VALUES ('j1', 'p1', 'success', 'manual', 1, 1)",
        &[],
    )
    .await
    .unwrap();
    // An explicit non-zero seq: the copy must preserve it (the PG trigger
    // only allocates for seq = 0).
    exec::execute(&db,
        "INSERT INTO jobs (id, pipeline_id, status, trigger_by, seq) VALUES ('j2', 'p2', 'pending', 'cron', 7)",
        &[],).await.unwrap();
    exec::execute(&db,
        "INSERT INTO audit_logs (id, actor_username, action, seq) VALUES ('a1', 'boss', 'job.trigger', 3)",
        &[],).await.unwrap();
    // manager_meta is populated at Manager start (not by the migration
    // chain) — seed one row so the carry-over is observable.
    exec::execute(
        &db,
        "INSERT INTO manager_meta (key, value) VALUES ('manager_instance_id', 'mgr-migrate-1')",
        &[],
    )
    .await
    .unwrap();
    pool
}

#[tokio::test]
async fn migration_copies_every_table_and_verifies() {
    let Some(url) = pg_url() else {
        eprintln!("BUZ_TEST_PG_URL unset — skipping");
        return;
    };
    let sqlite = seeded_sqlite().await;

    let schema = format!("buz_migrate_{}", uuid::Uuid::new_v4().simple());
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .unwrap();
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(&admin)
        .await
        .unwrap();
    let options = url
        .parse::<sqlx::postgres::PgConnectOptions>()
        .unwrap()
        .options([("search_path", schema.as_str())]);
    let pg = PgPoolOptions::new()
        .max_connections(2)
        .connect_with(options)
        .await
        .unwrap();
    manager::db::migrate_pg::run_pg_migrations(&pg)
        .await
        .expect("apply PG baseline");

    // 1. Occupancy guard: the baseline alone is fine (0 rows), so run #1
    //    succeeds.
    let report = manager::db::migrate_sqlite_to_pg::migrate(&sqlite, &pg, true)
        .await
        .expect("migration run");
    let jobs = report
        .iter()
        .find(|(t, _, _)| t == "jobs")
        .expect("jobs row");
    assert_eq!((jobs.1, jobs.2), (2, 2), "jobs copied with counts verified");
    assert!(
        report.iter().all(|(_, s, c)| s == c),
        "every table verified"
    );

    // 2. Spot rows: the explicit seq and the pipeline run_counter survive.
    let pg_db = DbPool::Pg(pg.clone());
    let seq = exec::fetch_scalar_i64(&pg_db, "SELECT seq FROM jobs WHERE id = 'j2'", &[])
        .await
        .unwrap();
    assert_eq!(seq, 7, "explicit seq preserved (trigger skips seq != 0)");
    let counter = exec::fetch_scalar_i64(
        &pg_db,
        "SELECT run_counter FROM pipelines WHERE id = 'p1'",
        &[],
    )
    .await
    .unwrap();
    assert_eq!(counter, 2, "run_counter copied as data");
    let instance_id = exec::fetch_optional(
        &pg_db,
        "SELECT value FROM manager_meta WHERE key = 'manager_instance_id'",
        &[],
    )
    .await
    .unwrap()
    .expect("meta row")
    .text(0);
    assert_eq!(
        instance_id, "mgr-migrate-1",
        "instance identity carried over"
    );

    // 3. Occupancy guard: the second run against a populated target is
    //    refused — the migration never overwrites.
    let err = manager::db::migrate_sqlite_to_pg::migrate(&sqlite, &pg, false)
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("already carries rows"),
        "guard must refuse a populated target: {err}"
    );

    // 4. Source-schema guard: an unmigrated database is refused.
    let bare = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    let err = manager::db::migrate_sqlite_to_pg::migrate(&bare, &pg, false)
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("not a migrated Manager database"),
        "source schema guard: {err}"
    );

    let _ = sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&admin)
        .await;
    let _ = pg.close().await;
    let _ = admin.close().await;
    let _ = sqlite.close().await;
}
