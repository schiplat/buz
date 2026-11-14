//! Plugin-pool and plugin-source rows: the curator-managed template index.
//!
//! The sync/refresh/resolution logic lives in `services::plugin_pool`; this
//! module owns the table access and the row shapes both sides share.
//!
//! The pool list projections interpolate [`POOL_ROW_COLUMNS`] (one shared
//! column list, three statements) — those shapes are declared as dynamic
//! fragments in `store_sql_validation_test`.

use crate::store::exec::{self, DbPool, GenericRow, SqlValue};

use shared::error::AppError;

// ───────────────────────────── row shapes ─────────────────────────────

#[derive(Debug, Clone, serde::Serialize)]
pub struct SourceRow {
    pub id: String,
    pub name: String,
    pub url: String,
    /// Optional mirror origin — tried before `url` on every fetch. Content
    /// is sha256-pinned per file, so endpoint order is availability only.
    pub mirror_url: Option<String>,
    pub auto_enable: bool,
    pub enabled: bool,
    pub last_synced_at: Option<String>,
    pub last_sync_error: Option<String>,
    /// Highest catalog sequence this source has had VERIFIED (signed) and
    /// accepted. `None` = no signed index synced yet. The anti-rollback
    /// watermark: sync refuses anything lower.
    pub last_index_sequence: Option<i64>,
    /// Issue time of that index (diagnostic, from the signed header).
    pub last_index_issued_at: Option<String>,
}

/// The plugin's descriptive scalar meta (from a full-meta index / meta.json),
/// persisted per pool row as JSON so the dashboard can say what the plugin
/// does without downloading the template. Every field optional — legacy
/// sources carry none of it.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct PoolEntryMeta {
    pub name: Option<String>,
    pub description: Option<String>,
    pub description_zh: Option<String>,
    pub category: Option<String>,
    pub author: Option<String>,
    pub publisher: Option<String>,
    pub license: Option<String>,
    pub source_repo: Option<String>,
    pub engine: Option<String>,
    /// Deprecation notice from the source's index overlay (index/meta
    /// `deprecated`); persisted with the row so resolve can warn.
    #[serde(default)]
    pub deprecated: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct PoolRow {
    pub namespace: String,
    pub plugin_id: String,
    pub version: String,
    pub sha256: String,
    pub source_id: String,
    pub source_name: Option<String>,
    pub file_path: String,
    pub enabled: bool,
    pub reviewed: bool,
    pub added_by: Option<String>,
    /// Last curator to change `enabled`, and when. `None` = never toggled
    /// since it entered the pool (or toggled before this was recorded).
    pub updated_by: Option<String>,
    pub updated_at: Option<String>,
    /// The spec's `requires_buz` declaration, if it made one.
    pub requires_buz: Option<String>,
    /// Three-state compatibility verdict against this manager build: `None`
    /// = not checked yet (transient fetch failure at sync time), `Some(false)`
    /// = spec invalid or requires_buz unmet, `Some(true)` = passes.
    pub buz_compatible: Option<bool>,
    /// The gate-identical error behind `buz_compatible = Some(false)`.
    pub compat_error: Option<String>,
    /// Descriptive meta from the source's full-meta index (name, description,
    /// category, …). `None` = legacy source without meta.
    pub meta: Option<PoolEntryMeta>,
}

/// One existing row's compatibility state, keyed by the service.
pub struct ExistingPoolRow {
    pub namespace: String,
    pub plugin_id: String,
    pub version: String,
    pub reviewed: bool,
    pub sha256: String,
    pub requires_buz: Option<String>,
    pub buz_compatible: Option<bool>,
    pub compat_error: Option<String>,
    pub compat_checked_against: Option<String>,
}

/// Everything one pool-row upsert writes.
pub struct PoolEntryWrite<'a> {
    pub namespace: &'a str,
    pub plugin_id: &'a str,
    pub version: &'a str,
    pub sha256: &'a str,
    pub source_id: &'a str,
    pub file_path: &'a str,
    /// Only the INSERT path uses it — a re-sync never resets the curator's
    /// enable/disable verdict (see the ON CONFLICT clause).
    pub enabled: bool,
    pub reviewed: bool,
    /// `sync` | `refresh` — provenance of the first insert.
    pub added_by: &'a str,
    pub requires_buz: Option<&'a str>,
    pub buz_compatible: Option<bool>,
    pub compat_error: Option<&'a str>,
    pub compat_checked_against: Option<&'a str>,
    pub meta_json: Option<&'a str>,
}

// ───────────────────────────── pool rows ─────────────────────────────

const POOL_ROW_COLUMNS: &str = "p.namespace, p.plugin_id, p.version, p.sha256, p.source_id, s.name, \
     p.file_path, p.enabled, p.reviewed, p.added_by, p.requires_buz, p.buz_compatible, p.compat_error, \
     p.meta_json, p.updated_by, p.updated_at";

fn pool_row_from(r: GenericRow) -> PoolRow {
    PoolRow {
        namespace: r.text(0),
        plugin_id: r.text(1),
        version: r.text(2),
        sha256: r.text(3),
        source_id: r.text(4),
        source_name: r.opt_text(5),
        file_path: r.text(6),
        enabled: r.i64(7) != 0,
        reviewed: r.i64(8) != 0,
        added_by: r.opt_text(9),
        requires_buz: r.opt_text(10),
        buz_compatible: r.opt_i64(11).map(|v| v != 0),
        compat_error: r.opt_text(12),
        meta: r
            .opt_text(13)
            .and_then(|raw| serde_json::from_str(&raw).ok()),
        updated_by: r.opt_text(14),
        updated_at: r.opt_text(15),
    }
}

/// Every pool row joined with its source name.
pub async fn list_pool(pool: &DbPool) -> Result<Vec<PoolRow>, AppError> {
    Ok(exec::fetch_all(
        pool,
        &format!(
            "SELECT {POOL_ROW_COLUMNS} \
             FROM plugin_pool p LEFT JOIN plugin_sources s ON s.id = p.source_id \
             ORDER BY p.namespace, p.plugin_id, p.version"
        ),
        &[],
    )
    .await?
    .into_iter()
    .map(pool_row_from)
    .collect())
}

/// Enabled rows for one (namespace, plugin) — resolution candidates.
pub async fn enabled_rows_for_namespace(
    pool: &DbPool,
    namespace: &str,
    plugin_id: &str,
) -> Result<Vec<PoolRow>, AppError> {
    Ok(exec::fetch_all(
        pool,
        &format!(
            "SELECT {POOL_ROW_COLUMNS} \
             FROM plugin_pool p LEFT JOIN plugin_sources s ON s.id = p.source_id \
             WHERE p.namespace = ? AND p.plugin_id = ? AND p.enabled = 1"
        ),
        &[namespace.into(), plugin_id.into()],
    )
    .await?
    .into_iter()
    .map(pool_row_from)
    .collect())
}

/// Every version row of one (namespace, plugin), oldest version first.
pub async fn pool_rows_for_plugin(
    pool: &DbPool,
    namespace: &str,
    plugin_id: &str,
) -> Result<Vec<PoolRow>, AppError> {
    Ok(exec::fetch_all(
        pool,
        &format!(
            "SELECT {POOL_ROW_COLUMNS} \
             FROM plugin_pool p LEFT JOIN plugin_sources s ON s.id = p.source_id \
             WHERE p.namespace = ? AND p.plugin_id = ? \
             ORDER BY p.version"
        ),
        &[namespace.into(), plugin_id.into()],
    )
    .await?
    .into_iter()
    .map(pool_row_from)
    .collect())
}

/// Existing rows of one source — the sync's keep-or-recompute map.
pub async fn existing_rows_for_source(
    pool: &DbPool,
    source_id: &str,
) -> Result<Vec<ExistingPoolRow>, AppError> {
    Ok(exec::fetch_all(
        pool,
        "SELECT namespace, plugin_id, version, reviewed, sha256, requires_buz, \
                buz_compatible, compat_error, compat_checked_against \
         FROM plugin_pool WHERE source_id = ?",
        &[source_id.into()],
    )
    .await?
    .into_iter()
    .map(|r| ExistingPoolRow {
        namespace: r.text(0),
        plugin_id: r.text(1),
        version: r.text(2),
        reviewed: r.i64(3) != 0,
        sha256: r.text(4),
        requires_buz: r.opt_text(5),
        buz_compatible: r.opt_i64(6).map(|v| v != 0),
        compat_error: r.opt_text(7),
        compat_checked_against: r.opt_text(8),
    })
    .collect())
}

/// `(source_id, file_path)` of one plugin's rows (`None` = not in the pool).
pub async fn pool_source_of(
    pool: &DbPool,
    namespace: &str,
    plugin_id: &str,
) -> Result<Option<(String, String)>, AppError> {
    Ok(exec::fetch_optional(
        pool,
        "SELECT source_id, file_path FROM plugin_pool WHERE namespace = ? AND plugin_id = ? LIMIT 1",
        &[namespace.into(), plugin_id.into()],
    )
    .await?
    .map(|r| (r.text(0), r.text(1))))
}

/// `(sha256, source_id, file_path)` of one exact pool entry.
pub async fn pool_entry_facts(
    pool: &DbPool,
    namespace: &str,
    plugin_id: &str,
    version: &str,
) -> Result<Option<(String, String, String)>, AppError> {
    Ok(exec::fetch_optional(
        pool,
        "SELECT sha256, source_id, file_path FROM plugin_pool \
         WHERE namespace = ? AND plugin_id = ? AND version = ?",
        &[namespace.into(), plugin_id.into(), version.into()],
    )
    .await?
    .map(|r| (r.text(0), r.text(1), r.text(2))))
}

/// Insert or refresh one pool row. `enabled` applies on INSERT only — a
/// re-sync must never reset the curator's verdict.
pub async fn upsert_pool_entry(pool: &DbPool, entry: &PoolEntryWrite<'_>) -> Result<(), AppError> {
    exec::execute(
        pool,
        "INSERT INTO plugin_pool (namespace, plugin_id, version, sha256, source_id, file_path, \
                                enabled, reviewed, added_by, requires_buz, buz_compatible, \
                                compat_error, compat_checked_against, meta_json)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(namespace, plugin_id, version) DO UPDATE SET
           sha256 = excluded.sha256,
           source_id = excluded.source_id,
           file_path = excluded.file_path,
           reviewed = excluded.reviewed,
           requires_buz = excluded.requires_buz,
           buz_compatible = excluded.buz_compatible,
           compat_error = excluded.compat_error,
           compat_checked_against = excluded.compat_checked_against,
           meta_json = excluded.meta_json",
        &[
            entry.namespace.into(),
            entry.plugin_id.into(),
            entry.version.into(),
            entry.sha256.into(),
            entry.source_id.into(),
            entry.file_path.into(),
            entry.enabled.into(),
            entry.reviewed.into(),
            entry.added_by.into(),
            entry.requires_buz.into(),
            match entry.buz_compatible {
                Some(b) => SqlValue::Int(i64::from(b)),
                None => SqlValue::Null,
            },
            entry.compat_error.into(),
            entry.compat_checked_against.into(),
            entry.meta_json.into(),
        ],
    )
    .await?;
    Ok(())
}

// ───────────────────────────── source rows ─────────────────────────────

/// All plugin sources, creation-ordered.
pub async fn list_sources(pool: &DbPool) -> Result<Vec<SourceRow>, AppError> {
    Ok(exec::fetch_all(
        pool,
        "SELECT id, name, url, mirror_url, auto_enable, enabled, last_synced_at, last_sync_error, \
                last_index_sequence, last_index_issued_at \
             FROM plugin_sources ORDER BY created_at",
        &[],
    )
    .await?
    .into_iter()
    .map(|r| SourceRow {
        id: r.text(0),
        name: r.text(1),
        url: r.text(2),
        mirror_url: r.opt_text(3),
        auto_enable: r.i64(4) != 0,
        enabled: r.i64(5) != 0,
        last_synced_at: r.opt_text(6),
        last_sync_error: r.opt_text(7),
        last_index_sequence: r.opt_i64(8),
        last_index_issued_at: r.opt_text(9),
    })
    .collect())
}

/// Record one sync round's outcome on its source (sub-second stamp; success
/// clears the previous error).
pub async fn mark_source_synced(
    pool: &DbPool,
    source_id: &str,
    stamp: &str,
    ok: bool,
    error: Option<&str>,
) -> Result<(), AppError> {
    exec::execute(
        pool,
        "UPDATE plugin_sources SET last_synced_at = ?, \
         enabled = ?, last_sync_error = ? WHERE id = ?",
        &[
            stamp.into(),
            SqlValue::Int(i64::from(ok)),
            error.into(),
            source_id.into(),
        ],
    )
    .await?;
    Ok(())
}

/// `(last_index_sequence, last_index_sha256)` — the anti-rollback watermark.
pub async fn index_watermark(
    pool: &DbPool,
    source_id: &str,
) -> Result<Option<(Option<i64>, Option<String>)>, AppError> {
    Ok(exec::fetch_optional(
        pool,
        "SELECT last_index_sequence, last_index_sha256 FROM plugin_sources WHERE id = ?",
        &[source_id.into()],
    )
    .await?
    .map(|r| (r.opt_i64(0), r.opt_text(1))))
}

/// Advance the watermark to a verified signed index.
pub async fn advance_index_watermark(
    pool: &DbPool,
    source_id: &str,
    sequence: i64,
    sha256: &str,
    issued_at: Option<&str>,
) -> Result<(), AppError> {
    exec::execute(
        pool,
        "UPDATE plugin_sources SET last_index_sequence = ?, last_index_sha256 = ?, \
         last_index_issued_at = ? WHERE id = ?",
        &[
            SqlValue::Int(sequence),
            sha256.into(),
            issued_at.into(),
            source_id.into(),
        ],
    )
    .await?;
    Ok(())
}

/// One enabled pool entry (the curator's allowlist for remote sources).
pub struct EnabledEntry {
    pub namespace: String,
    pub plugin_id: String,
    pub version: String,
    pub sha256: String,
}

/// Enabled pool rows — the `uses:` availability check.
pub async fn enabled_entries(pool: &DbPool) -> Result<Vec<EnabledEntry>, AppError> {
    Ok(exec::fetch_all(
        pool,
        "SELECT namespace, plugin_id, version, sha256 FROM plugin_pool WHERE enabled = 1",
        &[],
    )
    .await?
    .into_iter()
    .map(|r| EnabledEntry {
        namespace: r.text(0),
        plugin_id: r.text(1),
        version: r.text(2),
        sha256: r.text(3),
    })
    .collect())
}
