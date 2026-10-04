//! Local step index resolution for `uses:` marketplace references
//! (marketplace ADR §6.2/§6.4 — Phase 1: `local` index type only).
//!
//! Semantics per the 2026-10-03 decisions: the local index directory is the
//! index (no index.json required); resolution happens at pipeline-save time
//! and locks ("解析即锁定"); provenance rows are per pipeline, never shared.
//! Remote `https` indexes and the static site are Phase 2 and deliberately
//! out of scope here.

use std::collections::HashSet;
use std::sync::OnceLock;

use sha2::{Digest, Sha256};
use shared::pipeline_yaml::{parse_uses_reference, UsesPin};
use shared::protocol::StepDef;
use shared::step_spec::{buz_requirement_satisfied, parse_and_validate_step_spec, StepSpec};

/// Step-index settings resolved from Manager config at startup.
#[derive(Debug, Clone, Default)]
pub struct StepIndexSettings {
    /// Path to `reviews.json` (empty → no review checking).
    pub reviewed_manifest: String,
    /// Strict mode: reject resolutions whose hash is not reviewed.
    pub require_reviewed: bool,
}

static SETTINGS: OnceLock<StepIndexSettings> = OnceLock::new();

/// Install startup settings (mirrors `services::clock::init`). No-op when
/// already initialized.
pub fn init(settings: StepIndexSettings) {
    let _ = SETTINGS.set(settings);
}

fn settings() -> StepIndexSettings {
    SETTINGS.get().cloned().unwrap_or_default()
}

/// Review gate applied to every resolution.
#[derive(Debug, Clone, Default)]
pub struct ResolutionPolicy {
    /// Content sha256 values listed in the reviewed manifest.
    pub reviewed_hashes: HashSet<String>,
    /// Reject unreviewed templates instead of only warning.
    pub require_reviewed: bool,
}

/// Load reviewed hashes from a `reviews.json` manifest
/// (`{ "reviews": [ { "sha256": ... } ] }`). Missing file → empty set
/// (with `require_reviewed` that fails closed: nothing is reviewed).
/// Malformed file → warn and continue empty.
pub fn load_reviewed_hashes(path: &str) -> HashSet<String> {
    if path.trim().is_empty() {
        return HashSet::new();
    }
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(e) => {
            tracing::warn!("reviewed manifest {path} unreadable: {e} — treating as empty");
            return HashSet::new();
        }
    };
    let parsed: Result<ReviewsManifest, _> = serde_json::from_str(&raw);
    match parsed {
        Ok(manifest) => manifest
            .reviews
            .into_iter()
            .map(|r| r.sha256.to_lowercase())
            .collect(),
        Err(e) => {
            tracing::warn!("reviewed manifest {path} malformed: {e} — treating as empty");
            HashSet::new()
        }
    }
}

/// Build the resolution policy from startup settings.
pub fn policy_from_settings() -> ResolutionPolicy {
    let settings = settings();
    ResolutionPolicy {
        reviewed_hashes: load_reviewed_hashes(&settings.reviewed_manifest),
        require_reviewed: settings.require_reviewed,
    }
}

#[derive(serde::Deserialize)]
struct ReviewsManifest {
    #[serde(default)]
    reviews: Vec<ReviewEntry>,
}

#[derive(serde::Deserialize)]
struct ReviewEntry {
    sha256: String,
    #[serde(default)]
    #[allow(dead_code)]
    id: String,
    #[serde(default)]
    #[allow(dead_code)]
    version: String,
}

/// A step template resolved from an index, with everything provenance needs.
#[derive(Debug, Clone)]
pub struct ResolvedStep {
    pub spec: StepSpec,
    /// Where the template came from, e.g. `local:/opt/buz/pipelines/steps`.
    pub source: String,
    /// SHA-256 over the raw spec file bytes.
    pub content_sha256: String,
}

/// A template row as stored in `step_templates`.
struct StoredSpec {
    spec: StepSpec,
    raw_yaml: String,
    source: String,
    content_sha256: String,
}

/// Resolve a `uses:` reference against the `step_templates` table
/// (the DB is the primary trusted source — ADR §6.2, 2026-10-04).
///
/// Pin semantics unchanged: `@X.Y.Z` exact (PK makes it unambiguous),
/// `@X` / `@X.Y` → highest matching version, `@sha256:<hex>` matches the
/// stored content hash. The raw YAML's actual hash is recomputed on read
/// and must match the stored column — a tampered row fails loudly.
async fn resolve_in_db(
    pool: &sqlx::SqlitePool,
    uses_ref: &str,
    current_buz: &str,
) -> Result<ResolvedStep, String> {
    let (reference, pin) = parse_uses_reference(uses_ref)?;

    let rows: Vec<(String, String, String, String)> = sqlx::query_as(
        "SELECT version, raw_yaml, content_sha256, source
         FROM step_templates WHERE id = ? ORDER BY version ASC",
    )
    .bind(&reference)
    .fetch_all(pool)
    .await
    .map_err(|e| format!("cannot query step templates: {e}"))?;

    if rows.is_empty() {
        return Err(format!(
            "step '{reference}' not found in the step library — import it via \
             `buz-manager step-import` or the steps API"
        ));
    }

    // Integrity first: recompute the hash of the stored truth.
    let rows: Vec<StoredSpec> = rows
        .into_iter()
        .map(|(version, raw_yaml, stored_hash, source)| {
            let actual = format!("{:x}", Sha256::digest(raw_yaml.as_bytes()));
            if actual != stored_hash {
                return Err(format!(
                    "step '{reference}@{version}' failed integrity check — stored hash does \
                     not match its content; re-import the template"
                ));
            }
            let spec: StepSpec = serde_yaml::from_str(&raw_yaml).map_err(|e| {
                format!("stored template '{reference}@{version}' no longer parses: {e}")
            })?;
            Ok(StoredSpec {
                spec,
                raw_yaml,
                source,
                content_sha256: stored_hash,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;

    let picked = match &pin {
        UsesPin::Sha256(hex) => rows
            .into_iter()
            .find(|entry| entry.content_sha256 == *hex)
            .ok_or_else(|| {
                format!("no step '{reference}' with content sha256:{hex} in the step library")
            })?,
        UsesPin::Version(v) => {
            let parts: Vec<&str> = v.split('.').collect();
            let matched: Vec<StoredSpec> = rows
                .into_iter()
                .filter(|entry| {
                    entry.spec.version == *v
                        || (parts.len() < 3 && entry.spec.version.starts_with(&format!("{v}.")))
                })
                .collect();
            if matched.is_empty() {
                return Err(format!(
                    "step '{reference}' has no version matching @{v} in the step library"
                ));
            }
            if parts.len() < 3 {
                matched
                    .into_iter()
                    .max_by_key(|entry| version_triple(&entry.spec.version).unwrap_or((0, 0, 0)))
                    .expect("matched is non-empty")
            } else {
                matched.into_iter().next().expect("matched is non-empty")
            }
        }
    };

    if let Some(requires) = picked.spec.requires_buz.as_deref() {
        buz_requirement_satisfied(requires, current_buz)?;
    }

    Ok(ResolvedStep {
        spec: picked.spec,
        source: picked.source,
        content_sha256: picked.content_sha256,
    })
}

fn version_triple(v: &str) -> Option<(u64, u64, u64)> {
    let mut parts = v.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    Some((major, minor, patch))
}

/// Expand a `uses:` step into a concrete step (ADR §4/§5.1).
///
/// The template provides `run` / `engine` / `image` (not overridable) and
/// the timeout fallback; the call site keeps its own `name`, `env`,
/// `timeout`, `retry`, `continue-on-error`, `secrets`, and deployment
/// context (`host*`, `dir`, `volumes`, `privileged`, `registry_secret`).
/// `with:` values become `INPUT_<UPPER_SNAKE>` step env entries — the same
/// channel manual-trigger inputs use — and are validated against the
/// template's declared inputs (unknown keys and missing required inputs are
/// errors; literal values are type-checked, `${secrets.X}` placeholders are
/// resolved at claim time like any step env value).
pub fn expand_uses_step(step: &StepDef, resolved: &ResolvedStep) -> Result<StepDef, Vec<String>> {
    let mut errors: Vec<String> = Vec::new();
    let spec = &resolved.spec;

    let input_defs = spec.input_defs();
    let mut env = step.env.clone().unwrap_or_default();

    let with = step.with.clone().unwrap_or_default();
    for (key, value) in &with {
        if !input_defs.iter().any(|def| &def.name == key) {
            errors.push(format!(
                "with.{key}: template '{}' does not declare this input (declared: {})",
                spec.id,
                input_defs
                    .iter()
                    .map(|d| d.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        let env_key = format!("INPUT_{}", key.to_uppercase());
        env.insert(env_key, value.clone());
    }
    for def in &input_defs {
        let provided = with.contains_key(&def.name);
        let has_default = def.default.as_deref().is_some_and(|d| !d.is_empty());
        if def.required.unwrap_or(false) && !provided && !has_default {
            errors.push(format!(
                "with: missing required input '{}' of template '{}'",
                def.name, spec.id
            ));
        }
    }

    if !errors.is_empty() {
        return Err(errors);
    }

    Ok(StepDef {
        name: if step.name.trim().is_empty() {
            format!("{}@{}", spec.step_id(), spec.version)
        } else {
            step.name.clone()
        },
        run: spec.run.clone().unwrap_or_default(),
        engine: Some(spec.effective_engine().to_string()),
        // The expanded step is concrete — `uses` is cleared and the
        // provenance row carries the audit trail instead.
        uses: None,
        with: None,
        image: spec.image.clone(),
        volumes: step.volumes.clone(),
        host: step.host.clone(),
        host_labels: step.host_labels.clone(),
        hosts_strategy: step.hosts_strategy.clone(),
        dir: step.dir.clone(),
        timeout: step
            .timeout
            .clone()
            .or_else(|| spec.default_timeout.clone()),
        retry: step.retry,
        continue_on_error: step.continue_on_error,
        secrets: step.secrets.clone(),
        env: if env.is_empty() { None } else { Some(env) },
        registry_secret: step.registry_secret.clone(),
        config: step.config.clone(),
        privileged: step.privileged,
        config_source: step.config_source.clone(),
        config_key_path: step.config_key_path.clone(),
        config_dest: step.config_dest.clone(),
        config_render: step.config_render.clone(),
    })
}

/// Resolve any `uses:` steps of a pipeline save against the local index:
/// each reference is expanded into a concrete step and locked with one
/// provenance row (ADR §6.4 — 解析即锁定, per-pipeline records). Steps
/// without `uses:` pass through untouched, so this is cheap on the common
/// path.
pub async fn resolve_uses_steps(
    pool: &sqlx::SqlitePool,
    pipeline_id: &str,
    steps: Vec<StepDef>,
    policy: &ResolutionPolicy,
) -> Result<Vec<StepDef>, String> {
    if !steps.iter().any(|s| s.uses.is_some()) {
        return Ok(steps);
    }
    let current_buz = env!("CARGO_PKG_VERSION");

    let mut out = Vec::with_capacity(steps.len());
    for step in steps {
        if let Some(uses_ref) = step.uses.clone() {
            let resolved = resolve_in_db(pool, &uses_ref, current_buz).await?;
            let reviewed = policy.reviewed_hashes.contains(&resolved.content_sha256);
            if !reviewed && policy.require_reviewed {
                return Err(format!(
                    "step '{uses_ref}' (sha256 {}) is not officially reviewed and                      require_reviewed is enabled — pin an approved hash or disable the flag",
                    resolved.content_sha256
                ));
            }
            if !reviewed {
                tracing::warn!(
                    step = %uses_ref,
                    sha256 = %resolved.content_sha256,
                    "resolving a template that is not in the reviewed manifest"
                );
            }
            let expanded = expand_uses_step(&step, &resolved).map_err(|e| e.join("; "))?;
            record_resolution(pool, pipeline_id, &uses_ref, &resolved, &expanded, reviewed).await?;
            out.push(expanded);
        } else {
            out.push(step);
        }
    }
    Ok(out)
}

/// Outcome of [`import_template`].
#[derive(Debug, PartialEq, Eq)]
pub enum ImportOutcome {
    /// Row created.
    Inserted,
    /// Identical (id, version, sha256) already present — idempotent no-op.
    AlreadyPresent,
}

/// Validate one template's YAML and store it into `step_templates`.
/// Append-only: same (id, version) with identical content is an idempotent
/// no-op; with different content it is a conflict (the author must bump the
/// version). Every ingestion path (CLI / API / sync) goes through this.
pub async fn import_template(
    pool: &sqlx::SqlitePool,
    raw_yaml: &str,
    source: &str,
    current_buz: &str,
) -> Result<(ImportOutcome, String, String), String> {
    let spec = parse_and_validate_step_spec(raw_yaml, current_buz).map_err(|e| e.join("; "))?;
    let content_sha256 = format!("{:x}", Sha256::digest(raw_yaml.as_bytes()));

    let existing: Option<String> = sqlx::query_scalar(
        "SELECT content_sha256 FROM step_templates WHERE id = ? AND version = ?",
    )
    .bind(&spec.id)
    .bind(&spec.version)
    .fetch_optional(pool)
    .await
    .map_err(|e| format!("cannot query step templates: {e}"))?;

    if let Some(existing_hash) = existing {
        if existing_hash == content_sha256 {
            return Ok((ImportOutcome::AlreadyPresent, spec.id, spec.version));
        }
        return Err(format!(
            "step '{}@{}' already exists with different content — bump the version instead of              overwriting (library rows are append-only)",
            spec.id, spec.version
        ));
    }

    sqlx::query(
        "INSERT INTO step_templates (id, version, raw_yaml, content_sha256, source)
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(&spec.id)
    .bind(&spec.version)
    .bind(raw_yaml)
    .bind(&content_sha256)
    .bind(source)
    .execute(pool)
    .await
    .map_err(|e| format!("cannot insert step template: {e}"))?;

    Ok((ImportOutcome::Inserted, spec.id, spec.version))
}

async fn record_resolution(
    pool: &sqlx::SqlitePool,
    pipeline_id: &str,
    uses_ref: &str,
    resolved: &ResolvedStep,
    expanded: &StepDef,
    reviewed: bool,
) -> Result<(), String> {
    let expanded_json = serde_json::to_string(expanded)
        .map_err(|e| format!("cannot serialize expanded step: {e}"))?;
    sqlx::query(
        "INSERT INTO step_resolutions
         (id, pipeline_id, uses_ref, resolved_version, content_sha256, source, expanded_json, \
          reviewed)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(uuid::Uuid::new_v4().to_string())
    .bind(pipeline_id)
    .bind(uses_ref)
    .bind(&resolved.spec.version)
    .bind(&resolved.content_sha256)
    .bind(&resolved.source)
    .bind(expanded_json)
    .bind(reviewed)
    .execute(pool)
    .await
    .map_err(|e| format!("cannot record step resolution: {e}"))?;
    Ok(())
}

#[cfg(test)]
#[path = "step_resolver_tests.rs"]
mod tests;
