//! Postgres migration runner.
//!
//! Fresh-apply model on a versioned table, mirroring the SQLite runner's
//! shape (`schema_migrations` with the same column names): each pending
//! migration applies in ONE transaction (a failed apply leaves no partial
//! schema), then records its version in the same transaction.
//!
//! Startup serialisation: `pg_advisory_lock` is session-scoped, so the runner
//! takes one dedicated connection and holds the lock for the whole run — two
//! Managers starting concurrently against the same database take turns, and
//! the loser sees the winner's applied versions and skips them. The lock is
//! released explicitly on every path (a pooled session that merely returned
//! its connection would otherwise keep holding it).
//!
//! NOT yet wired into startup: `validate_cluster_mode` still refuses
//! `postgres:` fail-closed until the runtime wiring lands (store modules on
//! the dispatcher + pool selection). This runner is the piece that wiring
//! will call.

#[cfg(feature = "postgres")]
use sqlx::Connection as _;

#[cfg(feature = "postgres")]
pub const ADVISORY_LOCK_KEY: i64 = 0x6275_7A6D_6772_3031; // "buzmgr01"

#[cfg(feature = "postgres")]
/// `(version, sql)` pairs, ascending. Version 1 is the fresh baseline
/// translated from the fully migrated SQLite template (38 application tables,
/// indexes, 2 seq triggers, `datetime()` shims; `schema_migrations` is the
/// runner's own) — new PG deployments apply the merged baseline; there is no
/// incremental replay of the SQLite chain.
pub const PG_MIGRATIONS: &[(i64, &str)] = &[(1, include_str!("migrations/pg/001_baseline.sql"))];

#[cfg(feature = "postgres")]
pub async fn run_pg_migrations(pool: &sqlx::PgPool) -> anyhow::Result<()> {
    // Dedicated connection: the advisory lock is bound to this session.
    let mut conn = pool.acquire().await?;

    sqlx::query("SELECT pg_advisory_lock($1)")
        .bind(ADVISORY_LOCK_KEY)
        .execute(&mut *conn)
        .await?;

    let result = apply_pending(&mut conn).await;

    // Release on every path — a pooled session returning its connection keeps
    // the session (and the lock) alive.
    let unlock = sqlx::query("SELECT pg_advisory_unlock($1)")
        .bind(ADVISORY_LOCK_KEY)
        .execute(&mut *conn)
        .await;
    result?;
    unlock?;
    Ok(())
}

#[cfg(feature = "postgres")]
async fn apply_pending(conn: &mut sqlx::PgConnection) -> anyhow::Result<()> {
    // Same bookkeeping table shape as the SQLite runner. The default spells
    // the timestamp text directly — no dependency on the baseline's shim
    // functions, which do not exist until version 1 applies.
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
            version BIGINT PRIMARY KEY,
            applied_at TEXT NOT NULL DEFAULT to_char(now(), 'YYYY-MM-DD HH24:MI:SS')
        )",
    )
    .execute(&mut *conn)
    .await?;

    let applied: Vec<i64> = sqlx::query_scalar("SELECT version FROM schema_migrations")
        .fetch_all(&mut *conn)
        .await
        .unwrap_or_default();

    for (version, sql) in PG_MIGRATIONS {
        if applied.contains(version) {
            tracing::debug!(version, "pg migration already applied — skipped");
            continue;
        }
        tracing::info!(version, "applying pg migration");
        let mut tx = conn.begin().await?;
        sqlx::raw_sql(sql).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO schema_migrations (version) VALUES ($1)")
            .bind(version)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
    }

    Ok(())
}

#[cfg(all(test, feature = "postgres"))]
mod tests {
    use super::*;

    #[test]
    fn baseline_is_registered_as_version_1() {
        assert_eq!(PG_MIGRATIONS.len(), 1);
        assert_eq!(PG_MIGRATIONS[0].0, 1);
        assert!(PG_MIGRATIONS[0]
            .1
            .trim_start()
            .starts_with("-- Buz Manager: Postgres baseline schema."));
    }

    #[test]
    fn baseline_carries_the_full_schema() {
        let sql = PG_MIGRATIONS[0].1;
        // 38 application tables from the migrated template; `schema_migrations`
        // is owned by this runner, not the baseline. Statements must be plain
        // CREATE TABLE (fresh-apply model — no IF NOT EXISTS inside the tx).
        let create_tables = sql.matches("CREATE TABLE ").count();
        assert_eq!(create_tables, 38, "baseline table count drifted");
        assert!(!sql.contains("CREATE TABLE IF NOT EXISTS"));
        // dialect shims are part of the baseline (shared statements call them)
        assert!(sql.contains("CREATE OR REPLACE FUNCTION datetime()"));
        // the two seq-allocation triggers made it across
        assert_eq!(sql.matches("CREATE TRIGGER ").count(), 2);
    }
}
