//! Local step index resolution for `uses:` marketplace references
//! (marketplace ADR §6.2/§6.4 — Phase 1: `local` index type only).
//!
//! Semantics per the 2026-10-03 decisions: the local index directory is the
//! index (no index.json required); resolution happens at pipeline-save time
//! and locks ("解析即锁定"); provenance rows are per pipeline, never shared.
//! Remote `https` indexes and the static site are Phase 2 and deliberately
//! out of scope here.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex, OnceLock};

use sqlx::SqlitePool;
use std::time::SystemTime;

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

/// Admin-edited runtime overrides (web settings). `None` field = use the
/// startup value. Applied without a restart by the settings API.
static RUNTIME_OVERRIDES: LazyLock<Mutex<Option<RuntimeOverrides>>> =
    LazyLock::new(|| Mutex::new(None));

#[derive(Clone, Default)]
pub struct RuntimeOverrides {
    pub require_reviewed: Option<bool>,
    pub local_dir: Option<String>,
    pub reviewed_manifest: Option<String>,
}

/// Replace admin-edited overrides; takes effect on the next resolution.
/// `Some(v)` overrides the startup value, `None` falls back to it — callers
/// pass the FULL desired override set (read from `platform_settings`).
pub fn apply_runtime_overrides(
    require_reviewed: Option<bool>,
    local_dir: Option<String>,
    reviewed_manifest: Option<String>,
) {
    *RUNTIME_OVERRIDES.lock().expect("override slot poisoned") = Some(RuntimeOverrides {
        require_reviewed,
        local_dir,
        reviewed_manifest,
    });
}

fn runtime_overrides() -> RuntimeOverrides {
    RUNTIME_OVERRIDES
        .lock()
        .expect("override slot poisoned")
        .clone()
        .unwrap_or_default()
}

/// Install startup settings (mirrors `services::clock::init`). No-op when
/// already initialized.
pub fn init(settings: StepIndexSettings) {
    let _ = SETTINGS.set(settings);
}

fn settings() -> StepIndexSettings {
    let base = SETTINGS.get().cloned().unwrap_or_default();
    let overrides = runtime_overrides();
    StepIndexSettings {
        require_reviewed: overrides.require_reviewed.unwrap_or(base.require_reviewed),
        reviewed_manifest: overrides
            .reviewed_manifest
            .unwrap_or(base.reviewed_manifest),
        ..base
    }
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
///
/// Successful parses are cached keyed on the file's `(mtime, len)`: the
/// manifest is read on every resolution and every `/steps` listing, and a
/// stat is all an unchanged file needs. Failures are not cached — a file
/// that appears later must be picked up.
pub fn load_reviewed_hashes(path: &str) -> HashSet<String> {
    if path.trim().is_empty() {
        return HashSet::new();
    }
    let Some(stamp) = file_stamp(Path::new(path)) else {
        tracing::warn!("reviewed manifest {path} unreadable — treating as empty");
        return HashSet::new();
    };
    {
        let cache = MANIFEST_CACHE.lock().expect("manifest cache poisoned");
        if let Some(entry) = cache.get(path) {
            if entry.stamp == stamp {
                return entry.hashes.clone();
            }
        }
    }

    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(e) => {
            tracing::warn!("reviewed manifest {path} unreadable: {e} — treating as empty");
            return HashSet::new();
        }
    };
    let parsed: Result<ReviewsManifest, _> = serde_json::from_str(&raw);
    let hashes = match parsed {
        Ok(manifest) => manifest
            .reviews
            .into_iter()
            .map(|r| r.sha256.to_lowercase())
            .collect(),
        Err(e) => {
            tracing::warn!("reviewed manifest {path} malformed: {e} — treating as empty");
            HashSet::new()
        }
    };

    let mut cache = MANIFEST_CACHE.lock().expect("manifest cache poisoned");
    cache.insert(
        path.to_string(),
        ManifestCacheEntry {
            stamp,
            hashes: hashes.clone(),
        },
    );
    hashes
}

/// `(mtime, len)` identity of a file, or `None` when it cannot be stat'ed.
fn file_stamp(path: &Path) -> Option<(SystemTime, u64)> {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.modified().ok()?, meta.len()))
}

struct ManifestCacheEntry {
    stamp: (SystemTime, u64),
    hashes: HashSet<String>,
}

static MANIFEST_CACHE: LazyLock<Mutex<HashMap<String, ManifestCacheEntry>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

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

/// Effective step directory: the admin-set override when present, else the
/// config-resolved default. All consumers must use this so a settings change
/// takes effect everywhere at once.
pub fn effective_local_dir(yaml_scan_dir: &str, config_local_dir: &str) -> PathBuf {
    if let Some(dir) = runtime_overrides().local_dir {
        if !dir.trim().is_empty() {
            return PathBuf::from(dir);
        }
    }
    local_step_index_dir(yaml_scan_dir, config_local_dir)
}

/// One template file found in the index.
#[derive(Clone)]
struct IndexedSpec {
    spec: StepSpec,
    source: String,
    content_sha256: String,
}

/// Cached parse of one index file, keyed on the file's `(mtime, len)`.
struct IndexCacheEntry {
    stamp: (SystemTime, u64),
    entry: IndexedSpec,
}

/// Parsed index files per index directory. Bounded by the number of template
/// files on disk — entries are dropped when their file disappears or changes.
static INDEX_CACHE: LazyLock<Mutex<HashMap<PathBuf, HashMap<PathBuf, IndexCacheEntry>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

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
    ///
    /// Parsed specs are cached per file keyed on `(mtime, len)`: resolution
    /// runs on every save and trigger, and an unchanged directory costs one
    /// stat per file instead of a full read+parse+hash sweep. Parse failures
    /// are not cached so a fixed file is picked up immediately.
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

        // Stat first, then take the lock only to collect hits — reads and
        // parses happen without the lock so a cold cache doesn't serialize
        // concurrent resolutions.
        let mut stamps: Vec<Option<(SystemTime, u64)>> = Vec::with_capacity(files.len());
        for file in &files {
            stamps.push(file_stamp(file));
        }

        let mut fresh: Vec<IndexedSpec> = Vec::with_capacity(files.len());
        let mut missing: Vec<(PathBuf, (SystemTime, u64))> = Vec::new();
        {
            let mut cache = INDEX_CACHE.lock().expect("index cache poisoned");
            let dir_cache = cache.entry(self.dir.clone()).or_default();
            // Drop entries for files that no longer exist in the directory.
            dir_cache.retain(|p, _| files.contains(p));
            for (file, stamp) in files.iter().zip(&stamps) {
                match stamp {
                    Some(stamp) => {
                        if let Some(hit) = dir_cache.get(file) {
                            if hit.stamp == *stamp {
                                fresh.push(hit.entry.clone());
                            } else {
                                missing.push((file.clone(), *stamp));
                            }
                        } else {
                            missing.push((file.clone(), *stamp));
                        }
                    }
                    None => {
                        tracing::warn!("step index: cannot read {} — skipped", file.display());
                    }
                }
            }
        }

        let mut loaded: Vec<(PathBuf, (SystemTime, u64), IndexedSpec)> = Vec::new();
        for (file, stamp) in missing {
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
            loaded.push((
                file,
                stamp,
                IndexedSpec {
                    content_sha256: format!("{:x}", Sha256::digest(raw.as_bytes())),
                    source: format!("local:{}", self.dir.display()),
                    spec,
                },
            ));
        }

        if !loaded.is_empty() {
            let mut cache = INDEX_CACHE.lock().expect("index cache poisoned");
            let dir_cache = cache.entry(self.dir.clone()).or_default();
            for (file, stamp, entry) in loaded {
                fresh.push(entry.clone());
                dir_cache.insert(file, IndexCacheEntry { stamp, entry });
            }
        }

        Ok(fresh)
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
            UsesPin::Branch(branch) => {
                return Err(format!(
                    "branch pin '@{branch}' resolves only from remote step sources — the local index is pinned by version or sha256"
                ));
            }
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

pub fn version_triple(v: &str) -> Option<(u64, u64, u64)> {
    let mut parts = v.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    Some((major, minor, patch))
}

/// Expand a `uses:` step into a concrete step (ADR §4/§5.1).
///
/// The template provides `run` / `image` and the default `engine`; the call
/// site may override the engine within the template's declared `engines`
/// (e.g. a `[shell, ssh]` template runs on the Runner host or on the
/// pipeline's target hosts). The call site also keeps its own `name`, `env`,
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
        let is_env_ref = value.contains("${env.");
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
        if def.is_masking() && (is_env_ref || !is_secret_ref) {
            errors.push(format!(
                "with.{}: masking input must be exactly a ${{secrets.NAME}} reference",
                def.name
            ));
        }
        if is_secret_ref {
            deferred_defs.push(def.clone());
            deferred_values.insert(def.name.clone(), value);
        } else if !is_vars_ref && !is_env_ref {
            // `${env.X}` values resolve on the Runner (previous steps'
            // $BUZ_ENV) — literal type-checking here would wrongly reject them.
            let mut definition = def.clone();
            definition.required = Some(false);
            validation_defs.push(definition);
            validation_values.insert(def.name.clone(), value);
        }
    }

    if step.uses.is_some() && !step.capabilities.is_empty() {
        errors.push("capabilities: uses steps inherit capabilities from their template".into());
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

    // Engine: the template default, or a call-site override within the
    // template's declared `engines` (shell ↔ ssh reuse of one template).
    let engine = match step
        .engine
        .as_deref()
        .map(str::trim)
        .filter(|e| !e.is_empty())
    {
        Some(requested) => {
            let requested = requested.to_ascii_lowercase();
            if spec.engine_override_ok(&requested) {
                requested
            } else {
                errors.push(format!(
                    "engine: '{requested}' is not supported by {} {} — declared engines: {:?}",
                    spec.id,
                    spec.version,
                    spec.declared_engines()
                ));
                spec.effective_engine().to_string()
            }
        }
        None => spec.effective_engine().to_string(),
    };

    let capabilities = if step.uses.is_some() {
        spec.capabilities.clone()
    } else {
        step.capabilities.clone()
    };
    if let Err(error) = shared::step_spec::validate_step_capabilities(&capabilities, &engine) {
        errors.push(error);
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
        run: match (
            spec.run.as_deref().map(str::trim).filter(|r| !r.is_empty()),
            step.run.trim(),
        ) {
            (Some(template_run), "") => template_run.to_string(),
            (Some(template_run), caller_run) => format!("{template_run}\n{caller_run}"),
            (None, caller_run) => caller_run.to_string(),
        },
        engine: Some(engine),
        // The expanded step is concrete — `uses` is cleared and the
        // provenance row carries the audit trail instead.
        uses: None,
        with: None,
        image: spec.image.clone(),
        capabilities,
        volumes: step.volumes.clone(),
        host: step.host.clone(),
        host_labels: step.host_labels.clone(),
        hosts_strategy: step.hosts_strategy.clone(),
        dir: step.dir.clone().or_else(|| spec.default_dir.clone()),
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
            if !step.capabilities.is_empty() {
                return Err("capabilities: raw pipeline steps cannot request execution capabilities; declare them on a locked uses: template".into());
            }
            out.push(step);
        }
    }
    Ok(ResolvedSteps {
        steps: out,
        resolutions,
    })
}

/// Async entry point for handlers: resolution reads the step index and the
/// review manifest synchronously, so run it on the blocking pool instead of
/// stalling a tokio worker (the in-process caches make warm calls cheap, but
/// cold sweeps of a large index must not block the runtime).
pub async fn resolve_uses_steps_async(
    steps: Vec<StepDef>,
    index_dir: PathBuf,
    pool: &SqlitePool,
) -> Result<ResolvedSteps, String> {
    let policy = policy_from_settings();
    let index = LocalStepIndex::new(index_dir);
    let current_buz = env!("CARGO_PKG_VERSION");
    let http = reqwest::Client::new();

    let mut out = Vec::with_capacity(steps.len());
    let mut resolutions = Vec::new();
    for (step_order, step) in steps.into_iter().enumerate() {
        if let Some(uses_ref) = step.uses.clone() {
            // Pool first: a namespace with enabled pool entries resolves
            // remotely (community sources). Everything else — official
            // `buz/` via git clone, air-gap vendor dirs — resolves from the
            // local index. Branch pins error on the local path by design.
            let (resolved, reviewed) = match super::step_pool::resolve_remote(
                &http,
                pool,
                &uses_ref,
                &policy,
                current_buz,
            )
            .await?
            {
                Some((resolved, reviewed)) => (resolved, reviewed),
                None => {
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
                    (resolved, reviewed)
                }
            };
            if !reviewed {
                tracing::warn!(
                    step = %uses_ref,
                    sha256 = %resolved.content_sha256,
                    "resolving a template without an official review badge"
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
            if !step.capabilities.is_empty() {
                return Err("capabilities: raw pipeline steps cannot request execution capabilities; declare them on a locked uses: template".into());
            }
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
