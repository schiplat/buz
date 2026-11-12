//! Dual-backend execution core.
//!
//! `DbPool` wraps the backend pool; store functions that have completed the
//! boundary migration take it and never name the backend. Statements are
//! written in SQLite dialect; the Postgres path routes through
//! [`crate::store::dialect::sqlite_to_postgres`] at execution time.
//!
//! Rows come back as [`GenericRow`] — decoded from the backend row by runtime
//! type cascade (NULL → i64 → f64 → text → bytes) — so a renamed or reordered
//! column surfaces as a decode miss on both dialects identically.
//!
//! Migration status: modules migrate to this dispatcher one at a time
//! (`health`, `metrics` first as the proof slice); the rest still take
//! `&SqlitePool` and are tracked in HANDOFF §3.

use sqlx::Row as _;
use sqlx::ValueRef as _;

// Only the Postgres dispatch arms reference the rewriter; a default build
// (SQLite-only) would flag the import as unused.
#[cfg(feature = "postgres")]
use crate::store::dialect;

// ───────────────────────────── backend pools ─────────────────────────────

/// Either backend's connection pool. Every store function that has completed
/// the boundary migration takes this; `SqlitePool`-taking functions are
/// pre-migration and tracked in HANDOFF §3.
#[derive(Debug, Clone)]
pub enum DbPool {
    Sqlite(sqlx::SqlitePool),
    #[cfg(feature = "postgres")]
    Pg(sqlx::PgPool),
}

/// An acquired connection (pooled).
pub enum DbConn {
    Sqlite(Box<sqlx::pool::PoolConnection<sqlx::Sqlite>>),
    #[cfg(feature = "postgres")]
    Pg(Box<sqlx::pool::PoolConnection<sqlx::Postgres>>),
}

impl DbConn {
    pub async fn execute(&mut self, sql: &str, binds: &[SqlValue]) -> Result<u64, sqlx::Error> {
        match self {
            Self::Sqlite(conn) => {
                let mut q = sqlx::query(sql);
                for b in binds {
                    q = bind_sqlite(q, b);
                }
                // Box → PoolConnection → SqliteConnection: deref to the bare
                // connection, which is what implements Executor.
                q.execute(&mut ***conn).await.map(|r| r.rows_affected())
            }
            #[cfg(feature = "postgres")]
            Self::Pg(conn) => {
                // The rewritten statement outlives the query builder: bind it
                // to a named local, not a temporary.
                let rewritten = dialect::sqlite_to_postgres(sql);
                let mut q = sqlx::query(&rewritten);
                for b in binds {
                    q = bind_pg(q, b);
                }
                q.execute(&mut ***conn).await.map(|r| r.rows_affected())
            }
        }
    }
}

/// A transaction scope opened through `begin_immediate`.
pub enum DbTx<'a> {
    Sqlite(sqlx::Transaction<'a, sqlx::Sqlite>),
    #[cfg(feature = "postgres")]
    Pg(sqlx::Transaction<'a, sqlx::Postgres>),
}

impl DbPool {
    /// Bridge for call sites that still hold the app's SQLite pool: wrap it
    /// into a `DbPool` (Pool is an Arc clone, no connection is created).
    /// Removed once db::init produces a `DbPool` directly (runtime wiring).
    pub fn from_sqlite(pool: &sqlx::SqlitePool) -> Self {
        Self::Sqlite(pool.clone())
    }

    /// The dialect-aware write-transaction opener. See `store::tx`.
    pub async fn begin_immediate(&self) -> Result<DbTx<'_>, sqlx::Error> {
        match self {
            Self::Sqlite(pool) => Ok(DbTx::Sqlite(pool.begin_with("BEGIN IMMEDIATE").await?)),
            #[cfg(feature = "postgres")]
            Self::Pg(pool) => {
                // Postgres has no BEGIN IMMEDIATE: row-level locks on the
                // writes inside provide the serialisation. Per-site decisions
                // live in the boundary document.
                Ok(DbTx::Pg(pool.begin().await?))
            }
        }
    }

    pub async fn acquire(&self) -> Result<DbConn, sqlx::Error> {
        match self {
            Self::Sqlite(pool) => Ok(DbConn::Sqlite(Box::new(pool.acquire().await?))),
            #[cfg(feature = "postgres")]
            Self::Pg(pool) => Ok(DbConn::Pg(Box::new(pool.acquire().await?))),
        }
    }
}

impl DbTx<'_> {
    pub async fn commit(self) -> Result<(), sqlx::Error> {
        match self {
            Self::Sqlite(tx) => tx.commit().await,
            #[cfg(feature = "postgres")]
            Self::Pg(tx) => tx.commit().await,
        }
    }

    pub async fn rollback(self) -> Result<(), sqlx::Error> {
        match self {
            Self::Sqlite(tx) => tx.rollback().await,
            #[cfg(feature = "postgres")]
            Self::Pg(tx) => tx.rollback().await,
        }
    }

    /// Execute one statement; returns rows affected.
    pub async fn execute(&mut self, sql: &str, binds: &[SqlValue]) -> Result<u64, sqlx::Error> {
        match self {
            Self::Sqlite(tx) => {
                let mut q = sqlx::query(sql);
                for b in binds {
                    q = bind_sqlite(q, b);
                }
                q.execute(&mut **tx).await.map(|r| r.rows_affected())
            }
            #[cfg(feature = "postgres")]
            Self::Pg(tx) => {
                // The rewritten statement outlives the query builder: bind it
                // to a named local, not a temporary.
                let rewritten = dialect::sqlite_to_postgres(sql);
                let mut q = sqlx::query(&rewritten);
                for b in binds {
                    q = bind_pg(q, b);
                }
                q.execute(&mut **tx).await.map(|r| r.rows_affected())
            }
        }
    }
}

// ───────────────────────────── bind values ─────────────────────────────

/// A bound parameter, backend-agnostic. `bool` binds as 1/0 (both dialects
/// store booleans as integer columns today).
#[derive(Debug, Clone, PartialEq)]
pub enum SqlValue {
    Null,
    Text(String),
    Int(i64),
    Real(f64),
    Blob(Vec<u8>),
}

impl From<&str> for SqlValue {
    fn from(v: &str) -> Self {
        Self::Text(v.to_string())
    }
}
impl From<String> for SqlValue {
    fn from(v: String) -> Self {
        Self::Text(v)
    }
}
impl From<i64> for SqlValue {
    fn from(v: i64) -> Self {
        Self::Int(v)
    }
}
impl From<i32> for SqlValue {
    fn from(v: i32) -> Self {
        Self::Int(i64::from(v))
    }
}
impl From<bool> for SqlValue {
    fn from(v: bool) -> Self {
        Self::Int(i64::from(v))
    }
}
impl From<Option<&str>> for SqlValue {
    fn from(v: Option<&str>) -> Self {
        v.map(|s| Self::Text(s.to_string())).unwrap_or(Self::Null)
    }
}
impl From<Option<String>> for SqlValue {
    fn from(v: Option<String>) -> Self {
        v.map(Self::Text).unwrap_or(Self::Null)
    }
}
impl From<Option<i64>> for SqlValue {
    fn from(v: Option<i64>) -> Self {
        v.map(Self::Int).unwrap_or(Self::Null)
    }
}

// ───────────────────────────── generic rows ─────────────────────────────

/// A decoded result row: every column mapped to its backend-neutral value.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct GenericRow {
    values: Vec<SqlValue>,
}

impl GenericRow {
    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// Text decode: `Int`/`Real` stringify (SQLite semantics), `Null` → "".
    pub fn text(&self, i: usize) -> String {
        match self.values.get(i) {
            Some(SqlValue::Text(v)) => v.clone(),
            Some(SqlValue::Int(v)) => v.to_string(),
            Some(SqlValue::Real(v)) => v.to_string(),
            _ => String::new(),
        }
    }

    pub fn opt_text(&self, i: usize) -> Option<String> {
        match self.values.get(i) {
            Some(SqlValue::Text(v)) => Some(v.clone()),
            Some(SqlValue::Int(v)) => Some(v.to_string()),
            Some(SqlValue::Real(v)) => Some(v.to_string()),
            _ => None,
        }
    }

    /// Integer decode: `Int` as-is, `Text` parsed (SQLite stores numeric text
    /// in text columns too), `Null`/missing → 0.
    pub fn i64(&self, i: usize) -> i64 {
        match self.values.get(i) {
            Some(SqlValue::Int(v)) => *v,
            Some(SqlValue::Text(v)) => v.parse().unwrap_or(0),
            Some(SqlValue::Real(v)) => *v as i64,
            _ => 0,
        }
    }

    pub fn opt_i64(&self, i: usize) -> Option<i64> {
        match self.values.get(i) {
            Some(SqlValue::Int(v)) => Some(*v),
            Some(SqlValue::Text(v)) => Some(v.parse().unwrap_or(0)),
            Some(SqlValue::Real(v)) => Some(*v as i64),
            _ => None,
        }
    }

    pub fn real(&self, i: usize) -> f64 {
        match self.values.get(i) {
            Some(SqlValue::Real(v)) => *v,
            Some(SqlValue::Int(v)) => *v as f64,
            Some(SqlValue::Text(v)) => v.parse().unwrap_or(0.0),
            _ => 0.0,
        }
    }

    pub fn is_null(&self, i: usize) -> bool {
        matches!(self.values.get(i), None | Some(SqlValue::Null))
    }
}

// ───────────────────── per-backend row decoding ─────────────────────

/// Type cascade: try each backend-neutral value in priority order. SQLite is
/// dynamically typed per value; PG is strict per column — the cascade works
/// on both because a column that IS the requested type decodes first.
fn decode_sqlite_row(row: sqlx::sqlite::SqliteRow) -> GenericRow {
    let ncols = row.columns().len();
    let mut values = Vec::with_capacity(ncols);
    for i in 0..ncols {
        let v = if row.try_get_raw(i).map(|v| v.is_null()).unwrap_or(true) {
            SqlValue::Null
        } else if let Ok(v) = row.try_get::<i64, _>(i) {
            SqlValue::Int(v)
        } else if let Ok(v) = row.try_get::<f64, _>(i) {
            SqlValue::Real(v)
        } else if let Ok(v) = row.try_get::<String, _>(i) {
            SqlValue::Text(v)
        } else if let Ok(v) = row.try_get::<Vec<u8>, _>(i) {
            SqlValue::Blob(v)
        } else {
            SqlValue::Null
        };
        values.push(v);
    }
    GenericRow { values }
}

#[cfg(feature = "postgres")]
fn decode_pg_row(row: sqlx::postgres::PgRow) -> GenericRow {
    let ncols = row.columns().len();
    let mut values = Vec::with_capacity(ncols);
    for i in 0..ncols {
        let v = if row.try_get_raw(i).map(|v| v.is_null()).unwrap_or(true) {
            SqlValue::Null
        } else if let Ok(v) = row.try_get::<i64, _>(i) {
            SqlValue::Int(v)
        } else if let Ok(v) = row.try_get::<f64, _>(i) {
            SqlValue::Real(v)
        } else if let Ok(v) = row.try_get::<String, _>(i) {
            SqlValue::Text(v)
        } else if let Ok(v) = row.try_get::<Vec<u8>, _>(i) {
            SqlValue::Blob(v)
        } else {
            SqlValue::Null
        };
        values.push(v);
    }
    GenericRow { values }
}

// ───────────────────────────── binding ─────────────────────────────

fn bind_sqlite<'q>(
    q: sqlx::query::Query<'q, sqlx::Sqlite, sqlx::sqlite::SqliteArguments<'q>>,
    v: &'q SqlValue,
) -> sqlx::query::Query<'q, sqlx::Sqlite, sqlx::sqlite::SqliteArguments<'q>> {
    match v {
        SqlValue::Null => q.bind(None::<String>),
        SqlValue::Text(v) => q.bind(v.as_str()),
        SqlValue::Int(v) => q.bind(*v),
        SqlValue::Real(v) => q.bind(*v),
        SqlValue::Blob(v) => q.bind(v.as_slice()),
    }
}

#[cfg(feature = "postgres")]
fn bind_pg<'q>(
    q: sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments>,
    v: &'q SqlValue,
) -> sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments> {
    match v {
        SqlValue::Null => q.bind(None::<String>),
        SqlValue::Text(v) => q.bind(v.as_str()),
        SqlValue::Int(v) => q.bind(*v),
        SqlValue::Real(v) => q.bind(*v),
        SqlValue::Blob(v) => q.bind(v.as_slice()),
    }
}

// ───────────────────────────── dispatch ─────────────────────────────

/// Fetch all rows of a statement. On the Postgres path the statement is
/// rewritten first — see `store::dialect`.
pub async fn fetch_all(
    pool: &DbPool,
    sql: &str,
    binds: &[SqlValue],
) -> Result<Vec<GenericRow>, sqlx::Error> {
    match pool {
        DbPool::Sqlite(pool) => {
            let mut q = sqlx::query(sql);
            for b in binds {
                q = bind_sqlite(q, b);
            }
            Ok(q.fetch_all(pool)
                .await?
                .into_iter()
                .map(decode_sqlite_row)
                .collect())
        }
        #[cfg(feature = "postgres")]
        DbPool::Pg(pool) => {
            let rewritten = dialect::sqlite_to_postgres(sql);
            let mut q = sqlx::query(&rewritten);
            for b in binds {
                q = bind_pg(q, b);
            }
            Ok(q.fetch_all(pool)
                .await?
                .into_iter()
                .map(decode_pg_row)
                .collect())
        }
    }
}

/// Fetch at most one row.
pub async fn fetch_optional(
    pool: &DbPool,
    sql: &str,
    binds: &[SqlValue],
) -> Result<Option<GenericRow>, sqlx::Error> {
    match pool {
        DbPool::Sqlite(pool) => {
            let mut q = sqlx::query(sql);
            for b in binds {
                q = bind_sqlite(q, b);
            }
            Ok(q.fetch_optional(pool).await?.map(decode_sqlite_row))
        }
        #[cfg(feature = "postgres")]
        DbPool::Pg(pool) => {
            let rewritten = dialect::sqlite_to_postgres(sql);
            let mut q = sqlx::query(&rewritten);
            for b in binds {
                q = bind_pg(q, b);
            }
            Ok(q.fetch_optional(pool).await?.map(decode_pg_row))
        }
    }
}

/// Execute a statement; returns rows affected.
pub async fn execute(pool: &DbPool, sql: &str, binds: &[SqlValue]) -> Result<u64, sqlx::Error> {
    match pool {
        DbPool::Sqlite(pool) => {
            let mut q = sqlx::query(sql);
            for b in binds {
                q = bind_sqlite(q, b);
            }
            Ok(q.execute(pool).await?.rows_affected())
        }
        #[cfg(feature = "postgres")]
        DbPool::Pg(pool) => {
            let rewritten = dialect::sqlite_to_postgres(sql);
            let mut q = sqlx::query(&rewritten);
            for b in binds {
                q = bind_pg(q, b);
            }
            Ok(q.execute(pool).await?.rows_affected())
        }
    }
}

/// Fetch a single i64 scalar (`SELECT 1`, `COUNT(*)`, …). Implemented as a
/// one-row `fetch_optional` so all four dispatchers share the same bind
/// helpers; a missing row yields 0 (scalar statements in the store corpus are
/// aggregates that always produce exactly one row).
pub async fn fetch_scalar_i64(
    pool: &DbPool,
    sql: &str,
    binds: &[SqlValue],
) -> Result<i64, sqlx::Error> {
    Ok(fetch_optional(pool, sql, binds)
        .await?
        .map(|r| r.i64(0))
        .unwrap_or(0))
}
