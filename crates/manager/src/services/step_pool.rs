//! Step marketplace pool: remote sources, their index.json, and the curated
//! template pool (ADR step-marketplace §6.8).
//!
//! A **source** is an `index.json` URL (schema §6.7.3: steps[] with id
//! `<namespace>/<step-id>`, version, file, sha256). Syncing fetches the
//! index, validates it, and stages every entry as a **pool row**
//! (`enabled = auto_enable`). Pool rows — not the index — are what pipeline
//! saves resolve against; template content is fetched on demand, verified
//! against the pinned sha256, and cached content-addressed under
//! `<data_dir>/step-index-cache/templates/`.

use anyhow::anyhow;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;
use std::path::PathBuf;

use crate::services::step_resolver::{self, ResolutionPolicy, ResolvedStep};

/// URL helpers ----------------------------------------------------------------

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

pub fn sha256_hex(raw: &str) -> String {
    format!("{:x}", Sha256::digest(raw.as_bytes()))
}

/// Rows -----------------------------------------------------------------------

#[derive(Debug, Clone, serde::Serialize)]
pub struct SourceRow {
    pub id: String,
    pub name: String,
    pub url: String,
    pub auto_enable: bool,
    pub enabled: bool,
    pub last_synced_at: Option<String>,
    pub last_sync_error: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct PoolRow {
    pub namespace: String,
    pub step_id: String,
    pub version: String,
    pub sha256: String,
    pub source_id: String,
    pub source_name: Option<String>,
    pub file_path: String,
    pub enabled: bool,
    pub reviewed: bool,
    pub added_by: Option<String>,
}

/// A resolved remote reference: everything `uses:` needs to expand.
pub struct RemoteResolved {
    pub namespace: String,
    pub step_id: String,
    pub version: String,
    pub sha256: String,
    pub source_url: String,
    pub file_path: String,
    pub reviewed: bool,
    pub content: String,
}

/// index.json -----------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct StepIndex {
    #[serde(default = "Vec::new")]
    pub steps: Vec<IndexEntry>,
}

#[derive(Debug, Deserialize)]
pub struct IndexEntry {
    pub id: String,
    pub version: String,
    pub file: String,
    pub sha256: String,
}

fn parse_index(raw: &str) -> Result<StepIndex, String> {
    let index: StepIndex =
        serde_json::from_str(raw).map_err(|e| format!("invalid index.json: {e}"))?;
    let mut seen = std::collections::HashSet::new();
    for entry in &index.steps {
        if entry.id.is_empty()
            || entry.version.is_empty()
            || entry.file.is_empty()
            || entry.sha256.len() != 64
        {
            return Err(format!("invalid index entry {}", entry.id));
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

/// DB access -------------------------------------------------------------------

pub async fn list_sources(pool: &SqlitePool) -> Result<Vec<SourceRow>, anyhow::Error> {
    let rows: Vec<(String, String, String, i64, i64, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT id, name, url, auto_enable, enabled, last_synced_at, last_sync_error FROM step_sources ORDER BY created_at",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(
            |(id, name, url, auto_enable, enabled, last_synced_at, last_sync_error)| SourceRow {
                id,
                name,
                url,
                auto_enable: auto_enable != 0,
                enabled: enabled != 0,
                last_synced_at,
                last_sync_error,
            },
        )
        .collect())
}

pub async fn get_source(pool: &SqlitePool, id: &str) -> Result<Option<SourceRow>, anyhow::Error> {
    Ok(list_sources(pool).await?.into_iter().find(|s| s.id == id))
}

pub async fn list_pool(pool: &SqlitePool) -> Result<Vec<PoolRow>, anyhow::Error> {
    let rows: Vec<(
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
    )> = sqlx::query_as(
        "SELECT p.namespace, p.step_id, p.version, p.sha256, p.source_id, s.name, p.file_path, \
                p.enabled, p.reviewed, p.added_by \
         FROM step_pool p LEFT JOIN step_sources s ON s.id = p.source_id \
         ORDER BY p.namespace, p.step_id, p.version",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(
            |(
                namespace,
                step_id,
                version,
                sha256,
                source_id,
                source_name,
                file_path,
                enabled,
                reviewed,
                added_by,
            )| {
                PoolRow {
                    namespace,
                    step_id,
                    version,
                    sha256,
                    source_id,
                    source_name,
                    file_path,
                    enabled: enabled != 0,
                    reviewed: reviewed != 0,
                    added_by,
                }
            },
        )
        .collect())
}

/// Enabled pool rows for one namespace (resolution candidates).
async fn enabled_rows_for_namespace(
    pool: &SqlitePool,
    namespace: &str,
    step_id: &str,
) -> Result<Vec<PoolRow>, anyhow::Error> {
    let rows: Vec<(
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
    )> = sqlx::query_as(
        "SELECT p.namespace, p.step_id, p.version, p.sha256, p.source_id, s.name, p.file_path, \
                p.enabled, p.reviewed, p.added_by \
         FROM step_pool p LEFT JOIN step_sources s ON s.id = p.source_id \
         WHERE p.namespace = ? AND p.step_id = ? AND p.enabled = 1",
    )
    .bind(namespace)
    .bind(step_id)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(
            |(
                namespace,
                step_id,
                version,
                sha256,
                source_id,
                source_name,
                file_path,
                enabled,
                reviewed,
                added_by,
            )| {
                PoolRow {
                    namespace,
                    step_id,
                    version,
                    sha256,
                    source_id,
                    source_name,
                    file_path,
                    enabled: enabled != 0,
                    reviewed: reviewed != 0,
                    added_by,
                }
            },
        )
        .collect())
}

/// Sync ------------------------------------------------------------------------

/// Fetch `index.json`, validate it, and upsert every entry as a pool row
/// (`enabled = auto_enable`). Returns the number of entries.
pub async fn sync_source(
    http: &reqwest::Client,
    pool: &SqlitePool,
    source: &SourceRow,
) -> Result<usize, anyhow::Error> {
    let result = sync_source_inner(http, pool, source, None).await;
    let (ok, error) = match &result {
        Ok(_) => (true, None),
        Err(e) => (false, Some(e.to_string())),
    };
    let _ = sqlx::query(
        "UPDATE step_sources SET last_synced_at = datetime('now'), \
         enabled = ?, last_sync_error = ? WHERE id = ?",
    )
    .bind(ok as i64)
    .bind(error)
    .bind(&source.id)
    .execute(pool)
    .await;
    result
}

async fn sync_source_inner(
    http: &reqwest::Client,
    pool: &SqlitePool,
    source: &SourceRow,
    only_plugin: Option<(&str, &str)>,
) -> Result<usize, anyhow::Error> {
    let raw = http_get_text(http, &source.url).await?;
    let index = parse_index(&raw).map_err(|e| anyhow!("{e}"))?;
    let base = base_url(&source.url);

    // Official-style sources publish reviews.json next to the index; when it
    // is fetchable, pool rows carry the review badge by hash membership.
    // When it is NOT fetchable, `reviewed` keeps each row's CURRENT value —
    // a transient fetch failure must never wipe review badges.
    let fetched_reviews: Option<std::collections::HashSet<String>> =
        match http_get_text(http, &join_url(&base, "reviews.json")).await {
            Ok(raw) => Some(
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
            ),
            Err(e) => {
                tracing::warn!(
                    source = %source.name,
                    error = %e,
                    "reviews.json fetch failed — keeping existing review badges"
                );
                None
            }
        };

    // Current badges for this source: the fallback when reviews.json could
    // not be fetched this round.
    let existing_reviewed: std::collections::HashMap<(String, String, String), bool> =
        sqlx::query_as(
            "SELECT namespace, step_id, version, reviewed FROM step_pool WHERE source_id = ?",
        )
        .bind(&source.id)
        .fetch_all(pool)
        .await
        .map_err(|e| anyhow!("{e}"))?
        .into_iter()
        .map(|(ns, id, v, r): (String, String, String, i64)| ((ns, id, v), r != 0))
        .collect();

    for entry in &index.steps {
        // Namespace comes from the template id (`<ns>/<step-id>`); it is the
        // unit of uniqueness across sources.
        let (namespace, step_id) = entry
            .id
            .split_once('/')
            .ok_or_else(|| anyhow!("index entry id '{}' is missing a namespace", entry.id))?;
        let namespace = namespace.to_string();
        let step_id = step_id.to_string();
        if let Some((want_ns, want_id)) = only_plugin {
            if namespace != want_ns || step_id != want_id {
                continue;
            }
        }
        let reviewed = match &fetched_reviews {
            Some(hashes) => hashes.contains(&entry.sha256.to_lowercase()),
            // No reviews.json this round: keep whatever the row had.
            None => *existing_reviewed
                .get(&(namespace.clone(), step_id.clone(), entry.version.clone()))
                .unwrap_or(&false),
        };
        sqlx::query(
            "INSERT INTO step_pool (namespace, step_id, version, sha256, source_id, file_path, enabled, reviewed, added_by)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, 'sync')
             ON CONFLICT(namespace, step_id, version) DO UPDATE SET
               sha256 = excluded.sha256,
               source_id = excluded.source_id,
               file_path = excluded.file_path,
               reviewed = excluded.reviewed",
        // NOTE: `enabled` is deliberately NOT updated on conflict — a
        // re-sync must never reset the curator's enable/disable verdict.
        // New entries still start as auto_enable via the INSERT values.
        )
        .bind(&namespace)
        .bind(&step_id)
        .bind(&entry.version)
        .bind(&entry.sha256.to_lowercase())
        .bind(&source.id)
        .bind(&entry.file)
        .bind(source.auto_enable as i64)
        .bind(reviewed as i64)
        .execute(pool)
        .await?;
    }
    Ok(index.steps.len())
}

/// Refresh one plugin's metadata from its source: re-fetch the index,
/// upsert only this (namespace, step_id), and return the refreshed rows.
/// The curator's enable state survives (see the ON CONFLICT note above).
pub async fn refresh_plugin(
    http: &reqwest::Client,
    pool: &SqlitePool,
    namespace: &str,
    step_id: &str,
) -> anyhow::Result<Vec<PoolRow>> {
    let (source_id, file_path): (String, String) = sqlx::query_as(
        "SELECT source_id, file_path FROM step_pool WHERE namespace = ? AND step_id = ? LIMIT 1",
    )
    .bind(namespace)
    .bind(step_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| anyhow!("{e}"))?
    .ok_or_else(|| anyhow!("plugin {namespace}/{step_id} is not in the pool"))?;
    let source = get_source(pool, &source_id)
        .await
        .map_err(|e| anyhow!("{e}"))?
        .ok_or_else(|| anyhow!("source {source_id} disappeared"))?;

    // Preferred path: the plugin's own small meta.json (a few KB, resilient
    // on intermittent networks), sitting beside the template files. Falls
    // back to the full index for sources that do not publish per-plugin
    // meta files yet.
    let meta_url = join_url(
        &base_url(&source.url),
        &format!("{}/meta.json", parent_dir(&file_path)),
    );
    match http_get_text(http, &meta_url).await {
        Ok(raw) => {
            upsert_plugin_meta(pool, &source, &raw, &meta_url, namespace, step_id).await?;
        }
        Err(meta_error) => {
            tracing::info!(
                source = %source.name,
                meta_url = %meta_url,
                error = %meta_error,
                "per-plugin meta.json unavailable — falling back to the full index"
            );
            sync_source_inner(http, pool, &source, Some((namespace, step_id))).await?;
        }
    }

    let rows: Vec<PoolRow> = sqlx::query_as(
        "SELECT p.namespace, p.step_id, p.version, p.sha256, p.source_id, s.name, p.file_path, \
                p.enabled, p.reviewed, p.added_by \
         FROM step_pool p LEFT JOIN step_sources s ON s.id = p.source_id \
         WHERE p.namespace = ? AND p.step_id = ? \
         ORDER BY p.version",
    )
    .bind(namespace)
    .bind(step_id)
    .fetch_all(pool)
    .await
    .map_err(|e| anyhow!("{e}"))?
    .into_iter()
    .map(
        |(
            namespace,
            step_id,
            version,
            sha256,
            source_id,
            source_name,
            file_path,
            enabled,
            reviewed,
            added_by,
        ): (
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
        )| {
            PoolRow {
                namespace,
                step_id,
                version,
                sha256,
                source_id,
                source_name,
                file_path,
                enabled: enabled != 0,
                reviewed: reviewed != 0,
                added_by,
            }
        },
    )
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
}

/// Upsert one plugin's rows from its meta.json. Same conflict semantics as
/// the full sync (enable state preserved); review badges are preserved too —
/// meta.json carries no review data, the full sync refreshes those.
async fn upsert_plugin_meta(
    pool: &SqlitePool,
    source: &SourceRow,
    raw: &str,
    meta_url: &str,
    namespace: &str,
    step_id: &str,
) -> anyhow::Result<()> {
    let meta: PluginMeta =
        serde_json::from_str(raw).map_err(|e| anyhow!("invalid meta.json at {meta_url}: {e}"))?;
    let expected_id = format!("{namespace}/{step_id}");
    if meta.id != expected_id {
        return Err(anyhow!(
            "meta.json id mismatch: expected {expected_id}, got {}",
            meta.id
        ));
    }

    let existing_reviewed: std::collections::HashMap<(String, String, String), bool> =
        sqlx::query_as(
            "SELECT namespace, step_id, version, reviewed FROM step_pool WHERE source_id = ?",
        )
        .bind(&source.id)
        .fetch_all(pool)
        .await
        .map_err(|e| anyhow!("{e}"))?
        .into_iter()
        .map(|(ns, id, v, r): (String, String, String, i64)| ((ns, id, v), r != 0))
        .collect();

    for entry in &meta.versions {
        let sha256 = entry.sha256.to_lowercase();
        if entry.sha256.len() != 64 {
            return Err(anyhow!(
                "meta.json entry {}@{} has a malformed sha256",
                expected_id,
                entry.version
            ));
        }
        let reviewed = *existing_reviewed
            .get(&(
                namespace.to_string(),
                step_id.to_string(),
                entry.version.clone(),
            ))
            .unwrap_or(&false);
        sqlx::query(
            "INSERT INTO step_pool (namespace, step_id, version, sha256, source_id, file_path, enabled, reviewed, added_by)
             VALUES (?, ?, ?, ?, ?, ?, 0, ?, 'refresh')
             ON CONFLICT(namespace, step_id, version) DO UPDATE SET
               sha256 = excluded.sha256,
               source_id = excluded.source_id,
               file_path = excluded.file_path,
               reviewed = excluded.reviewed",
        )
        .bind(namespace)
        .bind(step_id)
        .bind(&entry.version)
        .bind(&sha256)
        .bind(&source.id)
        .bind(&entry.file)
        .bind(reviewed as i64)
        .execute(pool)
        .await
        .map_err(|e| anyhow!("{e}"))?;
    }
    Ok(())
}

/// Enable-time verification（收录即校验）---------------------------------------

/// Fetch (cache → HTTP), verify the sha256, parse, and run the save-time spec
/// validation. An entry only enters the enabled pool once this passes —
/// curation is a checked act, not a flag flip.
pub async fn verify_entry(
    http: &reqwest::Client,
    pool: &SqlitePool,
    namespace: &str,
    step_id: &str,
    version: &str,
    current_buz: &str,
) -> Result<(), String> {
    let (sha256, source_id, file_path): (String, String, String) = sqlx::query_as(
        "SELECT sha256, source_id, file_path FROM step_pool \
         WHERE namespace = ? AND step_id = ? AND version = ?",
    )
    .bind(namespace)
    .bind(step_id)
    .bind(version)
    .fetch_optional(pool)
    .await
    .map_err(|e| e.to_string())?
    .ok_or_else(|| format!("pool entry {namespace}/{step_id}@{version} not found"))?;

    let source = get_source(pool, &source_id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("source {source_id} disappeared"))?;

    let content = fetch_content(http, &source.url, &file_path, &sha256).await?;
    let spec: shared::step_spec::StepSpec = serde_yaml::from_str(&content)
        .map_err(|e| format!("template {namespace}/{step_id}@{version} is invalid YAML: {e}"))?;
    if spec.id != format!("{namespace}/{step_id}") {
        return Err(format!(
            "template id mismatch: pool says {namespace}/{step_id}, YAML says {}",
            spec.id
        ));
    }
    if spec.version != version {
        return Err(format!(
            "template version mismatch: pool says {version}, YAML says {}",
            spec.version
        ));
    }
    if let Err(errors) = shared::step_spec::validate_step_spec(&spec, current_buz) {
        return Err(format!(
            "step spec '{namespace}/{step_id}' is invalid: {}",
            errors.join("; ")
        ));
    }
    if let Some(requires) = spec.requires_buz.as_deref() {
        shared::step_spec::buz_requirement_satisfied(requires, current_buz).map_err(|e| {
            format!("step spec '{namespace}/{step_id}' requires buz {requires}: {e}")
        })?;
    }
    Ok(())
}

/// Startup sync: refresh every enabled source (design §6.8 point 3 —
/// startup + manual refresh; no background timer by default). Failures are
/// logged per source and never block startup.
pub async fn sync_all_enabled(pool: &SqlitePool, http: &reqwest::Client) -> usize {
    let sources = match list_sources(pool).await {
        Ok(sources) => sources,
        Err(e) => {
            tracing::warn!(error = %e, "step source listing failed at startup sync");
            return 0;
        }
    };
    let mut synced = 0usize;
    for source in sources.iter().filter(|s| s.enabled) {
        match sync_source(http, pool, source).await {
            Ok(count) => {
                synced += 1;
                tracing::info!(source = %source.name, entries = count, "step source synced");
            }
            Err(e) => {
                tracing::warn!(source = %source.name, error = %e, "step source sync failed");
            }
        }
    }
    synced
}

/// Resolution ------------------------------------------------------------------

/// Resolve a `uses:` reference from the enabled pool. `None` = the namespace
/// has no pool entries (caller falls back to the local index). Branch pins
/// resolve through the branch's own index.json (mutable — frozen at save).
pub async fn resolve_remote(
    http: &reqwest::Client,
    pool: &SqlitePool,
    uses_ref: &str,
    policy: &ResolutionPolicy,
    current_buz: &str,
) -> Result<Option<(ResolvedStep, bool)>, String> {
    let (reference, pin) = shared::pipeline_yaml::parse_uses_reference(uses_ref)?;
    let (namespace, step_id) = match reference.split_once('/') {
        Some((ns, id)) => (ns.to_string(), id.to_string()),
        None => {
            return Err(format!(
                "uses reference '{reference}' is missing a namespace"
            ))
        }
    };

    let rows = enabled_rows_for_namespace(pool, &namespace, &step_id)
        .await
        .map_err(|e| format!("pool lookup failed: {e}"))?;
    if rows.is_empty() {
        return Ok(None);
    }

    let step_id = reference
        .split_once('/')
        .map(|(_, id)| id.to_string())
        .unwrap_or_default();
    let candidates: Vec<PoolRow> = rows
        .into_iter()
        .filter(|row| row.step_id == step_id)
        .collect();
    if candidates.is_empty() {
        return Err(format!(
            "step '{reference}' is not in the template pool — ask an administrator to add it from its source"
        ));
    }

    let http_client = http;
    let picked: RemoteResolved = match &pin {
        shared::pipeline_yaml::UsesPin::Sha256(hex) => {
            let row = candidates
                .iter()
                .find(|row| row.sha256 == hex.to_lowercase())
                .ok_or_else(|| {
                    format!("no step '{reference}' with content sha256:{hex} in the template pool")
                })?;
            let source = get_source(pool, &row.source_id)
                .await
                .map_err(|e| e.to_string())?
                .ok_or_else(|| format!("source {} disappeared", row.source_id))?;
            let content =
                fetch_content(http_client, &source.url, &row.file_path, &row.sha256).await?;
            RemoteResolved {
                namespace: row.namespace.clone(),
                step_id: row.step_id.clone(),
                version: row.version.clone(),
                sha256: row.sha256.clone(),
                source_url: source.url.clone(),
                file_path: row.file_path.clone(),
                reviewed: row.reviewed,
                content,
            }
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
                    "step '{reference}' has no version matching @{v} in the template pool"
                ));
            }
            let best = if parts.len() < 3 {
                matched
                    .iter()
                    .map(|row| step_resolver::version_triple(&row.version).unwrap_or((0, 0, 0)))
                    .max()
                    .expect("matched is non-empty")
            } else {
                step_resolver::version_triple(v).expect("validated X.Y.Z above")
            };
            let row = matched
                .iter()
                .find(|row| step_resolver::version_triple(&row.version) == Some(best))
                .expect("non-empty");
            let source = get_source(pool, &row.source_id)
                .await
                .map_err(|e| e.to_string())?
                .ok_or_else(|| format!("source {} disappeared", row.source_id))?;
            let content =
                fetch_content(http_client, &source.url, &row.file_path, &row.sha256).await?;
            RemoteResolved {
                namespace: row.namespace.clone(),
                step_id: row.step_id.clone(),
                version: row.version.clone(),
                sha256: row.sha256.clone(),
                source_url: source.url.clone(),
                file_path: row.file_path.clone(),
                reviewed: row.reviewed,
                content,
            }
        }
        shared::pipeline_yaml::UsesPin::Branch(branch) => {
            // Resolve through the branch's own index.json: mutable by
            // definition, so the fetched content hash is frozen at save.
            let any = candidates.first().expect("candidates is non-empty");
            let source = get_source(pool, &any.source_id)
                .await
                .map_err(|e| e.to_string())?
                .ok_or_else(|| format!("source {} disappeared", any.source_id))?;
            let branch_base = join_url(&base_url(&source.url), branch);
            let branch_raw = http_get_text(http, &join_url(&branch_base, "index.json"))
                .await
                .map_err(|e| format!("branch '{branch}' index fetch failed: {e}"))?;
            let branch_index = parse_index(&branch_raw)?;
            let entry = branch_index
                .steps
                .iter()
                .find(|entry| entry.id == reference)
                .ok_or_else(|| {
                    format!("branch '{branch}' has no step '{reference}' in its index")
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
                step_id,
                version: format!(
                    "{branch} ({})",
                    &content_sha256[..12.min(content_sha256.len())]
                ),
                sha256: content_sha256.clone(),
                source_url: source.url.clone(),
                file_path: entry.file.clone(),
                reviewed: entry.sha256.to_lowercase() == content_sha256,
                content,
            }
        }
    };

    if policy.require_reviewed && !picked.reviewed {
        return Err(format!(
            "step '{uses_ref}' (sha256 {}) is not officially reviewed and require_reviewed is enabled — pin an approved hash or disable the flag",
            picked.sha256
        ));
    }

    let spec: shared::step_spec::StepSpec = serde_yaml::from_str(&picked.content).map_err(|e| {
        format!(
            "template {}/{}/{} is invalid YAML: {e}",
            picked.namespace, picked.step_id, picked.version
        )
    })?;
    if spec.id != format!("{}/{}", picked.namespace, picked.step_id) {
        return Err(format!(
            "template id mismatch: index says {}/{}, YAML says {}",
            picked.namespace, picked.step_id, spec.id
        ));
    }
    // Full save-time validation applies to remote templates identically —
    // the pool changes WHERE a template comes from, never WHETHER it is checked.
    if let Err(errors) = shared::step_spec::validate_step_spec(&spec, current_buz) {
        return Err(format!(
            "step spec '{}/{}' is invalid: {}",
            picked.namespace,
            picked.step_id,
            errors.join("; ")
        ));
    }
    if let Some(requires) = spec.requires_buz.as_deref() {
        shared::step_spec::buz_requirement_satisfied(requires, current_buz).map_err(|e| {
            format!(
                "step spec '{}/{}' requires buz {requires}: {e}",
                picked.namespace, picked.step_id
            )
        })?;
    }

    let reviewed = picked.reviewed;
    Ok(Some((
        ResolvedStep {
            spec,
            source: format!("{} ({})", picked.source_url, picked.file_path),
            content_sha256: picked.sha256,
        },
        reviewed,
    )))
}

/// Fetch template content with sha256 verification and a content-addressed
/// cache under `<data_dir>/step-index-cache/templates/`.
async fn fetch_content(
    http: &reqwest::Client,
    source_url: &str,
    file_path: &str,
    sha256: &str,
) -> Result<String, String> {
    let cache = PathBuf::from("data/step-index-cache/templates").join(format!("{sha256}.yaml"));
    if let Ok(raw) = tokio::fs::read_to_string(&cache).await {
        if sha256_hex(&raw) == sha256 {
            return Ok(raw);
        }
        // A cache entry that fails verification is treated as absent.
    }
    let url = join_url(&base_url(source_url), file_path);
    let raw = http_get_text(http, &url)
        .await
        .map_err(|e| format!("template download from {url} failed: {e}"))?;
    if sha256_hex(&raw) != sha256 {
        return Err(format!(
            "template content from {url} does not match its pinned sha256 — refusing to use it"
        ));
    }
    if let Some(parent) = cache.parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }
    let _ = tokio::fs::write(&cache, &raw).await;
    Ok(raw)
}

/// Build the source-sync HTTP client. The timeout is the single
/// `step_sync_timeout` setting (manager.toml default + DB override) — every
/// caller constructs through here so there is exactly one place it lives.
pub fn http_client(timeout_secs: u64) -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(timeout_secs))
        .build()
        .expect("static client config")
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
        return Err(anyhow!(
            "source returned HTTP {} — check that {url} serves a valid index.json",
            resp.status()
        ));
    }
    resp.text()
        .await
        .map_err(|e| anyhow!("source response could not be read: {e}"))
}
