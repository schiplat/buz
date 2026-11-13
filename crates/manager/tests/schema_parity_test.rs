//! Schema parity between the SQLite migration chain and the Postgres
//! baseline.
//!
//! The two backends must expose the **same tables and the same columns** —
//! the store's shared statements are compiled against both schemas by the
//! SQL gates, but nothing stopped one schema from drifting (a column added to
//! the SQLite chain and missed in the PG baseline compiles fine until a
//! shared statement touches it, and then only at runtime).
//!
//! The SQLite side always runs: migrations are applied to an in-memory
//! database and the table/column set is read from `sqlite_master`. The
//! Postgres side runs when `BUZ_TEST_PG_URL` points at a reachable server
//! (the CI Postgres job sets it): the baseline is applied into a throwaway
//! schema and read back from `information_schema`.
//!
//! Type checking is deliberately out of scope: SQLite's dynamic types and
//! the PG baseline's TEXT/INTEGER spellings are compared by shape, not by
//! declaration text.

use std::collections::BTreeMap;

use sqlx::sqlite::SqlitePoolOptions;

const SECRET: &str = "test-secret-0123456789-abcdefghijklmnop";

/// Tables that are backend-owned (created and maintained by each backend's
/// migration runner), not application schema. Both runners own
/// `schema_migrations` on their own backend, so both schemas carry it.
const EXCLUDED_TABLES: &[&str] = &["schema_migrations"];

async fn sqlite_schema() -> BTreeMap<String, Vec<String>> {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    manager::db::run_migrations(&pool, SECRET).await.unwrap();

    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE type = 'table' \
         AND name NOT LIKE 'sqlite_%' ORDER BY name",
    )
    .fetch_all(&pool)
    .await
    .unwrap();

    let mut out = BTreeMap::new();
    for table in tables {
        // `pragma_table_info` is a table-valued function, not an identifier.
        let columns: Vec<String> = sqlx::query_scalar(&format!(
            "SELECT name FROM pragma_table_info('{table}') ORDER BY cid"
        ))
        .fetch_all(&pool)
        .await
        .unwrap();
        out.insert(table, columns);
    }
    pool.close().await;
    out
}

#[cfg(feature = "postgres")]
async fn postgres_schema() -> BTreeMap<String, Vec<String>> {
    use sqlx::postgres::PgPoolOptions;

    let url = std::env::var("BUZ_TEST_PG_URL")
        .expect("BUZ_TEST_PG_URL must be set for the Postgres parity leg");
    let schema = format!("buz_parity_{}", uuid::Uuid::new_v4().simple());

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
        .max_connections(2)
        .connect_with(options)
        .await
        .expect("connect schema pool");
    manager::db::migrate_pg::run_pg_migrations(&pg)
        .await
        .expect("apply pg baseline");

    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT table_name, column_name FROM information_schema.columns \
         WHERE table_schema = $1 ORDER BY table_name, ordinal_position",
    )
    .bind(&schema)
    .fetch_all(&pg)
    .await
    .expect("read information_schema");

    let _ = sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&admin)
        .await;
    let _ = pg.close().await;
    let _ = admin.close().await;

    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (table, column) in rows {
        out.entry(table).or_default().push(column);
    }
    out
}

/// The application schema must be the same on both backends: same tables
/// (minus the backend-owned ones), same columns per table, same order
/// (column order matters for the positional `GenericRow` reads).
#[tokio::test]
async fn sqlite_and_postgres_schemas_match() {
    let sqlite = sqlite_schema().await;

    if !cfg!(feature = "postgres") || std::env::var("BUZ_TEST_PG_URL").is_err() {
        // SQLite-only run: still guard the extractor and the excluded set so
        // the test cannot silently degrade to "finds nothing".
        assert!(
            sqlite.len() > 30,
            "sqlite migration produced only {} tables",
            sqlite.len()
        );
        for excluded in EXCLUDED_TABLES {
            if excluded == &"schema_migrations" {
                continue; // owned by the SQLite runner too, legitimately present
            }
            assert!(
                !sqlite.contains_key(*excluded),
                "unexpected backend-owned table {excluded} in the SQLite schema"
            );
        }
        eprintln!("BUZ_TEST_PG_URL unset — Postgres parity leg skipped");
        return;
    }

    #[cfg(feature = "postgres")]
    {
        let postgres = postgres_schema().await;

        let app_sqlite: Vec<&String> = sqlite
            .keys()
            .filter(|t| !EXCLUDED_TABLES.contains(&t.as_str()))
            .collect();
        let app_pg: Vec<&String> = postgres
            .keys()
            .filter(|t| !EXCLUDED_TABLES.contains(&t.as_str()))
            .collect();

        let mut failures = Vec::new();
        let sqlite_only: Vec<_> = app_sqlite.iter().filter(|t| !app_pg.contains(t)).collect();
        let pg_only: Vec<_> = app_pg.iter().filter(|t| !app_sqlite.contains(t)).collect();
        if !sqlite_only.is_empty() {
            failures.push(format!(
                "tables missing from the PG baseline: {sqlite_only:?}"
            ));
        }
        if !pg_only.is_empty() {
            failures.push(format!("tables only in the PG baseline: {pg_only:?}"));
        }

        for table in &app_sqlite {
            let (Some(pg_columns), sqlite_columns) = (postgres.get(*table), &sqlite[*table]) else {
                continue; // already reported above
            };
            let missing: Vec<_> = sqlite_columns
                .iter()
                .filter(|c| !pg_columns.contains(c))
                .collect();
            let extra: Vec<_> = pg_columns
                .iter()
                .filter(|c| !sqlite_columns.contains(c))
                .collect();
            if !missing.is_empty() {
                failures.push(format!("{table}: columns missing on PG: {missing:?}"));
            }
            if !extra.is_empty() {
                failures.push(format!("{table}: columns only on PG: {extra:?}"));
            }
            // Positional reads depend on the projection order matching; the
            // column set equality above catches renames, this catches reorders.
            if missing.is_empty() && extra.is_empty() && sqlite_columns != pg_columns {
                failures.push(format!(
                    "{table}: column order differs\n  sqlite: {sqlite_columns:?}\n  pg:    {pg_columns:?}"
                ));
            }
        }

        assert!(
            failures.is_empty(),
            "SQLite and Postgres schemas drifted:\n\n{}",
            failures.join("\n\n")
        );
        println!(
            "schema parity ok: {} application tables match on both backends",
            app_sqlite.len()
        );
    }
}
