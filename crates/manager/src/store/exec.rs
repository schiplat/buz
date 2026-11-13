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

use sqlx::Column as _;
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
    /// Removed once every caller holds the `DbPool` directly.
    pub fn from_sqlite(pool: &sqlx::SqlitePool) -> Self {
        Self::Sqlite(pool.clone())
    }

    /// The SQLite pool when this is the SQLite backend. SQLite-only paths
    /// (migrations, the key-rotation CLI) use this to fail closed on Postgres
    /// instead of pretending to work.
    pub fn as_sqlite(&self) -> Option<&sqlx::SqlitePool> {
        match self {
            Self::Sqlite(pool) => Some(pool),
            #[cfg(feature = "postgres")]
            Self::Pg(_) => None,
        }
    }

    /// Configured pool capacity — the health probe reports it.
    pub fn size(&self) -> u32 {
        match self {
            Self::Sqlite(pool) => pool.size(),
            #[cfg(feature = "postgres")]
            Self::Pg(pool) => pool.size(),
        }
    }

    /// Idle connections right now — the health probe reports it.
    pub fn num_idle(&self) -> usize {
        match self {
            Self::Sqlite(pool) => pool.num_idle(),
            #[cfg(feature = "postgres")]
            Self::Pg(pool) => pool.num_idle(),
        }
    }

    /// Close every pooled connection (tests simulate a DB outage with it).
    pub async fn close(&self) {
        match self {
            Self::Sqlite(pool) => pool.close().await,
            #[cfg(feature = "postgres")]
            Self::Pg(pool) => pool.close().await,
        }
    }

    /// The dialect-aware write-transaction opener. See
    /// `transaction-boundaries.md` for the per-site Postgres translation.
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

    /// Open a plain (deferred) transaction — for write scopes that only insert
    /// and have no read-then-write invariant to serialise. Prefer
    /// `begin_immediate` wherever correctness depends on the writer lock; the
    /// boundary document enumerates those sites.
    pub async fn begin(&self) -> Result<DbTx<'_>, sqlx::Error> {
        match self {
            Self::Sqlite(pool) => Ok(DbTx::Sqlite(pool.begin().await?)),
            #[cfg(feature = "postgres")]
            Self::Pg(pool) => Ok(DbTx::Pg(pool.begin().await?)),
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

    /// Fetch every row of a statement inside this transaction.
    pub async fn fetch_all(
        &mut self,
        sql: &str,
        binds: &[SqlValue],
    ) -> Result<Vec<GenericRow>, sqlx::Error> {
        match self {
            Self::Sqlite(tx) => {
                let mut q = sqlx::query(sql);
                for b in binds {
                    q = bind_sqlite(q, b);
                }
                Ok(q.fetch_all(&mut **tx)
                    .await?
                    .into_iter()
                    .map(decode_sqlite_row)
                    .collect())
            }
            #[cfg(feature = "postgres")]
            Self::Pg(tx) => {
                let rewritten = dialect::sqlite_to_postgres(sql);
                let mut q = sqlx::query(&rewritten);
                for b in binds {
                    q = bind_pg(q, b);
                }
                Ok(q.fetch_all(&mut **tx)
                    .await?
                    .into_iter()
                    .map(decode_pg_row)
                    .collect())
            }
        }
    }

    /// Fetch at most one row of a statement inside this transaction.
    pub async fn fetch_optional(
        &mut self,
        sql: &str,
        binds: &[SqlValue],
    ) -> Result<Option<GenericRow>, sqlx::Error> {
        match self {
            Self::Sqlite(tx) => {
                let mut q = sqlx::query(sql);
                for b in binds {
                    q = bind_sqlite(q, b);
                }
                Ok(q.fetch_optional(&mut **tx).await?.map(decode_sqlite_row))
            }
            #[cfg(feature = "postgres")]
            Self::Pg(tx) => {
                let rewritten = dialect::sqlite_to_postgres(sql);
                let mut q = sqlx::query(&rewritten);
                for b in binds {
                    q = bind_pg(q, b);
                }
                Ok(q.fetch_optional(&mut **tx).await?.map(decode_pg_row))
            }
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
impl From<f64> for SqlValue {
    fn from(v: f64) -> Self {
        Self::Real(v)
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
/// Column names are captured at decode time, so wide rows address fields by
/// name (immune to projection reorders) and narrow rows may use positions.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct GenericRow {
    names: Vec<String>,
    values: Vec<SqlValue>,
}

impl GenericRow {
    fn from_parts(names: Vec<String>, values: Vec<SqlValue>) -> Self {
        Self { names, values }
    }

    /// Position of a column by name; `None` when absent (callers decide
    /// whether that is a query-shape bug worth surfacing).
    pub fn idx(&self, name: &str) -> Option<usize> {
        self.names.iter().position(|n| n == name)
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    // ── positional accessors (narrow, fixed projections) ──

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

    // ── by-name accessors (wide rows) ──

    pub fn text_of(&self, name: &str) -> String {
        self.idx(name).map(|i| self.text(i)).unwrap_or_default()
    }

    pub fn opt_text_of(&self, name: &str) -> Option<String> {
        self.idx(name).and_then(|i| self.opt_text(i))
    }

    pub fn i64_of(&self, name: &str) -> i64 {
        self.idx(name).map(|i| self.i64(i)).unwrap_or(0)
    }

    pub fn opt_i64_of(&self, name: &str) -> Option<i64> {
        self.idx(name).and_then(|i| self.opt_i64(i))
    }

    pub fn real_of(&self, name: &str) -> f64 {
        self.idx(name).map(|i| self.real(i)).unwrap_or(0.0)
    }

    pub fn is_null_at(&self, name: &str) -> bool {
        self.idx(name).map(|i| self.is_null(i)).unwrap_or(true)
    }
}

// ───────────────────── per-backend row decoding ─────────────────────

/// Type cascade: try each backend-neutral value in priority order. SQLite is
/// dynamically typed per value; PG is strict per column — the cascade works
/// on both because a column that IS the requested type decodes first, and
/// NULL falls through to `Null` (no non-Option type decodes a NULL).
fn decode_sqlite_row(row: sqlx::sqlite::SqliteRow) -> GenericRow {
    let names: Vec<String> = row.columns().iter().map(|c| c.name().to_string()).collect();
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
    GenericRow::from_parts(names, values)
}

#[cfg(feature = "postgres")]
fn decode_pg_row(row: sqlx::postgres::PgRow) -> GenericRow {
    let names: Vec<String> = row.columns().iter().map(|c| c.name().to_string()).collect();
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
    GenericRow::from_parts(names, values)
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

#[cfg(test)]
mod tests {
    use super::*;

    async fn sqlite_mem() -> DbPool {
        DbPool::Sqlite(
            sqlx::SqlitePool::connect("sqlite::memory:")
                .await
                .expect("in-memory pool"),
        )
    }

    #[tokio::test]
    async fn execute_fetch_and_by_name() {
        let pool = sqlite_mem().await;
        execute(
            &pool,
            "CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT NOT NULL, note TEXT, score REAL)",
            &[],
        )
        .await
        .unwrap();
        let n = execute(
            &pool,
            "INSERT INTO t (id, name, note, score) VALUES (?, ?, ?, ?)",
            &[SqlValue::Int(7), "seven".into(), SqlValue::Null, 0.5.into()],
        )
        .await
        .unwrap();
        assert_eq!(n, 1);

        let row = fetch_optional(
            &pool,
            "SELECT id, name, note, score FROM t WHERE id = ?",
            &[SqlValue::Int(7)],
        )
        .await
        .unwrap()
        .expect("row");

        // by-name accessors across the type cascade
        assert_eq!(row.i64_of("id"), 7);
        assert_eq!(row.text_of("name"), "seven");
        assert!(row.is_null_at("note"));
        assert_eq!(row.opt_text_of("note"), None);
        assert!((row.real_of("score") - 0.5).abs() < f64::EPSILON);
    }

    #[tokio::test]
    async fn scalar_and_bind_kinds() {
        let pool = sqlite_mem().await;
        execute(&pool, "CREATE TABLE c (v INTEGER)", &[])
            .await
            .unwrap();
        execute(&pool, "INSERT INTO c (v) VALUES (?)", &[SqlValue::Int(1)])
            .await
            .unwrap();
        execute(&pool, "INSERT INTO c (v) VALUES (?)", &[SqlValue::Int(2)])
            .await
            .unwrap();
        // bool binds as 1/0; text bind; Option<i64> Some/None
        execute(&pool, "INSERT INTO c (v) VALUES (?)", &[true.into()])
            .await
            .unwrap();
        assert_eq!(
            fetch_scalar_i64(&pool, "SELECT COUNT(*) FROM c", &[])
                .await
                .unwrap(),
            3
        );
        assert_eq!(
            fetch_scalar_i64(&pool, "SELECT v FROM c WHERE v = ?", &[SqlValue::Int(2)])
                .await
                .unwrap(),
            2
        );
        assert_eq!(
            fetch_scalar_i64(&pool, "SELECT v FROM c WHERE v = ?", &[999.into()])
                .await
                .unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn transaction_commit_and_rollback() {
        let pool = sqlite_mem().await;
        execute(&pool, "CREATE TABLE tx_t (v TEXT)", &[])
            .await
            .unwrap();

        let mut tx = pool.begin_immediate().await.unwrap();
        tx.execute("INSERT INTO missing_table (v) VALUES (?)", &[])
            .await
            .unwrap_err(); // a failed statement must not poison the tx handle
        tx.execute("INSERT INTO tx_t (v) VALUES (?)", &["kept".into()])
            .await
            .unwrap();
        tx.commit().await.unwrap();

        let mut tx = pool.begin_immediate().await.unwrap();
        tx.execute("INSERT INTO tx_t (v) VALUES (?)", &["dropped".into()])
            .await
            .unwrap();
        tx.rollback().await.unwrap();

        let rows = fetch_all(&pool, "SELECT v FROM tx_t ORDER BY v", &[])
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].text_of("v"), "kept");
    }
}
