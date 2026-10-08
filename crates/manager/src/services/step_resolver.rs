//! Local step index resolution for `uses:` marketplace references
//! (marketplace ADR §6.2/§6.4 — Phase 1: `local` index type only).
//!
//! Semantics per the 2026-10-03 decisions: the local index directory is the
//! index (no index.json required); resolution happens at pipeline-save time
//! and locks ("解析即锁定"); provenance rows are per pipeline, never shared.
//! Remote `https` indexes and the static site are Phase 2 and deliberately
//! out of scope here.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use sha2::{Digest, Sha256};
use shared::pipeline_yaml::{parse_uses_reference, UsesPin};
use shared::protocol::{validate_input_values, StepDef};
use shared::step_spec::{buz_requirement_satisfied, validate_step_spec, StepSpec};

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
#[derive(Clone)]
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
        let files = shared::yaml_files::discover_yaml_files(&self.dir).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                format!(
                    "local step index directory {} does not exist",
                    self.dir.display()
                )
            } else {
                format!("cannot read {}: {e}", self.dir.display())
            }
        })?;

        let mut specs = Vec::new();
        for file in files {
            let Ok(raw) = std::fs::read_to_string(&file) else {
                tracing::warn!("step index: cannot read {} — skipped", file.display());
                continue;
            };
            let Ok(spec) = serde_yaml::from_str::<StepSpec>(&raw) else {
                tracing::warn!(
                    "step index: {} is not valid YAML — skipped (run `buz-manager \\
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
                // The chosen (id, version) must be declared by exactly one
                // file — two files with the same id+version but different
                // content would otherwise resolve nondeterministically.
                let best = if parts.len() < 3 {
                    // Major pin → the highest matching version wins.
                    matched
                        .iter()
                        .map(|entry| version_triple(&entry.spec.version).unwrap_or((0, 0, 0)))
                        .max()
                        .expect("matched is non-empty")
                } else {
                    version_triple(v).expect("validated X.Y.Z above")
                };
                let at_best: Vec<&IndexedSpec> = matched
                    .iter()
                    .filter(|entry| version_triple(&entry.spec.version) == Some(best))
                    .collect();
                if at_best.len() > 1 {
                    let pretty = format!("{}.{}.{}", best.0, best.1, best.2);
                    return Err(format!(
                        "step '{reference}@{pretty}' is ambiguous — {} files declare the same \
                         id and version in the local index; keep one file per id+version",
                        at_best.len()
                    ));
                }
                at_best.into_iter().next().expect("non-empty").clone()
            }
        };

        if let Err(errors) = validate_step_spec(&picked.spec, current_buz) {
            return Err(format!(
                "step spec '{}' is invalid: {}",
                picked.spec.id,
                errors.join("; ")
            ));
        }
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
    let with = step.with.clone().unwrap_or_default();
    let mut values = std::collections::HashMap::new();
    let mut deferred_defs = Vec::new();
    let mut deferred_values = std::collections::HashMap::new();
    let mut validation_defs = Vec::new();
    let mut validation_values = std::collections::HashMap::new();

    for def in &input_defs {
        let value = with.get(&def.name).cloned().or_else(|| def.default.clone());
        if def.required.unwrap_or(false)
            && value
                .as_deref()
                .map(str::trim)
                .unwrap_or_default()
                .is_empty()
        {
            errors.push(format!(
                "with: missing required input '{}' of template '{}'",
                def.name, spec.id
            ));
        }
        let Some(value) = value else { continue };
        values.insert(def.name.clone(), value.clone());
        let is_secret_ref = shared::secret_ref::parse_secrets_expr(&value).is_some();
        let is_vars_ref = shared::secret_ref::parse_vars_expr(&value).is_some();
        if value.contains("${secrets.") && !is_secret_ref {
            errors.push(format!(
                "with.{}: Secret inputs must use a complete ${{secrets.NAME}} reference",
                def.name
            ));
        }
        if value.contains("${vars.") && !is_vars_ref {
            errors.push(format!(
                "with.{}: variable inputs must use a complete ${{vars.NAME}} reference",
                def.name
            ));
        }
        if def.is_masking() && !is_secret_ref {
            errors.push(format!(
                "with.{}: masking input must be exactly a ${{secrets.NAME}} reference",
                def.name
            ));
        }
        if is_secret_ref {
            deferred_defs.push(def.clone());
            deferred_values.insert(def.name.clone(), value);
        } else if !is_vars_ref {
            let mut definition = def.clone();
            definition.required = Some(false);
            validation_defs.push(definition);
            validation_values.insert(def.name.clone(), value);
        }
    }

    for key in with.keys() {
        if !input_defs.iter().any(|def| &def.name == key) {
            errors.push(format!(
                "with.{key}: template '{}' does not declare this input (declared: {})",
                spec.id,
                input_defs
                    .iter()
                    .map(|def| def.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }
    if let Err(field_errors) =
        validate_input_values(&validation_defs, &validation_values, |_| false)
    {
        errors.extend(
            field_errors
                .into_iter()
                .map(|error| format!("with.{}: {}", error.name, error.message)),
        );
    }

    let mut env = step.env.clone().unwrap_or_default();
    for (name, value) in &values {
        env.insert(shared::protocol::input_env_key(name), value.clone());
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

#[derive(Debug, Clone)]
pub struct PendingStepResolution {
    pub step_order: i64,
    pub uses_ref: String,
    pub resolved_version: String,
    pub content_sha256: String,
    pub source: String,
    pub expanded_json: String,
    pub input_defs_json: String,
    pub reviewed: bool,
}

#[derive(Debug, Clone)]
pub struct ResolvedSteps {
    pub steps: Vec<StepDef>,
    pub resolutions: Vec<PendingStepResolution>,
}

/// Resolve all marketplace references without mutating the database. The caller
/// persists `resolutions` in the same transaction as the expanded tasks.
pub fn resolve_uses_steps(
    steps: Vec<StepDef>,
    index_dir: &Path,
    policy: &ResolutionPolicy,
) -> Result<ResolvedSteps, String> {
    let index = LocalStepIndex::new(index_dir.to_path_buf());
    let current_buz = env!("CARGO_PKG_VERSION");

    let mut out = Vec::with_capacity(steps.len());
    let mut resolutions = Vec::new();
    for (step_order, step) in steps.into_iter().enumerate() {
        if let Some(uses_ref) = step.uses.clone() {
            let resolved = index.resolve(&uses_ref, current_buz)?;
            let reviewed = policy.reviewed_hashes.contains(&resolved.content_sha256);
            if !reviewed && policy.require_reviewed {
                return Err(format!(
                    "step '{uses_ref}' (sha256 {}) is not officially reviewed and require_reviewed is enabled — pin an approved hash or disable the flag",
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
            let expanded_json = serde_json::to_string(&expanded)
                .map_err(|e| format!("cannot serialize expanded step: {e}"))?;
            resolutions.push(PendingStepResolution {
                step_order: step_order as i64,
                uses_ref,
                resolved_version: resolved.spec.version.clone(),
                content_sha256: resolved.content_sha256,
                source: resolved.source,
                expanded_json,
                input_defs_json: serde_json::to_string(&resolved.spec.input_defs())
                    .map_err(|e| format!("cannot serialize step input definitions: {e}"))?,
                reviewed,
            });
            out.push(expanded);
        } else {
            out.push(step);
        }
    }
    Ok(ResolvedSteps {
        steps: out,
        resolutions,
    })
}

/// Replace a pipeline's provenance rows inside the caller's transaction.
pub async fn replace_step_resolutions(
    conn: &mut sqlx::SqliteConnection,
    pipeline_id: &str,
    resolutions: &[PendingStepResolution],
) -> Result<(), String> {
    sqlx::query("DELETE FROM step_resolutions WHERE pipeline_id = ?")
        .bind(pipeline_id)
        .execute(&mut *conn)
        .await
        .map_err(|e| format!("cannot replace step resolutions: {e}"))?;
    for resolution in resolutions {
        sqlx::query(
            "INSERT INTO step_resolutions
             (id, pipeline_id, step_order, uses_ref, resolved_version, content_sha256, source, expanded_json, input_defs_json, reviewed)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(pipeline_id)
        .bind(resolution.step_order)
        .bind(&resolution.uses_ref)
        .bind(&resolution.resolved_version)
        .bind(&resolution.content_sha256)
        .bind(&resolution.source)
        .bind(&resolution.expanded_json)
        .bind(&resolution.input_defs_json)
        .bind(resolution.reviewed)
        .execute(&mut *conn)
        .await
        .map_err(|e| format!("cannot record step resolution: {e}"))?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "step_resolver_tests.rs"]
mod tests;
