//! Step marketplace pool: remote sources, their index.json, and the curated
//! template pool (ADR plugin-marketplace §6.8).
//!
//! A **source** is an `index.json` URL (schema §6.7.3: steps[] with id
//! `<namespace>/<plugin-id>`, version, file, sha256, plus full scalar meta
//! incl. `requires_buz` from newer generators). Syncing fetches the
//! index, validates it, and stages every entry as a **pool row**
//! (`enabled = auto_enable`). Pool rows — not the index — are what pipeline
//! saves resolve against; each staged template is probed once (download +
//! sha256 + gate evaluation, content-cached) so listed-but-missing files
//! surface as red pool rows, and template content is fetched on demand,
//! verified against the pinned sha256, and cached content-addressed under
//! `<data_dir>/plugin-index-cache/templates/`.

use anyhow::anyhow;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;
use std::path::PathBuf;

use crate::services::plugin_resolver::{self, ResolutionPolicy, ResolvedPlugin};

// URL helpers ------------------------------------------------------------------

/// Base URL of a source: the index URL minus its `index.json` tail.
pub fn base_url(index_url: &str) -> String {
    let trimmed = index_url.trim_end_matches('/');
    match trimmed.strip_suffix("index.json") {
        Some(without) => without.trim_end_matches('/').to_string(),
        None => trimmed.to_string(),
    }
}

fn join_url(base: &str, rest: &str) -> String {
    format!(
        "{}/{}",
        base.trim_end_matches('/'),
        rest.trim_start_matches('/')
    )
}

/// Ordered fetch bases for a source: the mirror first (when configured),
/// then the canonical origin.
fn source_bases(source: &SourceRow) -> Vec<String> {
    let mut bases = Vec::new();
    if let Some(mirror) = source
        .mirror_url
        .as_deref()
        .map(str::trim)
        .filter(|mirror| !mirror.is_empty())
    {
        bases.push(base_url(mirror));
    }
    bases.push(base_url(&source.url));
    bases
}

/// GET a source-relative path from the first endpoint that answers. Every
/// caller hash-verifies content, so endpoint order affects availability
/// only — never integrity.
async fn http_get_text_from_bases(
    http: &reqwest::Client,
    bases: &[String],
    path: &str,
) -> anyhow::Result<String> {
    let mut last = None;
    for base in bases {
        let url = join_url(base, path);
        match http_get_text(http, &url).await {
            Ok(text) => return Ok(text),
            Err(e) => {
                // Expected while failing over between mirror and origin; the
                // error surfaces to the caller when every endpoint fails.
                tracing::debug!(
                    url = %url,
                    error = %e,
                    "source endpoint failed — trying the next configured endpoint"
                );
                last = Some(e);
            }
        }
    }
    match last {
        Some(e) => Err(e),
        None => Err(anyhow!("no source endpoints configured")),
    }
}

pub fn sha256_hex(raw: &str) -> String {
    format!("{:x}", Sha256::digest(raw.as_bytes()))
}

/// Semver-ish sort key mirroring the dashboard's version ordering; versions
/// that do not parse sort oldest.
fn version_sort_key(version: &str) -> (u64, u64, u64) {
    let mut parts = version.split('.');
    let mut next = || {
        parts
            .next()
            .and_then(|part| part.parse::<u64>().ok())
            .unwrap_or(0)
    };
    (next(), next(), next())
}

// Rows ------------------------------------------------------------------

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

#[allow(clippy::too_many_arguments)] // one row of pool settings; a struct here would only add noise
fn meta_json_string(
    category: &Option<String>,
    name: &Option<String>,
    description: &Option<String>,
    description_zh: &Option<String>,
    author: &Option<String>,
    publisher: &Option<String>,
    license: &Option<String>,
    source_repo: &Option<String>,
    engine: &Option<String>,
    deprecated: &Option<String>,
) -> Option<String> {
    let meta = PoolEntryMeta {
        name: name.clone(),
        description: description.clone(),
        description_zh: description_zh.clone(),
        category: category.clone(),
        author: author.clone(),
        publisher: publisher.clone(),
        license: license.clone(),
        source_repo: source_repo.clone(),
        engine: engine.clone(),
        deprecated: deprecated.clone(),
    };
    serde_json::to_string(&meta).ok()
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

/// Column tuple every plugin_pool row projection reads; `pool_row_from` is the
/// single mapping so the three SELECT sites can't drift apart.
type PoolRowSql = (
    String,
    String,
    String,
    String,
    String,
    Option<String>,
    String,
    i64,
    i64,
    Option<String>,
    Option<String>,
    Option<i64>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

const POOL_ROW_COLUMNS: &str = "p.namespace, p.plugin_id, p.version, p.sha256, p.source_id, s.name, \
     p.file_path, p.enabled, p.reviewed, p.added_by, p.requires_buz, p.buz_compatible, p.compat_error, \
     p.meta_json, p.updated_by, p.updated_at";

fn pool_row_from(
    (
        namespace,
        plugin_id,
        version,
        sha256,
        source_id,
        source_name,
        file_path,
        enabled,
        reviewed,
        added_by,
        requires_buz,
        buz_compatible,
        compat_error,
        meta_json,
        updated_by,
        updated_at,
    ): PoolRowSql,
) -> PoolRow {
    PoolRow {
        namespace,
        plugin_id,
        version,
        sha256,
        source_id,
        source_name,
        file_path,
        enabled: enabled != 0,
        reviewed: reviewed != 0,
        added_by,
        updated_by,
        updated_at,
        requires_buz,
        buz_compatible: buz_compatible.map(|v| v != 0),
        compat_error,
        meta: meta_json.and_then(|raw| serde_json::from_str(&raw).ok()),
    }
}

/// A resolved remote reference: everything `uses:` needs to expand.
pub struct RemoteResolved {
    pub namespace: String,
    pub plugin_id: String,
    pub version: String,
    pub sha256: String,
    pub source_url: String,
    pub file_path: String,
    pub reviewed: bool,
    pub content: String,
    /// From the row's persisted index meta (overlay `deprecated`).
    pub deprecated: Option<String>,
}

/// index.json -----------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct PluginIndex {
    /// Strictly increasing catalog version. Inside the signed bytes on
    /// purpose: it is the anti-rollback anchor (a number stored beside the
    /// signature could be raised without invalidating it).
    #[serde(default)]
    pub sequence: Option<u64>,
    /// Issue time of this catalog version (diagnostic; signed with the rest).
    #[serde(default)]
    pub issued_at: Option<String>,
    #[serde(default = "Vec::new")]
    pub steps: Vec<IndexEntry>,
}

#[derive(Debug, Deserialize)]
pub struct IndexEntry {
    pub id: String,
    pub version: String,
    pub file: String,
    pub sha256: String,
    /// Full-meta sources (§6.7.3) publish the spec's contract gate so sync
    /// can rule on compatibility without downloading the template. Older
    /// sources omit it → fetch-and-evaluate fallback.
    pub requires_buz: Option<String>,
    /// Deprecation notice carried by the source's index overlay.
    #[serde(default)]
    pub deprecated: Option<String>,
    /// Full-meta scalar fields — persisted onto the pool row so the dashboard
    /// can describe the plugin. Older sources omit them; the UI falls back to
    /// the raw id.
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub description_zh: Option<String>,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub publisher: Option<String>,
    #[serde(default)]
    pub license: Option<String>,
    #[serde(default)]
    pub source_repo: Option<String>,
    #[serde(default)]
    pub engine: Option<String>,
}

/// When an index fails to parse, append a short printable prefix of what the
/// endpoint actually returned. The sources list surfaces the message
/// verbatim, so the common failure — a 200 HTML page from a SPA fallback —
/// becomes self-evident instead of a bare serde position.
fn unexpected_body_hint(raw: &str) -> String {
    let trimmed = raw.trim_start();
    if trimmed.is_empty() {
        return " — the endpoint returned an empty body".to_string();
    }
    let hint = if trimmed.starts_with('<') {
        " — the body looks like HTML: the URL is probably serving a page (wrong path, or a fallback page for missing files) instead of JSON"
    } else if !(trimmed.starts_with('{') || trimmed.starts_with('[')) {
        " — the body does not look like JSON"
    } else {
        ""
    };
    const MAX_CHARS: usize = 240;
    let flattened: String = raw
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let truncated: String = flattened.chars().take(MAX_CHARS).collect();
    let suffix = if flattened.chars().count() > MAX_CHARS {
        " …(truncated)"
    } else {
        ""
    };
    format!(" — response body: {truncated}{suffix}{hint}")
}

pub fn parse_index(raw: &str) -> Result<PluginIndex, String> {
    let index: PluginIndex =
        serde_json::from_str(raw).map_err(|e| format!("invalid index.json: {e}"))?;
    let mut seen = std::collections::HashSet::new();
    let mut namespace: Option<String> = None;
    for entry in &index.steps {
        if entry.id.is_empty()
            || entry.version.is_empty()
            || entry.file.is_empty()
            || entry.sha256.len() != 64
        {
            return Err(format!("invalid index entry {}", entry.id));
        }
        // One index per namespace directory (`plugins/<ns>/index.json`): the
        // directory owning the namespace is the publishing trust boundary.
        let ns = match entry.id.split_once('/') {
            Some((ns, id)) if !ns.is_empty() && !id.is_empty() => ns.to_string(),
            _ => {
                return Err(format!(
                    "invalid index entry {}: id must be '<namespace>/<plugin>'",
                    entry.id
                ))
            }
        };
        match &namespace {
            Some(existing) if existing != &ns => {
                return Err(format!(
                    "index mixes namespaces '{existing}' and '{ns}' — one index per namespace directory"
                ));
            }
            None => namespace = Some(ns),
            _ => {}
        }
        if !seen.insert((entry.id.clone(), entry.version.clone())) {
            return Err(format!(
                "duplicate index entry {}@{}",
                entry.id, entry.version
            ));
        }
    }
    Ok(index)
}

// DB access ------------------------------------------------------------------

pub async fn list_sources(pool: &SqlitePool) -> Result<Vec<SourceRow>, anyhow::Error> {
    #[allow(clippy::type_complexity)] // sqlx row tuple mirrors the SELECT column order
    let rows: Vec<(
        String,
        String,
        String,
        Option<String>,
        i64,
        i64,
        Option<String>,
        Option<String>,
        Option<i64>,
        Option<String>,
    )> = sqlx::query_as(
        "SELECT id, name, url, mirror_url, auto_enable, enabled, last_synced_at, last_sync_error, \
                last_index_sequence, last_index_issued_at \
             FROM plugin_sources ORDER BY created_at",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(
            |(
                id,
                name,
                url,
                mirror_url,
                auto_enable,
                enabled,
                last_synced_at,
                last_sync_error,
                last_index_sequence,
                last_index_issued_at,
            )| SourceRow {
                id,
                name,
                url,
                mirror_url,
                auto_enable: auto_enable != 0,
                enabled: enabled != 0,
                last_synced_at,
                last_sync_error,
                last_index_sequence,
                last_index_issued_at,
            },
        )
        .collect())
}

pub async fn get_source(pool: &SqlitePool, id: &str) -> Result<Option<SourceRow>, anyhow::Error> {
    Ok(list_sources(pool).await?.into_iter().find(|s| s.id == id))
}

pub async fn list_pool(pool: &SqlitePool) -> Result<Vec<PoolRow>, anyhow::Error> {
    let rows: Vec<PoolRowSql> = sqlx::query_as(&format!(
        "SELECT {POOL_ROW_COLUMNS} \
         FROM plugin_pool p LEFT JOIN plugin_sources s ON s.id = p.source_id \
         ORDER BY p.namespace, p.plugin_id, p.version"
    ))
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(pool_row_from).collect())
}

/// Enabled pool rows for one namespace (resolution candidates).
async fn enabled_rows_for_namespace(
    pool: &SqlitePool,
    namespace: &str,
    plugin_id: &str,
) -> Result<Vec<PoolRow>, anyhow::Error> {
    let rows: Vec<PoolRowSql> = sqlx::query_as(&format!(
        "SELECT {POOL_ROW_COLUMNS} \
         FROM plugin_pool p LEFT JOIN plugin_sources s ON s.id = p.source_id \
         WHERE p.namespace = ? AND p.plugin_id = ? AND p.enabled = 1",
    ))
    .bind(namespace)
    .bind(plugin_id)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(pool_row_from).collect())
}

// Sync ------------------------------------------------------------------

/// Per-row state a sync round needs: the review badge, the pinned sha256,
/// and the cached compatibility verdict plus the manager version it was
/// computed against.
#[derive(Clone)]
struct ExistingRowState {
    reviewed: bool,
    sha256: String,
    requires_buz: Option<String>,
    buz_compatible: Option<bool>,
    compat_error: Option<String>,
    compat_checked_against: Option<String>,
}

async fn existing_rows_for_source(
    pool: &SqlitePool,
    source_id: &str,
) -> Result<std::collections::HashMap<(String, String, String), ExistingRowState>, anyhow::Error> {
    #[allow(clippy::type_complexity)] // sqlx row tuple mirrors the SELECT column order
    let rows: Vec<(
        String,
        String,
        String,
        i64,
        String,
        Option<String>,
        Option<i64>,
        Option<String>,
        Option<String>,
    )> = sqlx::query_as(
        "SELECT namespace, plugin_id, version, reviewed, sha256, requires_buz, \
                buz_compatible, compat_error, compat_checked_against \
         FROM plugin_pool WHERE source_id = ?",
    )
    .bind(source_id)
    .fetch_all(pool)
    .await
    .map_err(|e| anyhow!("{e}"))?;
    Ok(rows
        .into_iter()
        .map(
            |(ns, id, v, reviewed, sha256, requires_buz, buz_compatible, compat_error, checked)| {
                (
                    (ns, id, v),
                    ExistingRowState {
                        reviewed: reviewed != 0,
                        sha256,
                        requires_buz,
                        buz_compatible: buz_compatible.map(|b| b != 0),
                        compat_error,
                        compat_checked_against: checked,
                    },
                )
            },
        )
        .collect())
}

/// Compatibility verdict to persist for one entry: recomputed when the row is
/// new, its pinned sha256 changed, or the manager version moved since the
/// last check (startup sync then re-verifies every entry after an upgrade);
/// otherwise the cached verdict is kept.
///
/// A re-check is a **probe**: the template is downloaded once (sha256-verified
/// and content-address cached) and runs through the full gate evaluation, so
/// a listed-but-missing file, a drifted mirror, or an invalid spec all turn
/// into a red verdict at sync time instead of a surprise at enable time.
/// Steady-state syncs — nothing staged, nothing changed — keep the cached
/// verdict and stay at zero template requests; the cache also absorbs
/// manager-upgrade re-checks (unchanged content = cache hit).
/// `declared_requires` (from the source's full-meta index.json / meta.json)
/// only disambiguates a probe failure: an index whose gate is already unmet
/// reports that; a gate-satisfying entry with a dead file reports the file
/// problem. Sources without the field leave the verdict unknown (NULL) on
/// probe failure — retried on the next sync.
/// Returns `(requires_buz, buz_compatible, compat_error, checked_against)`.
/// Compatibility verdict for one index entry.
///
/// `verify_content` decides whether the template is actually downloaded. Only
/// the newest version of a plugin is verified at sync time — older versions
/// are staged as "unknown" and verified when someone enables them or pins
/// them in a pipeline. A `requires_buz` declared by the index is evaluated
/// without any download, so an obviously incompatible version still enters
/// the pool flagged rather than unknown.
#[allow(clippy::too_many_arguments)]
async fn resolve_compat(
    http: &reqwest::Client,
    bases: &[String],
    file_path: &str,
    sha256: &str,
    namespace: &str,
    plugin_id: &str,
    version: &str,
    current_buz: &str,
    existing: Option<&ExistingRowState>,
    declared_requires: Option<&str>,
    verify_content: bool,
) -> (Option<String>, Option<bool>, Option<String>, Option<String>) {
    let needs_check = match existing {
        None => true,
        Some(state) => {
            state.sha256 != sha256 || state.compat_checked_against.as_deref() != Some(current_buz)
        }
    };
    if !needs_check {
        let state = existing.expect("checked existing above");
        return (
            state.requires_buz.clone(),
            state.buz_compatible,
            state.compat_error.clone(),
            state.compat_checked_against.clone(),
        );
    }
    // Cheap, exact gate: the index already declares the contract requirement,
    // so an unsatisfied one needs no download to be reported.
    if let Some(requires) = declared_requires {
        if shared::plugin_spec::buz_requirement_satisfied(requires, current_buz).is_err() {
            let error = shared::plugin_spec::buz_requirement_satisfied(requires, current_buz)
                .err()
                .unwrap_or_default();
            return (
                Some(requires.to_string()),
                Some(false),
                Some(format!("requires_buz: {error}")),
                Some(current_buz.to_string()),
            );
        }
    }
    if !verify_content {
        // Staged but unverified: the verdict arrives when it is enabled or
        // resolved, both of which download and gate the content.
        return (declared_requires.map(str::to_string), None, None, None);
    }
    match fetch_content(http, bases, file_path, sha256).await {
        Ok(content) => {
            match evaluate_entry_content(&content, namespace, plugin_id, version, current_buz) {
                Ok(requires_buz) => (
                    requires_buz,
                    Some(true),
                    None,
                    Some(current_buz.to_string()),
                ),
                Err(error) => (
                    None,
                    Some(false),
                    Some(error),
                    Some(current_buz.to_string()),
                ),
            }
        }
        Err(fetch_error) => {
            let incompatible_per_index = declared_requires
                .map(|requires| {
                    shared::plugin_spec::buz_requirement_satisfied(requires, current_buz).is_err()
                })
                .unwrap_or(false);
            match declared_requires {
                // Same message the enable gate reports (validate_plugin_spec
                // wraps the requirement error with the `requires_buz:`
                // prefix), so the list hint and a gate refusal never disagree.
                Some(requires) if incompatible_per_index => {
                    let error =
                        shared::plugin_spec::buz_requirement_satisfied(requires, current_buz)
                            .unwrap_err();
                    (
                        Some(requires.to_string()),
                        Some(false),
                        Some(format!("requires_buz: {error}")),
                        Some(current_buz.to_string()),
                    )
                }
                // The gate is satisfiable per the index — the missing file is
                // the red flag. Persisted (not NULL) so the curator sees the
                // half-published mirror immediately instead of at enable time.
                Some(requires) => {
                    tracing::debug!(
                        plugin = %format!("{namespace}/{plugin_id}@{version}"),
                        error = %fetch_error,
                        "template probe failed — source lists the file but it does not download"
                    );
                    (
                        Some(requires.to_string()),
                        Some(false),
                        Some(format!(
                            "template file is listed in the source index but failed to download: {fetch_error}"
                        )),
                        Some(current_buz.to_string()),
                    )
                }
                None => {
                    tracing::debug!(
                        plugin = %format!("{namespace}/{plugin_id}@{version}"),
                        error = %fetch_error,
                        "template fetch failed during sync — compatibility stays unknown"
                    );
                    (None, None, None, None)
                }
            }
        }
    }
}

/// Fetch `index.json`, validate it, and upsert every entry as a pool row
/// (`enabled = auto_enable`). Returns the number of entries. Live progress
/// lands in `services::sync_progress` for the dashboard's progress bar.
pub async fn sync_source(
    http: &reqwest::Client,
    pool: &SqlitePool,
    source: &SourceRow,
    current_buz: &str,
) -> Result<usize, anyhow::Error> {
    super::sync_progress::begin(&source.id);
    let result = sync_source_inner(http, pool, source, None, current_buz).await;
    super::sync_progress::finish(&source.id, result.as_ref().err().map(|e| e.to_string()));
    let (ok, error) = match &result {
        Ok(_) => (true, None),
        Err(e) => (false, Some(e.to_string())),
    };
    // Sub-second timestamp: the UI detects "sync finished" by comparing
    // last_synced_at with the value seen at click time, and second
    // precision makes back-to-back syncs indistinguishable. Success also
    // clears any stale error from a previous failed attempt.
    let stamp = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let _ = sqlx::query(
        "UPDATE plugin_sources SET last_synced_at = ?, \
         enabled = ?, last_sync_error = ? WHERE id = ?",
    )
    .bind(&stamp)
    .bind(ok as i64)
    .bind(error)
    .bind(&source.id)
    .execute(pool)
    .await;
    result
}

/// Effective signature policy: `(require, public_key)` — the dashboard value
/// when set, otherwise the `manager.toml` default. Read per call (not cached in
/// a process-wide slot) so one database's override never leaks into another.
async fn signature_policy(pool: &SqlitePool) -> (bool, String) {
    let (default_require, default_key) = crate::services::plugin_resolver::signature_defaults();
    let require = crate::services::platform_settings::effective(
        &crate::store::exec::DbPool::from_sqlite(pool),
        crate::services::platform_settings::KEY_PLUGIN_REQUIRE_SIGNED_INDEX,
        if default_require { "true" } else { "false" },
    )
    .await
    .trim()
        == "true";
    let public_key = crate::services::platform_settings::effective(
        &crate::store::exec::DbPool::from_sqlite(pool),
        crate::services::platform_settings::KEY_PLUGIN_INDEX_PUBLIC_KEY,
        &default_key,
    )
    .await;
    (require, public_key)
}

/// Enforce the trust anchor on a fetched index. No-op unless
/// `plugin_require_signed_index` is on, in which case a source must publish
/// `index.json.sig` that verifies against the configured public key — that is
/// what stops a mirror (or anyone on the network) from rewriting both the
/// index and the hashes inside it.
///
/// Returns the verifying key id when the index WAS verified: only an
/// authentic index may advance the anti-rollback watermark (see
/// [`enforce_index_freshness`]), and only then is the review ledger required
/// to carry its own signature too (see [`verified_reviews`]).
async fn verify_index_signature(
    http: &reqwest::Client,
    bases: &[String],
    index_text: &str,
    require: bool,
    public_key: &str,
    source_name: &str,
) -> anyhow::Result<Option<String>> {
    if !require {
        return Ok(None);
    }
    if public_key.trim().is_empty() {
        anyhow::bail!(
            "source '{source_name}' must be signed, but no plugin_index_public_key is configured              (Settings → Review gate → Index signature)"
        );
    }
    let signature = http_get_text_from_bases(http, bases, shared::plugin_signature::SIGNATURE_FILE)
        .await
        .map_err(|e| {
            anyhow!(
                "source '{source_name}' index is not signed — publish index.json.sig \
             (`buz-plugin index --sign-key`) or turn off the required-signature setting: {e}"
            )
        })?;
    let key_id = shared::plugin_signature::verify_document(
        index_text.as_bytes(),
        &signature,
        public_key,
        shared::plugin_signature::SIGNATURE_FILE,
    )
    .map_err(|e| anyhow!("source '{source_name}' index signature rejected: {e}"))?;
    tracing::info!(source = %source_name, key_id = %key_id, "index signature verified");
    Ok(Some(key_id))
}

/// Under the trust anchor, a review ledger is trusted only with its own
/// signature: an unsigned or forged `reviews.json` could mark attacker-chosen
/// hashes as human-reviewed and walk straight through `require_reviewed`.
/// The ledger and its `reviews.json.sig` are produced by the same
/// `buz-plugin index --sign-key` run, so a source that publishes a ledger
/// publishes the signature next to it. Without the trust anchor the ledger is
/// consumed as-is (the review gate is then the operator's own choice).
async fn verified_reviews(
    http: &reqwest::Client,
    bases: &[String],
    ledger: &str,
    source_name: &str,
    public_key: &str,
) -> anyhow::Result<()> {
    let signature = http_get_text_from_bases(
        http,
        bases,
        shared::plugin_signature::REVIEWS_SIGNATURE_FILE,
    )
    .await
    .map_err(|e| {
        anyhow!(
            "source '{source_name}' publishes reviews.json without its signature — publish \
             reviews.json.sig alongside (buz-plugin index --sign-key signs index and ledger \
             together) or turn off the required-signature setting: {e}"
        )
    })?;
    shared::plugin_signature::verify_document(
        ledger.as_bytes(),
        &signature,
        public_key,
        shared::plugin_signature::REVIEWS_SIGNATURE_FILE,
    )
    .map_err(|e| anyhow!("source '{source_name}' reviews.json signature rejected: {e}"))?;
    Ok(())
}

/// Anti-rollback watermark.
///
/// A signature proves authenticity, not freshness: an attacker who kept an
/// older signed `index.json` (and the equally signed template hashes inside
/// it) can serve that pair forever, and every hash check would still pass.
/// The signed `sequence` is the missing ordering — a source only ever moves
/// forward. Rules:
///
/// - no `sequence` in a signed index → refused (regenerate with a tool that
///   writes one; accepting it would leave the replay window open);
/// - lower than the accepted sequence → refused (rollback);
/// - equal sequence but different bytes → refused (a publisher must not sign
///   two catalogs under one sequence);
/// - equal sequence, same bytes → idempotent re-sync, accepted.
///
/// The watermark is only advanced for an index that passed signature
/// verification, so an unsigned source (or a deployment with the trust anchor
/// off) never poisons it.
async fn enforce_index_freshness(
    pool: &SqlitePool,
    source: &SourceRow,
    index: &PluginIndex,
    raw: &str,
) -> anyhow::Result<()> {
    let Some(sequence) = index.sequence else {
        anyhow::bail!(
            "source '{}' index is signed but has no `sequence` — rollback protection needs it. \
             Regenerate the index with a buz-plugin that writes one (`buz-plugin index --sign-key …`)",
            source.name
        );
    };
    let sequence = i64::try_from(sequence).map_err(|_| {
        anyhow!(
            "source '{}' index declares an out-of-range sequence {sequence}",
            source.name
        )
    })?;
    let incoming_sha = sha256_hex(raw);
    let stored: Option<(Option<i64>, Option<String>)> = sqlx::query_as(
        "SELECT last_index_sequence, last_index_sha256 FROM plugin_sources WHERE id = ?",
    )
    .bind(&source.id)
    .fetch_optional(pool)
    .await?;
    let (accepted_sequence, accepted_sha) = stored.unwrap_or((None, None));
    if let Some(accepted) = accepted_sequence {
        if sequence < accepted {
            anyhow::bail!(
                "index rollback refused for source '{}': it already accepted sequence {accepted}, \
                 but this index reports sequence {sequence}. A validly signed but older catalog is \
                 being replayed. If the publisher intentionally restarted its sequence, reset this \
                 source's index watermark first",
                source.name
            );
        }
        if sequence == accepted && accepted_sha.as_deref() != Some(incoming_sha.as_str()) {
            anyhow::bail!(
                "source '{}' index reports sequence {sequence} with different content than the \
                 index already accepted at that sequence — a publisher must not sign two catalogs \
                 under one sequence. Re-issue the index with a higher sequence",
                source.name
            );
        }
    }
    sqlx::query(
        "UPDATE plugin_sources SET last_index_sequence = ?, last_index_sha256 = ?, \
         last_index_issued_at = ? WHERE id = ?",
    )
    .bind(sequence)
    .bind(&incoming_sha)
    .bind(&index.issued_at)
    .bind(&source.id)
    .execute(pool)
    .await?;
    if accepted_sequence != Some(sequence) {
        tracing::info!(
            source = %source.name,
            sequence,
            issued_at = index.issued_at.as_deref().unwrap_or("unknown"),
            "index catalog advanced"
        );
    }
    Ok(())
}

async fn sync_source_inner(
    http: &reqwest::Client,
    pool: &SqlitePool,
    source: &SourceRow,
    only_plugin: Option<(&str, &str)>,
    current_buz: &str,
) -> Result<usize, anyhow::Error> {
    let bases = source_bases(source);
    let raw = http_get_text_from_bases(http, &bases, "index.json").await?;
    let (require_signed, public_key) = signature_policy(pool).await;
    let verified = verify_index_signature(
        http,
        &bases,
        &raw,
        require_signed,
        &public_key,
        &source.name,
    )
    .await?;
    let index = parse_index(&raw)
        .map_err(|e| anyhow!("invalid index.json: {e}{}", unexpected_body_hint(&raw)))?;
    if verified.is_some() {
        // Authentic index → its sequence may move the watermark. Rejected
        // before any pool row is written, so a replay changes nothing.
        enforce_index_freshness(pool, source, &index, &raw).await?;
    }

    // Official-style sources publish reviews.json next to the index; when it
    // is fetchable, pool rows carry the review badge by hash membership.
    // When it is NOT fetchable, `reviewed` keeps each row's CURRENT value —
    // a transient fetch failure must never wipe review badges. Under the
    // trust anchor a fetched ledger must carry its own signature, so the
    // badges cannot be forged by rewriting the ledger alone.
    let fetched_reviews: Option<std::collections::HashSet<String>> =
        match http_get_text_from_bases(http, &bases, "reviews.json").await {
            Ok(raw) => {
                if require_signed {
                    verified_reviews(http, &bases, &raw, &source.name, &public_key).await?;
                }
                Some(
                    serde_json::from_str::<serde_json::Value>(&raw)
                        .ok()
                        .and_then(|v| v.get("reviews").cloned())
                        .and_then(|r| serde_json::from_value::<Vec<serde_json::Value>>(r).ok())
                        .map(|reviews| {
                            reviews
                                .iter()
                                .filter_map(|r| {
                                    r.get("sha256")
                                        .and_then(|s| s.as_str())
                                        .map(str::to_lowercase)
                                })
                                .collect()
                        })
                        .unwrap_or_default(),
                )
            }
            Err(e) => {
                tracing::debug!(
                    source = %source.name,
                    error = %e,
                    "no reviews.json for this source — keeping existing review badges"
                );
                None
            }
        };

    // Current rows for this source: the fallback for review badges when
    // reviews.json could not be fetched, the cache for compatibility
    // verdicts (re-verified only when sha / manager version changed), and
    // the "has the curator seen this plugin before" record.
    let existing_rows = existing_rows_for_source(pool, &source.id).await?;

    // Entries grouped per plugin, versions sorted newest first, so a
    // plugin's first appearance can initialize with its latest compatible
    // version enabled.
    let mut groups: Vec<(String, String, Vec<&IndexEntry>)> = Vec::new();
    for entry in &index.steps {
        // Namespace comes from the template id (`<ns>/<plugin-id>`); it is the
        // unit of uniqueness across sources.
        let (namespace, plugin_id) = entry
            .id
            .split_once('/')
            .ok_or_else(|| anyhow!("index entry id '{}' is missing a namespace", entry.id))?;
        if let Some((want_ns, want_id)) = only_plugin {
            if namespace != want_ns || plugin_id != want_id {
                continue;
            }
        }
        match groups
            .iter_mut()
            .find(|(ns, id, _)| ns == namespace && id == plugin_id)
        {
            Some((_, _, versions)) => versions.push(entry),
            None => groups.push((namespace.to_string(), plugin_id.to_string(), vec![entry])),
        }
    }
    // Totals known — the dashboard's progress bar switches from "index"
    // (indeterminate) to a true fraction from here on.
    let total_entries: usize = groups.iter().map(|(_, _, versions)| versions.len()).sum();
    super::sync_progress::set_totals(&source.id, groups.len(), total_entries);

    let mut incompatible = 0usize;
    let mut undownloadable = 0usize;
    for (namespace, plugin_id, versions) in groups {
        let mut versions = versions;
        versions.sort_by_key(|v| std::cmp::Reverse(version_sort_key(&v.version)));
        // Initialization: a plugin the pool has never seen starts with its
        // LATEST COMPATIBLE version enabled — never the whole version list,
        // and never an incompatible one. A plugin the curator has already
        // seen is user territory: its rows keep their verdicts (the conflict
        // clause below never touches `enabled`) and later versions stage for
        // review instead.
        // Only the newest version is content-verified here; the rest stage
        // with an unknown verdict and verify on enable / pipeline resolution.
        // Exception: when the index carries no `requires_buz` for an entry
        // (legacy source), there is no cheap gate to rule on it, so it is
        // verified as before — otherwise auto-enable could never see a
        // compatible-but-not-newest version.
        let newest_version = versions
            .first()
            .map(|entry| entry.version.clone())
            .unwrap_or_default();
        let group_is_new = versions.iter().all(|entry| {
            !existing_rows.contains_key(&(
                namespace.clone(),
                plugin_id.clone(),
                entry.version.clone(),
            ))
        });
        let mut init_slot_open = group_is_new && source.auto_enable;
        for entry in versions {
            let sha256 = entry.sha256.to_lowercase();
            let existing =
                existing_rows.get(&(namespace.clone(), plugin_id.clone(), entry.version.clone()));
            let reviewed = match &fetched_reviews {
                Some(hashes) => hashes.contains(&sha256),
                // No reviews.json this round: keep whatever the row had.
                None => existing.is_some_and(|state| state.reviewed),
            };
            let (requires_buz, buz_compatible, compat_error, checked_against) = resolve_compat(
                http,
                &bases,
                &entry.file,
                &sha256,
                &namespace,
                &plugin_id,
                &entry.version,
                current_buz,
                existing,
                entry.requires_buz.as_deref(),
                entry.version == newest_version || entry.requires_buz.is_none(),
            )
            .await;
            if buz_compatible == Some(false) {
                incompatible += 1;
                if compat_error
                    .as_deref()
                    .is_some_and(|e| e.contains("failed to download"))
                {
                    undownloadable += 1;
                }
            }
            let enable = init_slot_open && buz_compatible == Some(true);
            if enable {
                init_slot_open = false;
            }
            let meta_json = meta_json_string(
                &entry.category,
                &entry.name,
                &entry.description,
                &entry.description_zh,
                &entry.author,
                &entry.publisher,
                &entry.license,
                &entry.source_repo,
                &entry.engine,
                &entry.deprecated,
            );
            sqlx::query(
                "INSERT INTO plugin_pool (namespace, plugin_id, version, sha256, source_id, file_path, \
                                        enabled, reviewed, added_by, requires_buz, buz_compatible, \
                                        compat_error, compat_checked_against, meta_json)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, 'sync', ?, ?, ?, ?, ?)
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
                // NOTE: `enabled` is deliberately NOT updated on conflict — a
                // re-sync must never reset the curator's enable/disable verdict.
                // New plugins initialize via the INSERT values (latest
                // compatible version for auto-enable sources, staged
                // otherwise). Compatibility columns carry kept-or-recomputed
                // verdicts (NULL = unknown), so untouched rows are never
                // invalidated.
            )
            .bind(&namespace)
            .bind(&plugin_id)
            .bind(&entry.version)
            .bind(&sha256)
            .bind(&source.id)
            .bind(&entry.file)
            .bind(enable as i64)
            .bind(reviewed as i64)
            .bind(&requires_buz)
            .bind(buz_compatible.map(|b| b as i64))
            .bind(&compat_error)
            .bind(&checked_against)
            .bind(&meta_json)
            .execute(pool)
            .await?;
            super::sync_progress::tick_entry(&source.id);
        }
        super::sync_progress::tick_plugin(&source.id);
    }

    // Aggregate verdict for the whole source: the per-entry detail lives on the
    // pool rows (and in the dashboard), so the log carries one line per sync.
    if incompatible > 0 {
        tracing::warn!(
            source = %source.name,
            incompatible,
            failed_to_download = undownloadable,
            "source has versions that cannot be used here — see the plugin pool"
        );
    }
    Ok(index.steps.len())
}

/// Refresh one plugin's metadata from its source: re-fetch the index,
/// upsert only this (namespace, plugin_id), and return the refreshed rows.
/// The curator's enable state survives (see the ON CONFLICT note above).
pub async fn refresh_plugin(
    http: &reqwest::Client,
    pool: &SqlitePool,
    namespace: &str,
    plugin_id: &str,
    current_buz: &str,
) -> anyhow::Result<Vec<PoolRow>> {
    let (source_id, file_path): (String, String) = sqlx::query_as(
        "SELECT source_id, file_path FROM plugin_pool WHERE namespace = ? AND plugin_id = ? LIMIT 1",
    )
    .bind(namespace)
    .bind(plugin_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| anyhow!("{e}"))?
    .ok_or_else(|| anyhow!("plugin {namespace}/{plugin_id} is not in the pool"))?;
    let source = get_source(pool, &source_id)
        .await
        .map_err(|e| anyhow!("{e}"))?
        .ok_or_else(|| anyhow!("source {source_id} disappeared"))?;

    // Preferred path: the plugin's own small meta.json (a few KB, resilient
    // on intermittent networks), sitting beside the template files. Falls
    // back to the full index for sources that do not publish per-plugin
    // meta files yet.
    let bases = source_bases(&source);
    let meta_path = format!("{}/meta.json", parent_dir(&file_path));
    let meta_url = join_url(
        bases.last().expect("canonical base always present"),
        &meta_path,
    );
    // meta.json carries no signature, so it cannot be trusted when the index
    // must be signed: fall through to the signed full-index path instead.
    let (require_signed, _) = signature_policy(pool).await;
    let meta_result = if require_signed {
        Err(anyhow!(
            "index signature required — per-plugin meta.json is unsigned"
        ))
    } else {
        http_get_text_from_bases(http, &bases, &meta_path).await
    };
    match meta_result {
        Ok(raw) => {
            upsert_plugin_meta(
                http,
                pool,
                &source,
                &raw,
                &meta_url,
                namespace,
                plugin_id,
                current_buz,
            )
            .await?;
        }
        Err(meta_error) => {
            tracing::info!(
                source = %source.name,
                meta_url = %meta_url,
                error = %meta_error,
                "per-plugin meta.json unavailable — falling back to the full index"
            );
            sync_source_inner(
                http,
                pool,
                &source,
                Some((namespace, plugin_id)),
                current_buz,
            )
            .await?;
        }
    }

    let rows: Vec<PoolRow> = sqlx::query_as(&format!(
        "SELECT {POOL_ROW_COLUMNS} \
         FROM plugin_pool p LEFT JOIN plugin_sources s ON s.id = p.source_id \
         WHERE p.namespace = ? AND p.plugin_id = ? \
         ORDER BY p.version",
    ))
    .bind(namespace)
    .bind(plugin_id)
    .fetch_all(pool)
    .await
    .map_err(|e| anyhow!("{e}"))?
    .into_iter()
    .map(pool_row_from)
    .collect();
    Ok(rows)
}

/// Directory part of a repo-relative file path ("plugins/buz/archive").
fn parent_dir(file_path: &str) -> String {
    match file_path.rsplit_once('/') {
        Some((dir, _)) => dir.to_string(),
        None => String::new(),
    }
}

#[derive(Debug, serde::Deserialize)]
struct PluginMeta {
    id: String,
    versions: Vec<PluginMetaEntry>,
}

#[derive(Debug, serde::Deserialize)]
struct PluginMetaEntry {
    version: String,
    file: String,
    sha256: String,
    #[serde(default)]
    requires_buz: Option<String>,
    #[serde(default)]
    deprecated: Option<String>,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    description_zh: Option<String>,
    #[serde(default)]
    author: Option<String>,
    #[serde(default)]
    publisher: Option<String>,
    #[serde(default)]
    license: Option<String>,
    #[serde(default)]
    source_repo: Option<String>,
    #[serde(default)]
    engine: Option<String>,
}

/// Upsert one plugin's rows from its meta.json. Same conflict semantics as
/// the full sync (enable state preserved); review badges are preserved too —
/// meta.json carries no review data, the full sync refreshes those.
#[allow(clippy::too_many_arguments)] // one row of pool settings; a struct here would only add noise
async fn upsert_plugin_meta(
    http: &reqwest::Client,
    pool: &SqlitePool,
    source: &SourceRow,
    raw: &str,
    meta_url: &str,
    namespace: &str,
    plugin_id: &str,
    current_buz: &str,
) -> anyhow::Result<()> {
    let meta: PluginMeta =
        serde_json::from_str(raw).map_err(|e| anyhow!("invalid meta.json at {meta_url}: {e}"))?;
    let expected_id = format!("{namespace}/{plugin_id}");
    if meta.id != expected_id {
        return Err(anyhow!(
            "meta.json id mismatch: expected {expected_id}, got {}",
            meta.id
        ));
    }

    let existing_rows = existing_rows_for_source(pool, &source.id).await?;
    let newest_version = meta
        .versions
        .iter()
        .map(|entry| entry.version.as_str())
        .max_by(|a, b| version_sort_key(a).cmp(&version_sort_key(b)))
        .unwrap_or_default()
        .to_string();

    for entry in &meta.versions {
        let sha256 = entry.sha256.to_lowercase();
        if entry.sha256.len() != 64 {
            return Err(anyhow!(
                "meta.json entry {}@{} has a malformed sha256",
                expected_id,
                entry.version
            ));
        }
        let existing = existing_rows.get(&(
            namespace.to_string(),
            plugin_id.to_string(),
            entry.version.clone(),
        ));
        let reviewed = existing.is_some_and(|state| state.reviewed);
        let bases = source_bases(source);
        let (requires_buz, buz_compatible, compat_error, checked_against) = resolve_compat(
            http,
            &bases,
            &entry.file,
            &sha256,
            namespace,
            plugin_id,
            &entry.version,
            current_buz,
            existing,
            entry.requires_buz.as_deref(),
            entry.version == newest_version || entry.requires_buz.is_none(),
        )
        .await;
        let meta_json = meta_json_string(
            &entry.category,
            &entry.name,
            &entry.description,
            &entry.description_zh,
            &entry.author,
            &entry.publisher,
            &entry.license,
            &entry.source_repo,
            &entry.engine,
            &entry.deprecated,
        );
        sqlx::query(
            "INSERT INTO plugin_pool (namespace, plugin_id, version, sha256, source_id, file_path, \
                                    enabled, reviewed, added_by, requires_buz, buz_compatible, \
                                    compat_error, compat_checked_against, meta_json)
             VALUES (?, ?, ?, ?, ?, ?, 0, ?, 'refresh', ?, ?, ?, ?, ?)
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
        )
        .bind(namespace)
        .bind(plugin_id)
        .bind(&entry.version)
        .bind(&sha256)
        .bind(&source.id)
        .bind(&entry.file)
        .bind(reviewed as i64)
        .bind(&requires_buz)
        .bind(buz_compatible.map(|b| b as i64))
        .bind(&compat_error)
        .bind(&checked_against)
        .bind(&meta_json)
        .execute(pool)
        .await
        .map_err(|e| anyhow!("{e}"))?;
    }
    Ok(())
}

// Enable-time verification（收录即校验）---------------------------------------

/// Fetch (cache → HTTP), verify the sha256, parse, and run the save-time spec
/// validation. An entry only enters the enabled pool once this passes —
/// curation is a checked act, not a flag flip.
pub async fn verify_entry(
    http: &reqwest::Client,
    pool: &SqlitePool,
    namespace: &str,
    plugin_id: &str,
    version: &str,
    current_buz: &str,
) -> Result<(), String> {
    let (sha256, source_id, file_path): (String, String, String) = sqlx::query_as(
        "SELECT sha256, source_id, file_path FROM plugin_pool \
         WHERE namespace = ? AND plugin_id = ? AND version = ?",
    )
    .bind(namespace)
    .bind(plugin_id)
    .bind(version)
    .fetch_optional(pool)
    .await
    .map_err(|e| e.to_string())?
    .ok_or_else(|| format!("pool entry {namespace}/{plugin_id}@{version} not found"))?;

    let source = get_source(pool, &source_id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("source {source_id} disappeared"))?;

    let bases = source_bases(&source);
    let content = fetch_content(http, &bases, &file_path, &sha256).await?;
    evaluate_entry_content(&content, namespace, plugin_id, version, current_buz).map(|_| ())
}

/// Validate fetched template content for one pool entry: parse, identity
/// check, full save-time spec validation, and the buz requirement.
/// `Ok` carries the spec's `requires_buz` (if declared); `Err` is the exact
/// message the enable gate reports — sync-time compatibility hints reuse it
/// so the list and the gate can never disagree.
pub fn evaluate_entry_content(
    content: &str,
    namespace: &str,
    plugin_id: &str,
    version: &str,
    current_buz: &str,
) -> Result<Option<String>, String> {
    let spec: shared::plugin_spec::PluginSpec = serde_yaml::from_str(content)
        .map_err(|e| format!("template {namespace}/{plugin_id}@{version} is invalid YAML: {e}"))?;
    if spec.id != format!("{namespace}/{plugin_id}") {
        return Err(format!(
            "template id mismatch: pool says {namespace}/{plugin_id}, YAML says {}",
            spec.id
        ));
    }
    if spec.version != version {
        return Err(format!(
            "template version mismatch: pool says {version}, YAML says {}",
            spec.version
        ));
    }
    if let Err(errors) = shared::plugin_spec::validate_plugin_spec(&spec, current_buz) {
        return Err(format!(
            "plugin spec '{namespace}/{plugin_id}' is invalid: {}",
            errors.join("; ")
        ));
    }
    if let Some(requires) = spec.requires_buz.as_deref() {
        shared::plugin_spec::buz_requirement_satisfied(requires, current_buz).map_err(|e| {
            format!("plugin spec '{namespace}/{plugin_id}' requires buz {requires}: {e}")
        })?;
    }
    Ok(spec.requires_buz)
}

/// Startup sync: refresh every enabled source (design §6.8 point 3 —
/// startup + manual refresh; no background timer by default). Failures are
/// logged per source and never block startup.
pub async fn sync_all_enabled(
    pool: &SqlitePool,
    http: &reqwest::Client,
    current_buz: &str,
) -> usize {
    let sources = match list_sources(pool).await {
        Ok(sources) => sources,
        Err(e) => {
            tracing::warn!(error = %e, "plugin source listing failed at startup sync");
            return 0;
        }
    };
    let mut synced = 0usize;
    for source in sources.iter().filter(|s| s.enabled) {
        match sync_source(http, pool, source, current_buz).await {
            Ok(count) => {
                synced += 1;
                tracing::info!(source = %source.name, entries = count, "plugin source synced");
            }
            Err(e) => {
                tracing::warn!(source = %source.name, error = %e, "plugin source sync failed");
            }
        }
    }
    synced
}

// Resolution ------------------------------------------------------------------

/// Fetch the pinned row's spec content and assemble the resolved plugin.
/// Shared tail of every pin-kind branch in [`resolve_remote`].
async fn remote_resolved_for_row(
    http: &reqwest::Client,
    pool: &SqlitePool,
    row: &PoolRow,
) -> Result<RemoteResolved, String> {
    let source = get_source(pool, &row.source_id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("source {} disappeared", row.source_id))?;
    let bases = source_bases(&source);
    let content = fetch_content(http, &bases, &row.file_path, &row.sha256).await?;
    Ok(RemoteResolved {
        namespace: row.namespace.clone(),
        plugin_id: row.plugin_id.clone(),
        version: row.version.clone(),
        sha256: row.sha256.clone(),
        source_url: source.url.clone(),
        file_path: row.file_path.clone(),
        reviewed: row.reviewed,
        content,
        deprecated: row.meta.as_ref().and_then(|meta| meta.deprecated.clone()),
    })
}

/// Resolve a `uses:` reference from the enabled pool. `None` = the namespace
/// has no pool entries (caller falls back to the local index). Branch pins
/// resolve through the branch's own index.json (mutable — frozen at save).
pub async fn resolve_remote(
    http: &reqwest::Client,
    pool: &SqlitePool,
    uses_ref: &str,
    policy: &ResolutionPolicy,
    current_buz: &str,
) -> Result<Option<(ResolvedPlugin, bool)>, String> {
    let (reference, pin) = shared::pipeline_yaml::parse_uses_reference(uses_ref)?;
    let (namespace, plugin_id) = match reference.split_once('/') {
        Some((ns, id)) => (ns.to_string(), id.to_string()),
        None => {
            return Err(format!(
                "uses reference '{reference}' is missing a namespace"
            ))
        }
    };

    let rows = enabled_rows_for_namespace(pool, &namespace, &plugin_id)
        .await
        .map_err(|e| format!("pool lookup failed: {e}"))?;
    if rows.is_empty() {
        return Ok(None);
    }

    let plugin_id = reference
        .split_once('/')
        .map(|(_, id)| id.to_string())
        .unwrap_or_default();
    let candidates: Vec<PoolRow> = rows
        .into_iter()
        .filter(|row| row.plugin_id == plugin_id)
        .collect();
    if candidates.is_empty() {
        return Err(format!(
            "plugin '{reference}' is not in the template pool — ask an administrator to add it from its source"
        ));
    }

    let http_client = http;
    let picked: RemoteResolved = match &pin {
        shared::pipeline_yaml::UsesPin::Sha256(hex) => {
            let row = candidates
                .iter()
                .find(|row| row.sha256 == hex.to_lowercase())
                .ok_or_else(|| {
                    format!(
                        "no plugin '{reference}' with content sha256:{hex} in the template pool"
                    )
                })?;
            remote_resolved_for_row(http_client, pool, row).await?
        }
        shared::pipeline_yaml::UsesPin::Version(v) => {
            let parts: Vec<&str> = v.split('.').collect();
            let matched: Vec<&PoolRow> = candidates
                .iter()
                .filter(|row| {
                    row.version == *v
                        || (parts.len() < 3 && row.version.starts_with(&format!("{v}.")))
                })
                .collect();
            if matched.is_empty() {
                return Err(format!(
                    "plugin '{reference}' has no version matching @{v} in the template pool"
                ));
            }
            let best = if parts.len() < 3 {
                matched
                    .iter()
                    .map(|row| plugin_resolver::version_triple(&row.version).unwrap_or((0, 0, 0)))
                    .max()
                    .expect("matched is non-empty")
            } else {
                plugin_resolver::version_triple(v).expect("validated X.Y.Z above")
            };
            let row = matched
                .iter()
                .find(|row| plugin_resolver::version_triple(&row.version) == Some(best))
                .expect("non-empty");
            remote_resolved_for_row(http_client, pool, row).await?
        }
        shared::pipeline_yaml::UsesPin::Latest => {
            // Highest available version across the enabled pool rows — frozen
            // at save like any other pin.
            let best = candidates
                .iter()
                .map(|row| plugin_resolver::version_triple(&row.version).unwrap_or((0, 0, 0)))
                .max()
                .expect("candidates is non-empty");
            let row = candidates
                .iter()
                .find(|row| plugin_resolver::version_triple(&row.version) == Some(best))
                .expect("non-empty");
            remote_resolved_for_row(http_client, pool, row).await?
        }
        shared::pipeline_yaml::UsesPin::Branch(branch) => {
            // Resolve through the branch's own index.json: mutable by
            // definition, so the fetched content hash is frozen at save.
            let any = candidates.first().expect("candidates is non-empty");
            let source = get_source(pool, &any.source_id)
                .await
                .map_err(|e| e.to_string())?
                .ok_or_else(|| format!("source {} disappeared", any.source_id))?;
            let mut branch_base: Option<String> = None;
            let mut branch_raw: Option<String> = None;
            let mut last_branch_error: Option<anyhow::Error> = None;
            for base in source_bases(&source) {
                let candidate = join_url(&base, branch);
                match http_get_text(http, &join_url(&candidate, "index.json")).await {
                    Ok(raw) => {
                        branch_base = Some(candidate);
                        branch_raw = Some(raw);
                        break;
                    }
                    Err(e) => {
                        tracing::warn!(
                            url = %join_url(&candidate, "index.json"),
                            error = %e,
                            "branch index endpoint failed — falling back to the next configured endpoint"
                        );
                        last_branch_error = Some(e);
                    }
                }
            }
            let branch_raw = branch_raw.ok_or_else(|| {
                format!(
                    "branch '{branch}' index fetch failed: {}",
                    last_branch_error
                        .map(|e| e.to_string())
                        .unwrap_or_else(|| "no endpoints configured".into())
                )
            })?;
            let branch_base = branch_base.expect("set whenever the branch index answered");
            let branch_index = parse_index(&branch_raw)?;
            let entry = branch_index
                .steps
                .iter()
                .find(|entry| entry.id == reference)
                .ok_or_else(|| {
                    format!("branch '{branch}' has no plugin '{reference}' in its index")
                })?;
            let content = http_get_text(
                http,
                &join_url(&join_url(&branch_base, branch), &entry.file),
            )
            .await
            .map_err(|e| format!("branch '{branch}' template fetch failed: {e}"))?;
            let content_sha256 = sha256_hex(&content);
            RemoteResolved {
                namespace: namespace.clone(),
                plugin_id,
                version: format!(
                    "{branch} ({})",
                    &content_sha256[..12.min(content_sha256.len())]
                ),
                sha256: content_sha256.clone(),
                source_url: source.url.clone(),
                file_path: entry.file.clone(),
                reviewed: entry.sha256.to_lowercase() == content_sha256,
                content,
                deprecated: entry.deprecated.clone(),
            }
        }
    };

    if policy.require_reviewed && !picked.reviewed {
        return Err(format!(
            "plugin '{uses_ref}' (sha256 {}) is not officially reviewed and require_reviewed is enabled — pin an approved hash or disable the flag",
            picked.sha256
        ));
    }

    let spec: shared::plugin_spec::PluginSpec =
        serde_yaml::from_str(&picked.content).map_err(|e| {
            format!(
                "template {}/{}/{} is invalid YAML: {e}",
                picked.namespace, picked.plugin_id, picked.version
            )
        })?;
    if spec.id != format!("{}/{}", picked.namespace, picked.plugin_id) {
        return Err(format!(
            "template id mismatch: index says {}/{}, YAML says {}",
            picked.namespace, picked.plugin_id, spec.id
        ));
    }
    // Full save-time validation applies to remote templates identically —
    // the pool changes WHERE a template comes from, never WHETHER it is checked.
    if let Err(errors) = shared::plugin_spec::validate_plugin_spec(&spec, current_buz) {
        return Err(format!(
            "plugin spec '{}/{}' is invalid: {}",
            picked.namespace,
            picked.plugin_id,
            errors.join("; ")
        ));
    }
    if let Some(requires) = spec.requires_buz.as_deref() {
        shared::plugin_spec::buz_requirement_satisfied(requires, current_buz).map_err(|e| {
            format!(
                "plugin spec '{}/{}' requires buz {requires}: {e}",
                picked.namespace, picked.plugin_id
            )
        })?;
    }

    let reviewed = picked.reviewed;
    let deprecated = spec
        .deprecated
        .clone()
        .or_else(|| picked.deprecated.clone());
    Ok(Some((
        ResolvedPlugin {
            spec,
            source: format!("{} ({})", picked.source_url, picked.file_path),
            content_sha256: picked.sha256,
            deprecated,
        },
        reviewed,
    )))
}

/// Fetch template content with sha256 verification and a content-addressed
/// cache under `<data_dir>/plugin-index-cache/templates/`.
async fn fetch_content(
    http: &reqwest::Client,
    bases: &[String],
    file_path: &str,
    sha256: &str,
) -> Result<String, String> {
    let cache = PathBuf::from("data/plugin-index-cache/templates").join(format!("{sha256}.yaml"));
    if let Ok(raw) = tokio::fs::read_to_string(&cache).await {
        if sha256_hex(&raw) == sha256 {
            return Ok(raw);
        }
        // A cache entry that fails verification is treated as absent.
    }
    let raw = http_get_text_from_bases(http, bases, file_path)
        .await
        .map_err(|e| format!("template download for {file_path} failed: {e}"))?;
    if sha256_hex(&raw) != sha256 {
        return Err(format!(
            "template content for {file_path} does not match its pinned sha256 — refusing to use it"
        ));
    }
    if let Some(parent) = cache.parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }
    let _ = tokio::fs::write(&cache, &raw).await;
    Ok(raw)
}

/// Build the source-sync HTTP client. The timeout is the single
/// `plugin_sync_timeout` setting (manager.toml default + DB override) — every
/// caller constructs through here so there is exactly one place it lives.
///
/// Clients are cached per timeout: `reqwest::Client` wraps a connection pool
/// and is meant to be built once and cloned, not rebuilt per request — the
/// first build initializes the TLS backend, which on macOS can take many
/// seconds and was stalling every pipeline save / plugin sync.
pub fn http_client(timeout_secs: u64) -> reqwest::Client {
    static CLIENTS: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<u64, reqwest::Client>>,
    > = std::sync::OnceLock::new();
    let mut cache = CLIENTS
        .get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    cache
        .entry(timeout_secs)
        .or_insert_with(|| {
            crate::http_util::client_builder()
                .timeout(std::time::Duration::from_secs(timeout_secs))
                .build()
                .expect("static client config")
        })
        .clone()
}

async fn http_get_text(http: &reqwest::Client, url: &str) -> Result<String, anyhow::Error> {
    let resp = http.get(url).send().await.map_err(|e| {
        // Actionable messages for the curator: the raw reqwest error
        // ("error sending request for url …") is neither readable nor
        // fixable in the UI. The original error stays in the log.
        if e.is_timeout() {
            anyhow!("source timed out — try again or raise the sync timeout in Settings (raw: {e})")
        } else if e.is_connect() {
            anyhow!(
                "source unreachable (connection failed) — check the URL, network, or proxy; \
                 original error: {e}"
            )
        } else {
            anyhow!("source request failed: {e}")
        }
    })?;
    if !resp.status().is_success() {
        // This helper serves index.json, reviews.json, meta.json AND template
        // files, so the hint stays generic — callers add their own context.
        return Err(anyhow!(
            "source returned HTTP {} for {url} — the file does not exist or the path is wrong",
            resp.status()
        ));
    }
    resp.text()
        .await
        .map_err(|e| anyhow!("source response could not be read: {e}"))
}

// Mirror drift check ------------------------------------------------------------------

#[derive(Debug, serde::Serialize)]
pub struct MirrorCheck {
    /// `in-sync` | `out-of-sync` | `canonical-unreachable` | `mirror-unreachable`
    pub status: String,
    pub canonical_revision: Option<String>,
    pub mirror_revision: Option<String>,
    pub detail: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
struct RevisionSentinel {
    revision: String,
    #[serde(default)]
    updated_at: Option<String>,
}

/// Like `http_get_text`, but a 404 means "absent" (None) instead of an error —
/// sources and mirrors that predate revision sentinels are supported.
async fn http_get_text_opt(http: &reqwest::Client, url: &str) -> anyhow::Result<Option<String>> {
    let resp = http.get(url).send().await.map_err(|e| {
        if e.is_timeout() {
            anyhow!("source timed out — try again or raise the sync timeout in Settings (raw: {e})")
        } else if e.is_connect() {
            anyhow!(
                "source unreachable (connection failed) — check the URL, network, or proxy; \
                 original error: {e}"
            )
        } else {
            anyhow!("source request failed: {e}")
        }
    })?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !resp.status().is_success() {
        return Err(anyhow!(
            "source returned HTTP {} for {url} — the file does not exist or the path is wrong",
            resp.status()
        ));
    }
    resp.text()
        .await
        .map_err(|e| anyhow!("source response could not be read: {e}"))
        .map(Some)
}

async fn fetch_revision(
    http: &reqwest::Client,
    base: &str,
) -> anyhow::Result<Option<RevisionSentinel>> {
    match http_get_text_opt(http, &join_url(base, "revision.json")).await? {
        None => Ok(None),
        Some(raw) => serde_json::from_str(&raw)
            .map(Some)
            .map_err(|e| anyhow!("invalid revision.json under {base}: {e}")),
    }
}

/// Compare the namespace revision sentinel between the mirror and the
/// canonical origin: two tiny GETs in the common case. Only when a sentinel
/// is missing (legacy sources, older mirrors) does the check upgrade to a
/// full index byte-compare — the index is generated byte-deterministically,
/// so equal bytes mean equal content.
pub async fn check_mirror(
    http: &reqwest::Client,
    pool: &SqlitePool,
    source_id: &str,
) -> anyhow::Result<MirrorCheck> {
    let source = get_source(pool, source_id)
        .await?
        .ok_or_else(|| anyhow!("source {source_id} not found"))?;
    let mirror = source
        .mirror_url
        .as_deref()
        .map(str::trim)
        .filter(|mirror| !mirror.is_empty())
        .ok_or_else(|| anyhow!("source '{}' has no mirror configured", source.name))?;
    let bases = source_bases(&source);
    let canonical_base = bases.last().expect("canonical base always present");
    let mirror_base = base_url(mirror);

    let canonical = match fetch_revision(http, canonical_base).await {
        Ok(sentinel) => sentinel,
        Err(e) => {
            return Ok(MirrorCheck {
                status: "canonical-unreachable".into(),
                canonical_revision: None,
                mirror_revision: None,
                detail: Some(e.to_string()),
            })
        }
    };
    let mirror_side = match fetch_revision(http, &mirror_base).await {
        Ok(sentinel) => sentinel,
        Err(e) => {
            return Ok(MirrorCheck {
                status: "mirror-unreachable".into(),
                canonical_revision: canonical.as_ref().map(|s| s.revision.clone()),
                mirror_revision: None,
                detail: Some(e.to_string()),
            })
        }
    };

    match (canonical, mirror_side) {
        (Some(canon), Some(mirr)) => {
            let status = if canon.revision == mirr.revision {
                "in-sync"
            } else {
                "out-of-sync"
            };
            Ok(MirrorCheck {
                status: status.into(),
                canonical_revision: Some(canon.revision),
                mirror_revision: Some(mirr.revision),
                detail: canon.updated_at,
            })
        }
        (canon, mirr) => {
            let canonical_index = http_get_text(http, &join_url(canonical_base, "index.json"))
                .await
                .map_err(|e| anyhow!("{e}"))?;
            let mirror_index = http_get_text(http, &join_url(&mirror_base, "index.json"))
                .await
                .map_err(|e| anyhow!("{e}"))?;
            let status = if canonical_index == mirror_index {
                "in-sync"
            } else {
                "out-of-sync"
            };
            Ok(MirrorCheck {
                status: status.into(),
                canonical_revision: canon.map(|s| s.revision),
                mirror_revision: mirr.map(|s| s.revision),
                detail: Some(
                    "no revision.json sentinel on one side — compared the full index".into(),
                ),
            })
        }
    }
}

#[cfg(test)]
mod deprecated_meta_tests {
    use super::*;

    #[test]
    fn pool_meta_roundtrips_the_deprecation_notice() {
        let raw = meta_json_string(
            &None,
            &Some("plugin".into()),
            &None,
            &None,
            &None,
            &None,
            &None,
            &None,
            &None,
            &Some("use 1.1.1".into()),
        )
        .expect("meta json");
        let parsed: PoolEntryMeta = serde_json::from_str(&raw).expect("parse");
        assert_eq!(parsed.deprecated.as_deref(), Some("use 1.1.1"));
    }

    #[test]
    fn legacy_pool_meta_without_deprecated_deserializes() {
        let parsed: PoolEntryMeta =
            serde_json::from_str(r#"{"name":"old","engine":"shell"}"#).expect("legacy meta");
        assert_eq!(parsed.deprecated, None);
    }
}
