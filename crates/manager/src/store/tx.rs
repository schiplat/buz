//! Transaction scope: the single place that spells this dialect's
//! "take the writer now" token.
//!
//! SQLite's `BEGIN IMMEDIATE` acquires the database write lock at statement
//! start, which is what makes the Manager's read-then-write sites correct
//! without explicit row locks. The Postgres port does not get that for free:
//! each caller of [`begin_immediate`] is a site that must be re-examined for
//! the serialisation point it actually needs (`SELECT … FOR UPDATE`,
//! `SKIP LOCKED`, an advisory lock, or a unique constraint) — see
//! `docs/content/docs/01-architecture/execution/transaction-boundaries.md`.
//!
//! Keeping the literal here means `api/` never carries dialect SQL, and a grep
//! for `begin_immediate` enumerates every write-serialised site in the codebase.

use sqlx::{Sqlite, SqlitePool, Transaction};

use shared::error::AppError;

/// Open a transaction that holds SQLite's write lock from the first statement.
///
/// Prefer a named operation in `store::` over calling this directly: a bare
/// transaction in a handler is exactly the shape the boundary gate exists to
/// catch, and each one owes the boundary document an inventory row.
pub async fn begin_immediate(pool: &SqlitePool) -> Result<Transaction<'_, Sqlite>, AppError> {
    pool.begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(|e| AppError::Internal(e.to_string()))
}
