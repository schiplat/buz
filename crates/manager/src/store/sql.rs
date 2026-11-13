//! Statement provenance: how one statement reaches each backend.
//!
//! Every SQL literal in the store is either written once in a dialect-neutral
//! form (`Sql::Shared`) or carries an explicit per-backend body
//! (`Sql::Dual`). Statements that exist on one backend only are marked as
//! such and fail closed on the other, instead of being silently sent with the
//! wrong dialect.
//!
//! The classification is the enforcement point for backend isolation:
//!
//! - `Shared` statements must be portable (only `?` placeholders are
//!   translated per backend). `dialect::sqlite_to_postgres` is restricted to
//!   placeholder renumbering; non-portable syntax has to move into `Dual`.
//! - `Dual` bodies are used verbatim: the Postgres text carries `$n`
//!   placeholders and never passes through the rewriter, so native PG syntax
//!   (jsonb walks, `ON CONFLICT` targets, epoch math) is allowed there.
//! - `SqliteOnly` / `PgOnly` make a call on the wrong backend a clear
//!   configuration error rather than a SQL syntax error from the server.
//!
//! The static gates in `tests/store_sql_validation_test.rs` and the PG
//! `PREPARE` gate in `tests/pg_sql_compile_test.rs` check every statement
//! against both schemas.

#[cfg(feature = "postgres")]
use std::borrow::Cow;

/// One store statement, tagged with the backends it is valid on.
#[derive(Debug, Clone, Copy)]
pub enum Sql<'a> {
    /// Dialect-neutral text: only `?` placeholders differ between backends.
    Shared(&'a str),
    /// Explicit per-backend bodies. The Postgres text is used verbatim.
    ///
    /// Both bodies must accept **the same binds in the same order**: the
    /// executor hands one `binds` slice to either backend, so the SQLite
    /// `?` count and the Postgres `$n` maximum have to agree. The PG SQL
    /// compile gate checks this arity for every statement.
    Dual { sqlite: &'a str, pg: &'a str },
    /// Only exists on SQLite (migrations, key rotation, PRAGMA probes).
    SqliteOnly(&'a str),
    /// Only exists on Postgres (feature-gated server-specific queries).
    #[cfg(feature = "postgres")]
    PgOnly(&'a str),
}

impl<'a> Sql<'a> {
    /// The text to execute on SQLite.
    pub fn sqlite(&self) -> Result<&'a str, sqlx::Error> {
        match self {
            Self::Shared(sql) | Self::SqliteOnly(sql) => Ok(sql),
            Self::Dual { sqlite, .. } => Ok(sqlite),
            #[cfg(feature = "postgres")]
            Self::PgOnly(_) => Err(backend_mismatch(
                "statement is Postgres-only and cannot run on SQLite",
            )),
        }
    }

    /// The text to execute on Postgres. `Shared` statements are rewritten
    /// (placeholder renumbering); `Dual`/`PgOnly` bodies are returned as-is.
    #[cfg(feature = "postgres")]
    pub fn postgres(&self) -> Result<Cow<'a, str>, sqlx::Error> {
        match self {
            Self::Shared(sql) => Ok(Cow::Owned(crate::store::dialect::sqlite_to_postgres(sql))),
            Self::Dual { pg, .. } | Self::PgOnly(pg) => Ok(Cow::Borrowed(pg)),
            Self::SqliteOnly(_) => Err(backend_mismatch(
                "statement is SQLite-only and cannot run on Postgres",
            )),
        }
    }
}

#[cfg(feature = "postgres")]
fn backend_mismatch(message: &str) -> sqlx::Error {
    sqlx::Error::Configuration(message.to_string().into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_is_rewritten_for_postgres() {
        let sql = Sql::Shared("SELECT * FROM t WHERE id = ?");
        assert_eq!(sql.sqlite().unwrap(), "SELECT * FROM t WHERE id = ?");
        #[cfg(feature = "postgres")]
        assert_eq!(sql.postgres().unwrap(), "SELECT * FROM t WHERE id = $1");
    }

    #[test]
    fn dual_uses_each_body_verbatim() {
        let sql = Sql::Dual {
            sqlite: "SELECT json_extract(v, '$.id') FROM t WHERE id = ?",
            pg: "SELECT (v::jsonb ->> 'id') FROM t WHERE id = $1",
        };
        assert_eq!(
            sql.sqlite().unwrap(),
            "SELECT json_extract(v, '$.id') FROM t WHERE id = ?"
        );
        #[cfg(feature = "postgres")]
        assert_eq!(
            sql.postgres().unwrap(),
            "SELECT (v::jsonb ->> 'id') FROM t WHERE id = $1"
        );
    }

    #[test]
    fn sqlite_only_fails_closed_on_postgres() {
        let sql = Sql::SqliteOnly("PRAGMA journal_mode = WAL");
        assert_eq!(sql.sqlite().unwrap(), "PRAGMA journal_mode = WAL");
        #[cfg(feature = "postgres")]
        assert!(sql.postgres().is_err());
    }

    #[cfg(feature = "postgres")]
    #[test]
    fn pg_only_fails_closed_on_sqlite() {
        let sql = Sql::PgOnly("SELECT pg_advisory_lock(1)");
        assert!(sql.sqlite().is_err());
        assert_eq!(sql.postgres().unwrap(), "SELECT pg_advisory_lock(1)");
    }
}
