//! SQLite → Postgres data migration for in-place `single`-mode backend
//! switches (`buz-manager migrate-sqlite-to-pg`).
//!
//! Flow (mirrors the ops runbook in `storage-cluster.md`):
//!
//! 1. Open the source SQLite database read-only and verify it is a fully
//!    migrated Manager database (38 application tables).
//! 2. Point the Postgres target at an *empty* database, apply the baseline
//!    (idempotent migration runner), then refuse when any application table
//!    already carries rows — the tool never overwrites.
//! 3. Plan: print the per-table row counts. `--apply` performs the copy
//!    inside one Postgres transaction (all-or-nothing), tables in foreign-key
//!    order, columns enumerated from the SQLite schema (never caller text),
//!    values bound through the dual-backend dispatcher.
//! 4. Verify per-table counts inside the transaction before committing; any
//!    mismatch rolls everything back.
//!
//! `jobs.seq` / `audit_logs.seq`: the Postgres baseline triggers allocate
//! `max+1` only when the inserted row carries `seq = 0`. Copied rows keep
//! their explicit sequence numbers; legacy zero-seq rows are re-numbered
//! densely by the trigger (documented in the runbook).
//!
//! Stop the Manager service first: the source is opened read-only, but a
//! live writer would make the counts unverifiable.

#[cfg(feature = "postgres")]
use anyhow::Context as _;
use sqlx::Row as _;

/// The 38 application tables in foreign-key order (the PG baseline's
/// `CREATE TABLE` order, minus the runner-owned `schema_migrations`).
pub const TABLE_ORDER: &[&str] = &[
    "manager_meta",
    "hosts",
    "pipelines",
    "tasks",
    "variables",
    "step_resolutions",
    "sandbox_hosts",
    "sandbox_host_configs",
    "sandbox_config_changes",
    "secrets",
    "platform_vars",
    "pipeline_vars",
    "users",
    "teams",
    "team_members",
    "plugin_sources",
    "plugin_pool",
    "platform_settings",
    "scheduler_lease",
    "pipeline_git_hooks",
    "pipeline_trigger_events",
    "audit_logs",
    "api_tokens",
    "github_connections",
    "github_repos",
    "secret_github_binds",
    "variable_github_binds",
    "environments",
    "environment_var_overrides",
    "environment_secret_overrides",
    "environment_hosts",
    "runners",
    "jobs",
    "job_logs",
    "team_resource_grants",
    "pipeline_secrets",
    "runner_events",
    "job_approvals",
];

/// SQLite columns of one table (name, declared type) in ordinal order.
async fn sqlite_columns(
    sqlite: &sqlx::SqlitePool,
    table: &str,
) -> anyhow::Result<Vec<(String, String)>> {
    // Table names come from the fixed TABLE_ORDER list (never caller text),
    // so the pragma interpolation is identifier-safe.
    let rows = sqlx::query(&format!(
        "SELECT name, type FROM pragma_table_info('{table}') ORDER BY cid"
    ))
    .fetch_all(sqlite)
    .await
    .with_context(|| format!("introspect sqlite table {table}"))?;
    Ok(rows
        .into_iter()
        .map(|r| (r.get::<String, _>(0), r.get::<String, _>(1)))
        .collect())
}

async fn sqlite_table_count(sqlite: &sqlx::SqlitePool, table: &str) -> anyhow::Result<i64> {
    sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
        .fetch_one(sqlite)
        .await
        .with_context(|| format!("count sqlite table {table}"))
}

async fn pg_table_count(pg: &sqlx::PgPool, table: &str) -> anyhow::Result<i64> {
    // Quoted identifier, table names from the fixed list.
    sqlx::query_scalar(&format!("SELECT COUNT(*) FROM \"{table}\""))
        .fetch_one(pg)
        .await
        .with_context(|| format!("count pg table {table}"))
}

/// Rows of one table as dispatcher values. The SQLite cascade (NULL → i64 →
/// f64 → text) collapses dynamic typing exactly as stored; the parity-tested
/// schema carries no BLOB/REAL columns, so values bind to the PG TEXT/BIGINT
/// columns as Text/Int.
async fn read_rows(
    sqlite_db: &crate::store::exec::DbPool,
    table: &str,
    columns: &[(String, String)],
) -> anyhow::Result<Vec<Vec<crate::store::exec::SqlValue>>> {
    use crate::store::exec;
    let col_list = columns
        .iter()
        .map(|(name, _)| format!("\"{name}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let rows = exec::fetch_all(
        sqlite_db,
        &format!("SELECT {col_list} FROM \"{table}\""),
        &[],
    )
    .await
    .with_context(|| format!("read sqlite table {table}"))?;
    Ok(rows.iter().map(|r| r.values().to_vec()).collect())
}

fn chunk_rows(
    rows: &[Vec<crate::store::exec::SqlValue>],
    size: usize,
) -> Vec<&[Vec<crate::store::exec::SqlValue>]> {
    rows.chunks(size).collect()
}

/// Copy one table: batched multi-row INSERTs with explicit column lists.
/// Placeholders are `?`; the dispatcher renumbers them for Postgres.
async fn copy_table(
    tx: &mut crate::store::exec::DbTx<'_>,
    table: &str,
    columns: &[(String, String)],
    rows: Vec<Vec<crate::store::exec::SqlValue>>,
) -> anyhow::Result<()> {
    use crate::store::sql::Sql;
    let col_list = columns
        .iter()
        .map(|(name, _)| format!("\"{name}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let placeholder_row = format!("({})", vec!["?"; columns.len()].join(", "));
    for chunk in chunk_rows(&rows, 100) {
        if chunk.is_empty() {
            continue;
        }
        let values = vec![placeholder_row.clone(); chunk.len()].join(", ");
        let mut binds: Vec<crate::store::exec::SqlValue> = Vec::new();
        for row in chunk {
            binds.extend(row.iter().cloned());
        }
        tx.execute_sql(
            Sql::Shared(&format!(
                "INSERT INTO \"{table}\" ({col_list}) VALUES {values}"
            )),
            &binds,
        )
        .await
        .with_context(|| format!("insert into pg table {table}"))?;
    }
    Ok(())
}

/// One migration run. `apply = false` prints the plan (counts) only.
pub async fn run(
    config: &crate::config::Config,
    sqlite_path_override: Option<&str>,
    pg_url: &str,
    apply: bool,
) -> anyhow::Result<()> {
    let sqlite_path = match sqlite_path_override {
        Some(p) => std::path::PathBuf::from(p),
        None => std::path::PathBuf::from(config.database.path.clone()),
    };
    if !sqlite_path.exists() {
        anyhow::bail!("sqlite database not found: {}", sqlite_path.display());
    }

    // Read-only source: a live writer would make the verification lie.
    let sqlite = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect(&format!("sqlite://{}?mode=ro", sqlite_path.display()))
        .await
        .with_context(|| format!("open sqlite source {}", sqlite_path.display()))?;

    let pg_options = pg_url
        .parse::<sqlx::postgres::PgConnectOptions>()
        .context("parse --pg-url")?;
    let pg_admin = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect_with(pg_options.clone())
        .await
        .context("connect to postgres target")?;
    crate::db::migrate_pg::run_pg_migrations(&pg_admin)
        .await
        .context("apply PG baseline")?;
    let pg = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect_with(pg_options)
        .await
        .context("connect to postgres target")?;

    migrate(&sqlite, &pg, apply).await?;
    println!("next: point [database] url at this postgres: target and start the Manager");

    let _ = pg.close().await;
    let _ = pg_admin.close().await;
    let _ = sqlite.close().await;
    Ok(())
}

/// The migration core over connected pools: the occupancy guard, plan,
/// copy + per-table count verification inside one PG transaction. Returns
/// per-table `(source, copied)` counts.
pub async fn migrate(
    sqlite: &sqlx::SqlitePool,
    pg: &sqlx::PgPool,
    apply: bool,
) -> anyhow::Result<Vec<(String, i64, i64)>> {
    // The source must be a fully migrated Manager database.
    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE type = 'table' \
         AND name NOT LIKE 'sqlite_%' AND name != 'schema_migrations' ORDER BY name",
    )
    .fetch_all(sqlite)
    .await
    .context("read sqlite_master")?;
    let missing: Vec<&str> = TABLE_ORDER
        .iter()
        .filter(|t| !tables.iter().any(|have| have == *t))
        .copied()
        .collect();
    if !missing.is_empty() {
        anyhow::bail!(
            "source is not a migrated Manager database — missing tables: {missing:?}. \
             Start the Manager once against this file so the migration chain completes first."
        );
    }

    use crate::store::sql::Sql;

    let pg_db = crate::store::exec::DbPool::Pg(pg.clone());
    let sqlite_db = crate::store::exec::DbPool::Sqlite(sqlite.clone());
    // PG target: apply the occupancy guard — the tool never overwrites a
    // populated database.

    let mut occupied = Vec::new();
    for table in TABLE_ORDER {
        let n = pg_table_count(pg, table).await?;
        if n > 0 {
            occupied.push(format!("{table}={n}"));
        }
    }
    if !occupied.is_empty() {
        anyhow::bail!(
            "postgres target already carries rows ({}) — the migration never overwrites. \
             Drop the database (or its schema) and retry.",
            occupied.join(", ")
        );
    }

    // Plan: per-table source counts.
    let mut plan = Vec::new();
    for table in TABLE_ORDER {
        let n = sqlite_table_count(sqlite, table).await?;
        plan.push((*table, n));
    }
    let total: i64 = plan.iter().map(|(_, n)| n).sum();
    println!(
        "SQLite → Postgres migration plan ({} tables, {total} rows):",
        plan.len()
    );
    for (table, n) in &plan {
        println!("  {table}: {n}");
    }
    let mut report: Vec<(String, i64, i64)> = Vec::new();
    if !apply {
        println!("plan only — pass --apply to perform the copy");
        for (table, n) in plan {
            report.push((table.to_string(), n, 0));
        }
        return Ok(report);
    }

    // Copy inside one PG transaction; verify counts before committing.
    let started = std::time::Instant::now();
    let mut tx = pg_db.begin().await?;
    for (table, source_count) in &plan {
        let columns = sqlite_columns(sqlite, table).await?;
        if columns.is_empty() {
            anyhow::bail!("sqlite table {table} has no columns — schema drift");
        }
        let rows = read_rows(&sqlite_db, table, &columns).await?;
        if rows.len() as i64 != *source_count {
            anyhow::bail!(
                "source changed while reading {table} ({} of {source_count} rows) — \
                 stop the Manager and retry",
                rows.len()
            );
        }
        copy_table(&mut tx, table, &columns, rows).await?;
        // Verify through the tx: an outside connection cannot see the
        // uncommitted rows.
        let copied = tx
            .fetch_optional_sql(
                Sql::Shared(&format!("SELECT COUNT(*) AS n FROM \"{table}\"")),
                &[],
            )
            .await?
            .expect("count row")
            .i64(0);
        if copied != *source_count {
            anyhow::bail!(
                "verification failed for {table}: copied {copied} of {source_count} rows"
            );
        }
        report.push(((*table).to_string(), *source_count, copied));
    }

    // Per-table count verification happened above (inside the tx). Commit.
    tx.commit().await?;
    println!(
        "migration complete: {total} rows across {} tables in {:.1}s",
        plan.len(),
        started.elapsed().as_secs_f64()
    );
    Ok(report)
}
