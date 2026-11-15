//! Transaction-scoped serialisation for read-then-write invariants.
//!
//! SQLite callers open write transactions through `DbPool::begin_immediate`,
//! whose global writer lock serialises the whole transaction. Postgres has no
//! equivalent, so each named invariant takes the row (or advisory) lock that
//! protects it and holds it until commit. On SQLite these helpers are ordinary
//! reads — the writer lock already provides the guarantee — which keeps every
//! call site identical on both backends.
//!
//! Lock ordering: pipeline → job. `disable` and the trigger path lock the
//! pipeline first; `approve`/`reject` lock the pipeline and only then the job,
//! so no two transactions can take the same pair in opposite order.

use crate::store::exec::{DbTx, SqlValue};
use crate::store::sql::Sql;

/// Lock one pipeline row for the rest of the transaction.
///
/// A missing row locks nothing; the caller's own lookup reports the 404 or
/// conflict. Callers that read `pipelines.is_active` (or any other pipeline
/// column) before writing must take this first on Postgres — otherwise a
/// concurrent `disable` can commit between the read and the write.
pub async fn lock_pipeline_tx(tx: &mut DbTx<'_>, pipeline_id: &str) -> Result<(), sqlx::Error> {
    const SQL: Sql<'static> = Sql::Dual {
        sqlite: "SELECT id FROM pipelines WHERE id = ?",
        pg: "SELECT id FROM pipelines WHERE id = $1 FOR UPDATE",
    };
    tx.execute_sql(SQL, &[pipeline_id.into()]).await?;
    Ok(())
}

/// Lock one job row. Approval handlers take the pipeline lock first, then this
/// one (see the ordering note on the module).
pub async fn lock_job_tx(tx: &mut DbTx<'_>, job_id: &str) -> Result<(), sqlx::Error> {
    const SQL: Sql<'static> = Sql::Dual {
        sqlite: "SELECT id FROM jobs WHERE id = ?",
        pg: "SELECT id FROM jobs WHERE id = $1 FOR UPDATE",
    };
    tx.execute_sql(SQL, &[job_id.into()]).await?;
    Ok(())
}

/// Lock one runner row for the rest of the transaction.
pub async fn lock_runner_tx(tx: &mut DbTx<'_>, runner_id: &str) -> Result<(), sqlx::Error> {
    const SQL: Sql<'static> = Sql::Dual {
        sqlite: "SELECT id FROM runners WHERE id = ?",
        pg: "SELECT id FROM runners WHERE id = $1 FOR UPDATE",
    };
    tx.execute_sql(SQL, &[runner_id.into()]).await?;
    Ok(())
}

/// Serialise first-admin creation across concurrent `/setup` requests.
///
/// There is no row to lock yet (the `users` table is empty by definition), so
/// Postgres takes a transaction-scoped advisory lock instead. The second
/// request waits, then sees the winner's row in its `COUNT(*)` re-check and
/// returns the "setup already completed" conflict.
pub const SETUP_ADVISORY_LOCK_KEY: i64 = 0x6275_7a73_6574_7570;

pub async fn lock_first_admin_setup_tx(tx: &mut DbTx<'_>) -> Result<(), sqlx::Error> {
    const SQL: Sql<'static> = Sql::Dual {
        sqlite: "SELECT ?",
        pg: "SELECT pg_advisory_xact_lock($1)",
    };
    tx.execute_sql(SQL, &[SqlValue::Int(SETUP_ADVISORY_LOCK_KEY)])
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advisory_keys_are_distinct() {
        // The setup key must not collide with the instance or migration locks.
        assert_ne!(SETUP_ADVISORY_LOCK_KEY, 0x6275_7a6d_6772_3031); // buz-mgr instance
        assert_ne!(SETUP_ADVISORY_LOCK_KEY, 0x6275_7a73_6571_6a31); // jobs seq
        assert_ne!(SETUP_ADVISORY_LOCK_KEY, 0x6275_7a73_6571_6131); // audit seq
    }
}
