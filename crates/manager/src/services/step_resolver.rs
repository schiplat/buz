//! Local step index resolution for `uses:` marketplace references
//! (marketplace ADR §6.2/§6.4 — Phase 1: `local` index type only).
//!
//! Semantics per the 2026-10-03 decisions: the local index directory is the
//! index (no index.json required); resolution happens at pipeline-save time
//! and locks ("解析即锁定"); provenance rows are per pipeline, never shared.
//! Remote `https` indexes and the static site are Phase 2 and deliberately
//! out of scope here.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use shared::pipeline_yaml::{parse_uses_reference, UsesPin};
use shared::protocol::StepDef;
use shared::step_spec::{buz_requirement_satisfied, StepSpec};

/// A step template resolved from an index, with everything provenance needs.
#[derive(Debug, Clone)]
pub struct ResolvedStep {
    pub spec: StepSpec,
    /// Where the template came from, e.g. `local:/opt/buz/pipelines/steps`.
    pub source: String,
    /// SHA-256 over the raw spec file bytes.
    pub content_sha256: String,
}

/// Local index dir: configured value wins, else `<yaml_scan_dir>/steps`
/// (2026-10-03 decision — no extra config by default).
pub fn local_step_index_dir(yaml_scan_dir: &str, configured: &str) -> PathBuf {
    let configured = configured.trim();
    if configured.is_empty() {
        Path::new(yaml_scan_dir).join("steps")
    } else {
        PathBuf::from(configured)
    }
}

/// One template file found in the index.
struct IndexedSpec {
    spec: StepSpec,
    source: String,
    content_sha256: String,
}

/// The `local` step index: a directory of one-file-one-plugin YAML specs.
pub struct LocalStepIndex {
    dir: PathBuf,
}

impl LocalStepIndex {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// Load every parseable spec. Files that fail to parse are skipped with
    /// a warning — one broken file must not take the whole index down
    /// (the `buz step validate` CLI is the tool that reports them loudly).
    fn load(&self) -> Result<Vec<IndexedSpec>, String> {
        let entries = match std::fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(format!(
                    "local step index directory {} does not exist",
                    self.dir.display()
                ));
            }
            Err(e) => return Err(format!("cannot read {}: {e}", self.dir.display())),
        };

        let mut files: Vec<PathBuf> = entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                matches!(
                    p.extension().and_then(|e| e.to_str()),
                    Some("yaml") | Some("yml")
                )
            })
            .collect();
        files.sort();

        let mut specs = Vec::new();
        for file in files {
            let Ok(raw) = std::fs::read_to_string(&file) else {
                tracing::warn!("step index: cannot read {} — skipped", file.display());
                continue;
            };
            let Ok(spec) = serde_yaml::from_str::<StepSpec>(&raw) else {
                tracing::warn!(
                    "step index: {} is not a valid step spec — skipped (run `buz-manager \
                     step-validate` for details)",
                    file.display()
                );
                continue;
            };
            specs.push(IndexedSpec {
                content_sha256: format!("{:x}", Sha256::digest(raw.as_bytes())),
                source: format!("local:{}", self.dir.display()),
                spec,
            });
        }
        Ok(specs)
    }

    /// Resolve a `uses:` reference against this index.
    pub fn resolve(&self, uses_ref: &str, current_buz: &str) -> Result<ResolvedStep, String> {
        let (reference, pin) = parse_uses_reference(uses_ref)?;

        let candidates: Vec<IndexedSpec> = self
            .load()?
            .into_iter()
            .filter(|entry| entry.spec.id == reference)
            .collect();

        if candidates.is_empty() {
            return Err(format!(
                "step '{reference}' not found in local step index {}",
                self.dir.display()
            ));
        }

        let picked = match &pin {
            UsesPin::Sha256(hex) => candidates
                .into_iter()
                .find(|entry| entry.content_sha256 == *hex)
                .ok_or_else(|| {
                    format!(
                        "no step '{reference}' with content sha256:{hex} in local step index {}",
                        self.dir.display()
                    )
                })?,
            UsesPin::Version(v) => {
                let parts: Vec<&str> = v.split('.').collect();
                let matched: Vec<IndexedSpec> = candidates
                    .into_iter()
                    .filter(|entry| {
                        // Exact for @X.Y.Z, prefix for the @X / @1.2 major pins.
                        entry.spec.version == *v
                            || (parts.len() < 3 && entry.spec.version.starts_with(&format!("{v}.")))
                    })
                    .collect();
                if matched.is_empty() {
                    return Err(format!(
                        "step '{reference}' has no version matching @{v} in local step index {}",
                        self.dir.display()
                    ));
                }
                if parts.len() < 3 {
                    // Major pin → the highest matching version wins.
                    matched
                        .into_iter()
                        .max_by_key(|entry| {
                            version_triple(&entry.spec.version).unwrap_or((0, 0, 0))
                        })
                        .expect("matched is non-empty")
                } else {
                    // Exact pin must be unambiguous.
                    if matched.len() > 1 {
                        return Err(format!(
                            "step '{reference}@{v}' is ambiguous — {} files declare the same \
                             id and version in the local index",
                            matched.len()
                        ));
                    }
                    matched.into_iter().next().expect("non-empty")
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
    index_dir: &Path,
) -> Result<Vec<StepDef>, String> {
    if !steps.iter().any(|s| s.uses.is_some()) {
        return Ok(steps);
    }
    let index = LocalStepIndex::new(index_dir.to_path_buf());
    let current_buz = env!("CARGO_PKG_VERSION");

    let mut out = Vec::with_capacity(steps.len());
    for step in steps {
        if let Some(uses_ref) = step.uses.clone() {
            let resolved = index.resolve(&uses_ref, current_buz)?;
            let expanded = expand_uses_step(&step, &resolved).map_err(|e| e.join("; "))?;
            record_resolution(pool, pipeline_id, &uses_ref, &resolved, &expanded).await?;
            out.push(expanded);
        } else {
            out.push(step);
        }
    }
    Ok(out)
}

async fn record_resolution(
    pool: &sqlx::SqlitePool,
    pipeline_id: &str,
    uses_ref: &str,
    resolved: &ResolvedStep,
    expanded: &StepDef,
) -> Result<(), String> {
    let expanded_json = serde_json::to_string(expanded)
        .map_err(|e| format!("cannot serialize expanded step: {e}"))?;
    sqlx::query(
        "INSERT INTO step_resolutions
         (id, pipeline_id, uses_ref, resolved_version, content_sha256, source, expanded_json)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(uuid::Uuid::new_v4().to_string())
    .bind(pipeline_id)
    .bind(uses_ref)
    .bind(&resolved.spec.version)
    .bind(&resolved.content_sha256)
    .bind(&resolved.source)
    .bind(expanded_json)
    .execute(pool)
    .await
    .map_err(|e| format!("cannot record step resolution: {e}"))?;
    Ok(())
}

#[cfg(test)]
#[path = "step_resolver_tests.rs"]
mod tests;
