use anyhow::anyhow;
use sha2::{Digest, Sha256};
use std::error::Error;
use std::fmt;
use std::path::{Path, PathBuf};
use uuid::Uuid;

use shared::error::AppError;
use shared::pipeline_yaml::{default_concurrency_mode, ConcurrencyDef, PipelineYaml, TriggerYaml};
use shared::protocol::{EnvPair, PipelineInputDef, StepDef};

use crate::api::{
    insert_env_vars, insert_pipeline_secret_bindings, insert_pipeline_var_bindings, insert_steps,
};
use crate::store::exec::DbPool;

#[derive(Debug)]
struct StepResolutionError(String);

impl fmt::Display for StepResolutionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Error for StepResolutionError {}

#[derive(Debug)]
struct YamlConflictError(String);

impl fmt::Display for YamlConflictError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Error for YamlConflictError {}

/// True when a write lost a uniqueness race (two concurrent creates for one
/// origin basename). The partial unique index on `(yaml_path)` is the backstop.
fn is_unique_violation(error: &anyhow::Error) -> bool {
    error
        .chain()
        .any(|cause| cause.to_string().contains("UNIQUE constraint failed"))
}

/// Upsert a pipeline from its parsed YAML definition.
/// Match is by `yaml_path` + `source='yaml'` — same definition name = same
/// pipeline. Returns the pipeline id.
///
/// `expected_etag` is the optimistic-concurrency token from the client: when
/// set, it is compared **inside** the write transaction, so two writers holding
/// the same token cannot both succeed. `create_only` turns a would-be update
/// into a conflict (POST semantics).
#[allow(clippy::too_many_arguments)] // definition, provenance, settings, concurrency
pub async fn upsert_from_yaml(
    pool: &DbPool,
    file_path: &str,
    parsed: &PipelineYaml,
    step_index_dir: &Path,
    yaml_content: &str,
    sync_timeout_secs: u64,
    expected_etag: Option<&str>,
    create_only: bool,
) -> anyhow::Result<String> {
    let pipeline_name = parsed.name.trim();
    let description = parsed.description.clone().unwrap_or_default();
    let environments_json =
        serde_json::to_string(&parsed.environments).unwrap_or_else(|_| "[]".into());
    // Project `notify:` into the column the trigger path reads for both
    // sources (form pipelines write it through the API instead).
    let notify_json = serde_json::to_string(&parsed.notify.clone().unwrap_or_default())
        .unwrap_or_else(|_| "[]".into());

    let (
        trigger_type,
        webhook_path,
        webhook_secret,
        webhook_environment,
        cron_expr,
        cron_environment,
        input_defs,
    ) = extract_trigger(&parsed.on);

    let runner_labels = parsed
        .runner
        .as_ref()
        .and_then(|r| r.labels.clone())
        .unwrap_or_default();

    let timeout_seconds = parsed
        .runner
        .as_ref()
        .and_then(|r| r.timeout.as_deref())
        .and_then(|s| shared::pipeline_yaml::parse_duration_seconds(s).ok())
        .unwrap_or(3600);

    // `runner.resources`: a profile name; '' means the implicit default. The
    // YAML validator already checked the syntax; authorization happens at
    // trigger (team grant) so an unauthorized pipeline can still be saved and
    // fixed, but never run.
    let resource_profile = parsed
        .runner
        .as_ref()
        .and_then(|r| r.resources.as_deref())
        .map(str::trim)
        .filter(|name| !name.is_empty() && *name != shared::sandbox_config::DEFAULT_PROFILE)
        .map(str::to_string)
        .unwrap_or_default();

    let concurrency_group: Option<String> = parsed.concurrency.as_ref().map(|c| match c {
        ConcurrencyDef::Simple(s) => s.clone(),
        ConcurrencyDef::Full(f) => f.group.clone(),
    });
    let concurrency_mode = parsed.concurrency.as_ref().and_then(|c| match c {
        ConcurrencyDef::Full(f) if f.mode != default_concurrency_mode() => Some(f.mode.clone()),
        _ => None,
    });
    let concurrency_max_queue = parsed.concurrency.as_ref().and_then(|c| match c {
        ConcurrencyDef::Full(f) if f.max_queue != 3 => Some(f.max_queue),
        _ => None,
    });
    let concurrency_queue_timeout = parsed.concurrency.as_ref().and_then(|c| match c {
        ConcurrencyDef::Full(f) => f.queue_timeout.clone(),
        _ => None,
    });

    // Resolve team: YAML `team:` / `owner_group:` slug → team_id (omit → default).
    // Unknown slug is an error (no silent fallback).
    let team_slug = parsed
        .team
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("default");
    let (team_id, resolved_slug) = resolve_team_for_yaml(pool, team_slug).await?;

    let parsed_steps: Vec<StepDef> = parsed.steps.iter().map(StepDef::from).collect();

    // Effective allowlist: optional top-level `secrets:` ∪ step lists ∪ purpose
    // slots ∪ `${secrets.X}` in env maps. Top-level list is optional (GHA-style).
    // Step `secrets:` column still stores per-step narrowing for claim-time scope.
    let pipeline_secrets: Vec<String> =
        shared::pipeline_yaml::collect_effective_secret_names(parsed);

    let yaml_sha256 = format!("{:x}", Sha256::digest(yaml_content.as_bytes()));
    let yaml_key = normalize_yaml_path_key(file_path);
    let existing = find_yaml_pipeline_row(pool, &yaml_key).await?;
    let mut team_changed = false;
    if let Some(id) = existing.as_deref() {
        // An upsert is an explicit save: it always re-projects, so a stale
        // projection is repaired even when the text is unchanged. The scanner
        // tick that made a same-hash short-circuit worthwhile is gone.
        let stored_team_id = crate::store::pipelines::team_id_of(pool, id)
            .await
            .map_err(|e| anyhow!("{e}"))?
            .ok_or_else(|| anyhow!("pipeline {id} disappeared"))?;
        team_changed = stored_team_id != team_id;
        if team_changed {
            let active_jobs = crate::store::pipelines::active_job_count(pool, id)
                .await
                .map_err(|e| anyhow!("{e}"))?;
            if active_jobs > 0 {
                anyhow::bail!(
                    "pipeline team cannot change while {active_jobs} active job(s) exist; finish or cancel them first"
                );
            }
        }
    }

    // Resolve all templates before mutating the current DB projection. This
    // preserves the previous task set when a new YAML version cannot resolve.
    let http = crate::services::plugin_pool::http_client(sync_timeout_secs);
    let resolved_steps = crate::services::plugin_resolver::resolve_uses_steps_async(
        parsed_steps,
        step_index_dir.to_path_buf(),
        pool,
        &http,
    )
    .await
    .map_err(|error| anyhow::Error::new(StepResolutionError(error)))?;
    let steps = resolved_steps.steps;
    let resolutions = resolved_steps.resolutions;

    let labels_json = serde_json::to_string(&runner_labels).unwrap_or_else(|_| "[]".into());
    let input_defs_json = serde_json::to_string(&input_defs).unwrap_or_else(|_| "[]".into());

    if let Some(id) = existing {
        if create_only {
            return Err(anyhow::Error::new(YamlConflictError(format!(
                "definition {yaml_key} already exists — use PUT to update"
            ))));
        }
        let mut tx = pool.begin_immediate().await?;
        // Optimistic concurrency. The etag comparison plus the guarded
        // UPDATE (yaml_update_tx re-verifies the observed source in its
        // WHERE) close the check-to-write window on both backends — SQLite
        // via the write lock, Postgres via the guarded WHERE.
        let mut observed_source: Option<String> = None;
        if let Some(expected) = expected_etag {
            let current = crate::store::pipelines::yaml_source_tx(&mut tx, &id)
                .await
                .map_err(|e| anyhow!("{e}"))?
                .ok_or_else(|| anyhow!("pipeline {id} disappeared"))?;
            if expected != "*" && expected != yaml_content_etag(&current) {
                tx.rollback().await?;
                return Err(anyhow::Error::new(YamlConflictError(
                    "definition was modified by another writer — reload and retry".into(),
                )));
            }
            observed_source = Some(current);
        }
        if team_changed {
            let active_jobs = crate::store::pipelines::active_job_count_tx(&mut tx, &id)
                .await
                .map_err(|e| anyhow!("{e}"))?;
            if active_jobs > 0 {
                tx.rollback().await?;
                anyhow::bail!(
                    "pipeline team cannot change while {active_jobs} active job(s) exist; finish or cancel them first"
                );
            }
        }
        let updated = crate::store::pipelines::yaml_update_tx(
            &mut tx,
            &id,
            observed_source.as_deref(),
            &crate::store::pipelines::YamlPipelineUpdate {
                name: pipeline_name,
                description: &description,
                trigger_type: &trigger_type,
                webhook_path: webhook_path.as_deref(),
                webhook_secret: webhook_secret.as_deref().unwrap_or_default(),
                webhook_environment: webhook_environment.as_deref().unwrap_or(""),
                cron_expr: cron_expr.as_deref(),
                cron_environment: cron_environment.as_deref().unwrap_or(""),
                runner_labels: &labels_json,
                timeout_seconds,
                resource_profile: &resource_profile,
                concurrency_group: concurrency_group.as_deref(),
                concurrency_mode: concurrency_mode.as_deref().unwrap_or(""),
                concurrency_max_queue: concurrency_max_queue.map(|v| v as i64).unwrap_or(3),
                concurrency_queue_timeout_seconds: parse_concurrency_timeout_seconds(
                    concurrency_queue_timeout.as_deref(),
                ),
                owner_group: &resolved_slug,
                team_id: &team_id,
                input_defs: &input_defs_json,
                environments: &environments_json,
                notify_json: &notify_json,
                yaml_path: &yaml_key,
                yaml_sha256: &yaml_sha256,
                yaml_source: yaml_content,
            },
        )
        .await
        .map_err(|e| anyhow!("{e}"))?;
        if !updated {
            // Another writer changed the definition between our etag read and
            // this write — the guarded UPDATE caught it.
            tx.rollback().await?;
            return Err(anyhow::Error::new(YamlConflictError(
                "definition was modified by another writer — reload and retry".into(),
            )));
        }

        // Replace the previously expanded step projection after resolution succeeded.
        crate::store::pipelines::clear_projection_tx(&mut tx, &id)
            .await
            .map_err(|e| anyhow!("{e}"))?;

        insert_steps(&mut tx, &id, &steps).await?;
        let resolution_writes: Vec<crate::store::pipelines::StepResolutionWrite> = resolutions
            .iter()
            .map(|r| crate::store::pipelines::StepResolutionWrite {
                step_order: r.step_order,
                uses_ref: r.uses_ref.clone(),
                resolved_version: r.resolved_version.clone(),
                content_sha256: r.content_sha256.clone(),
                source: r.source.clone(),
                expanded_json: r.expanded_json.clone(),
                input_defs_json: r.input_defs_json.clone(),
                reviewed: r.reviewed,
            })
            .collect();
        crate::store::pipelines::replace_step_resolutions_tx(&mut tx, &id, &resolution_writes)
            .await
            .map_err(|e| anyhow!("{e}"))?;

        if let Some(ref envs) = parsed.env {
            let pairs: Vec<EnvPair> = envs
                .iter()
                .map(|(k, v)| EnvPair {
                    name: k.clone(),
                    value: v.clone(),
                })
                .collect();
            insert_env_vars(&mut tx, &id, &pairs).await?;
        }
        if !pipeline_secrets.is_empty() {
            // Same-team name resolve only (never bind another team's secret)
            let secret_ids = crate::store::pipelines::secret_ids_by_names_tx(
                &mut tx,
                &team_id,
                &pipeline_secrets,
            )
            .await
            .map_err(|e| anyhow!("{e}"))?;
            if secret_ids.len() != pipeline_secrets.len() {
                tracing::warn!(
                    pipeline = %pipeline_name,
                    team = %resolved_slug,
                    requested = ?pipeline_secrets,
                    bound = secret_ids.len(),
                    "YAML secrets: some names missing or not in this pipeline's team — skipped"
                );
            }
            insert_pipeline_secret_bindings(&mut tx, &id, &secret_ids).await?;
        }
        if let Some(ref var_names) = parsed.variables {
            if !var_names.is_empty() {
                let var_ids =
                    crate::store::pipelines::platform_var_ids_by_names_tx(&mut tx, var_names)
                        .await
                        .map_err(|e| anyhow!("{e}"))?;
                insert_pipeline_var_bindings(&mut tx, &id, &var_ids).await?;
            }
        }

        tx.commit().await?;
        tracing::info!("YAML scanner: created pipeline {pipeline_name} ({id}) from {file_path}");
        Ok(id)
    } else {
        let id = Uuid::new_v4().to_string();
        let mut tx = pool.begin().await?;

        let inserted = crate::store::pipelines::yaml_insert_tx(
            &mut tx,
            &crate::store::pipelines::YamlPipelineInsert {
                id: &id,
                name: pipeline_name,
                description: &description,
                trigger_type: &trigger_type,
                webhook_path: webhook_path.as_deref(),
                webhook_secret: webhook_secret.as_deref().unwrap_or_default(),
                webhook_environment: webhook_environment.as_deref().unwrap_or(""),
                cron_expr: cron_expr.as_deref(),
                cron_environment: cron_environment.as_deref().unwrap_or(""),
                runner_labels: &labels_json,
                timeout_seconds,
                resource_profile: &resource_profile,
                concurrency_group: concurrency_group.as_deref(),
                concurrency_mode: concurrency_mode.as_deref().unwrap_or(""),
                concurrency_max_queue: concurrency_max_queue.map(|v| v as i64).unwrap_or(3),
                concurrency_queue_timeout_seconds: parse_concurrency_timeout_seconds(
                    concurrency_queue_timeout.as_deref(),
                ),
                owner_group: &resolved_slug,
                team_id: &team_id,
                yaml_path: &yaml_key,
                yaml_sha256: &yaml_sha256,
                yaml_source: yaml_content,
                input_defs: &input_defs_json,
                environments: &environments_json,
                notify_json: &notify_json,
            },
        )
        .await
        .map_err(|e| anyhow!("{e}"))?;
        if !inserted {
            // A concurrent create won the unique index — surface 409, not a
            // unique-violation 500.
            tx.rollback().await?;
            return Err(anyhow::Error::new(YamlConflictError(
                "definition was created by another writer — reload and retry".into(),
            )));
        }

        insert_steps(&mut tx, &id, &steps).await?;
        let resolution_writes: Vec<crate::store::pipelines::StepResolutionWrite> = resolutions
            .iter()
            .map(|r| crate::store::pipelines::StepResolutionWrite {
                step_order: r.step_order,
                uses_ref: r.uses_ref.clone(),
                resolved_version: r.resolved_version.clone(),
                content_sha256: r.content_sha256.clone(),
                source: r.source.clone(),
                expanded_json: r.expanded_json.clone(),
                input_defs_json: r.input_defs_json.clone(),
                reviewed: r.reviewed,
            })
            .collect();
        crate::store::pipelines::replace_step_resolutions_tx(&mut tx, &id, &resolution_writes)
            .await
            .map_err(|e| anyhow!("{e}"))?;

        if let Some(ref envs) = parsed.env {
            let pairs: Vec<EnvPair> = envs
                .iter()
                .map(|(k, v)| EnvPair {
                    name: k.clone(),
                    value: v.clone(),
                })
                .collect();
            insert_env_vars(&mut tx, &id, &pairs).await?;
        }
        if !pipeline_secrets.is_empty() {
            // Same-team name resolve only (never bind another team's secret)
            let secret_ids = crate::store::pipelines::secret_ids_by_names_tx(
                &mut tx,
                &team_id,
                &pipeline_secrets,
            )
            .await
            .map_err(|e| anyhow!("{e}"))?;
            if secret_ids.len() != pipeline_secrets.len() {
                tracing::warn!(
                    pipeline = %pipeline_name,
                    team = %resolved_slug,
                    requested = ?pipeline_secrets,
                    bound = secret_ids.len(),
                    "YAML secrets: some names missing or not in this pipeline's team — skipped"
                );
            }
            insert_pipeline_secret_bindings(&mut tx, &id, &secret_ids).await?;
        }
        if let Some(ref var_names) = parsed.variables {
            if !var_names.is_empty() {
                let var_ids =
                    crate::store::pipelines::platform_var_ids_by_names_tx(&mut tx, var_names)
                        .await
                        .map_err(|e| anyhow!("{e}"))?;
                insert_pipeline_var_bindings(&mut tx, &id, &var_ids).await?;
            }
        }

        tx.commit().await?;
        tracing::info!("YAML scanner: created pipeline {pipeline_name} ({id}) from {file_path}");
        Ok(id)
    }
}

// ── Online editor helpers (file is authority; DB is runtime projection) ──

/// Result of writing a YAML file and projecting it into the DB.
#[derive(Debug, Clone)]
pub struct YamlFileWriteResult {
    pub pipeline_id: String,
    pub filename: String,
    pub yaml_path: String,
    pub name: String,
}

/// Metadata for a YAML file under the scan directory.
#[derive(Debug, Clone, serde::Serialize)]
pub struct YamlFileInfo {
    pub filename: String,
    pub yaml_path: String,
    pub pipeline_id: Option<String>,
    pub pipeline_name: Option<String>,
    pub team_id: Option<String>,
    pub is_active: Option<bool>,
}

/// Accept only a bare basename ending in `.yml` / `.yaml` (no path separators).
pub fn validate_yaml_filename(filename: &str) -> Result<(), AppError> {
    let name = filename.trim();
    if name.is_empty() {
        return Err(AppError::ValidationFailed("filename is required".into()));
    }
    if name.contains('/') || name.contains('\\') || name.contains("..") {
        return Err(AppError::ValidationFailed(
            "filename must be a bare name without path separators".into(),
        ));
    }
    let lower = name.to_ascii_lowercase();
    if !(lower.ends_with(".yml") || lower.ends_with(".yaml")) {
        return Err(AppError::ValidationFailed(
            "filename must end with .yml or .yaml".into(),
        ));
    }
    // Extension is the only allowed '.'; stem must not contain dots.
    let stem = if lower.ends_with(".yaml") {
        &name[..name.len() - 5]
    } else {
        &name[..name.len() - 4] // .yml
    };
    if stem.is_empty()
        || !stem
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(AppError::ValidationFailed(
            "filename stem may only contain letters, digits, '-', '_' (no '.')".into(),
        ));
    }
    Ok(())
}

/// Slugify a pipeline `name:` into a safe `.yml` filename.
pub fn filename_from_pipeline_name(name: &str) -> String {
    let mut slug = String::new();
    for c in name.trim().chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
        } else if c == '_' {
            slug.push('_');
        } else if c == '-' || c.is_whitespace() || c == '.' {
            // '.' reserved for extension; collapse runs of separators to one '-'.
            if !slug.ends_with('-') {
                slug.push('-');
            }
        }
    }
    let slug = slug.trim_matches('-');
    let slug = if slug.is_empty() { "pipeline" } else { slug };
    format!("{slug}.yml")
}

/// Join `scan_dir` + basename and ensure the result stays inside the scan directory.
pub fn resolve_yaml_path(scan_dir: &str, filename: &str) -> Result<PathBuf, AppError> {
    validate_yaml_filename(filename)?;
    let base = PathBuf::from(scan_dir);
    let candidate = base.join(filename.trim());
    // Defense in depth: reject if components escape (should be impossible with basename).
    if candidate
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(AppError::ValidationFailed(
            "invalid filename: path escape".into(),
        ));
    }
    Ok(candidate)
}

/// Storage string used as `pipelines.yaml_path` — **basename only** (flat scan_dir).
///
/// Files live only directly under `yaml_scan_dir`; no private subdirectories.
/// Legacy absolute/relative paths are normalized via [`normalize_yaml_path_key`].
pub fn yaml_path_storage(_scan_dir: &str, filename: &str) -> Result<String, AppError> {
    validate_yaml_filename(filename)?;
    Ok(filename.trim().to_string())
}

/// Normalize any stored path to the basename key used in DB.
pub fn normalize_yaml_path_key(yaml_path: &str) -> String {
    filename_from_yaml_path(yaml_path).unwrap_or_else(|| yaml_path.trim().to_string())
}

/// Weak content ETag for optimistic concurrency (`If-Match` / body `etag`).
pub fn yaml_content_etag(content: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    content.hash(&mut h);
    format!("\"{:016x}-{}\"", h.finish(), content.len())
}

/// Find pipeline id by basename key; migrates legacy full-path rows to basename.
pub async fn find_yaml_pipeline_row(
    pool: &DbPool,
    filename: &str,
) -> Result<Option<String>, AppError> {
    validate_yaml_filename(filename)?;
    if let Some(id) = crate::store::pipelines::pipeline_id_by_yaml_path(pool, filename).await? {
        return Ok(Some(id));
    }
    // Legacy rows may store absolute/relative paths — match by basename and rewrite.
    for (id, path) in crate::store::pipelines::legacy_yaml_rows(pool).await? {
        let Some(path) = path else { continue };
        if normalize_yaml_path_key(&path) == filename {
            crate::store::pipelines::set_yaml_path(pool, &id, filename).await?;
            return Ok(Some(id));
        }
    }
    Ok(None)
}

/// Ensure `path` (absolute or relative) resolves under `scan_dir`.
pub fn assert_path_under_scan_dir(scan_dir: &str, path: &Path) -> Result<(), AppError> {
    let base = PathBuf::from(scan_dir);
    let base_canon = match std::fs::canonicalize(&base) {
        Ok(p) => p,
        Err(_) => base.clone(),
    };
    let path_canon = match std::fs::canonicalize(path) {
        Ok(p) => p,
        Err(_) => {
            // File may not exist yet — compare parents.
            let parent = path.parent().unwrap_or(path);
            let parent_canon =
                std::fs::canonicalize(parent).unwrap_or_else(|_| parent.to_path_buf());
            if !parent_canon.starts_with(&base_canon)
                && !parent_canon.starts_with(&base)
                && parent != base.as_path()
            {
                return Err(AppError::ValidationFailed(
                    "path escapes pipeline yaml_scan_dir".into(),
                ));
            }
            return Ok(());
        }
    };
    if !path_canon.starts_with(&base_canon) && !path_canon.starts_with(&base) {
        return Err(AppError::ValidationFailed(
            "path escapes pipeline yaml_scan_dir".into(),
        ));
    }
    Ok(())
}

/// Read YAML file content from the scan directory.
/// Read a pipeline's canonical YAML text from the database.
///
/// The database is the only authority for pipeline definitions; `yaml_path`
/// is the origin basename the definition was imported under.
pub async fn read_yaml_file(pool: &DbPool, filename: &str) -> Result<String, AppError> {
    validate_yaml_filename(filename)?;
    let id = find_yaml_pipeline_row(pool, filename)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("pipeline {filename} not found")))?;
    let source = crate::store::pipelines::yaml_source_of(pool, &id)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("pipeline {filename} not found")))?;
    if source.is_empty() {
        return Err(AppError::NotFound(format!(
            "pipeline {filename} has no stored YAML"
        )));
    }
    Ok(source)
}

/// List stored pipeline definitions as their origin filenames.
pub async fn list_yaml_files(pool: &DbPool) -> Result<Vec<YamlFileInfo>, AppError> {
    let rows = crate::store::pipelines::yaml_file_rows(pool).await?;
    let files = rows
        .into_iter()
        .filter_map(|r| {
            let filename = filename_from_yaml_path(&r.yaml_path)?;
            Some(YamlFileInfo {
                filename,
                yaml_path: r.yaml_path,
                pipeline_id: Some(r.id),
                pipeline_name: Some(r.name),
                team_id: Some(r.team_id),
                is_active: Some(r.is_active),
            })
        })
        .collect();
    Ok(files)
}

/// Validate YAML, write to scan dir, and immediately upsert the DB projection.
///
/// Validate the YAML and persist it as the pipeline's canonical definition.
///
/// The database row is the only authority: validation, the optional bind and
/// the task/step projection all go through this one write path. `yaml_path`
/// keeps the origin basename for display and export; nothing is written to
/// disk.
///
/// When `bind_pipeline_id` is set (migrating a Form/API pipeline), that row is
/// retargeted to `source=yaml` + this path before upsert so the id is preserved.
/// `is_active` is never forced on.
#[allow(clippy::too_many_arguments)] // scanner context: filename, index, settings
pub async fn write_and_upsert_yaml(
    pool: &DbPool,
    filename: &str,
    content: &str,
    step_index_dir: &Path,
    bind_pipeline_id: Option<&str>,
    sync_timeout_secs: u64,
    expected_etag: Option<&str>,
    create_only: bool,
) -> Result<YamlFileWriteResult, AppError> {
    let filename = filename.trim();
    validate_yaml_filename(filename)?;

    let validation = crate::services::yaml_validate::validate_pipeline_yaml(pool, content).await?;
    crate::services::yaml_validate::reject_if_invalid(&validation)?;
    let parsed: PipelineYaml = serde_yaml::from_str(content).map_err(|e| {
        AppError::ValidationFailed(format!("YAML syntax error after validate: {e}"))
    })?;

    // DB key = basename; a flat namespace identifies the pipeline.
    let yaml_path = filename.to_string();

    // Preflight bind conflicts before mutating the target row.
    let mut bind_prev: Option<(String, Option<String>)> = None; // (source, yaml_path)
    if let Some(pid) = bind_pipeline_id {
        let (_, prev_source, prev_path) = crate::store::pipelines::yaml_row_identity(pool, pid)
            .await?
            .ok_or_else(|| AppError::NotFound("pipeline not found".into()))?;
        if let Some(other_id) = find_yaml_pipeline_row(pool, &yaml_path).await? {
            if other_id != pid {
                return Err(AppError::Conflict(format!(
                    "file {filename} already bound to pipeline {other_id}"
                )));
            }
        }
        bind_prev = Some((prev_source, prev_path));
    }

    if let Some(pid) = bind_pipeline_id {
        // Preserve pipeline id; do not force is_active.
        if let Err(e) = crate::store::pipelines::bind_pipeline_to_yaml(pool, pid, &yaml_path).await
        {
            let error = anyhow::Error::new(e);
            if is_unique_violation(&error) {
                return Err(AppError::Conflict(format!(
                    "definition {filename} was created concurrently — reload and retry"
                )));
            }
            return Err(AppError::Internal(format!(
                "failed to bind pipeline: {error}"
            )));
        }
    }

    let pipeline_id = match upsert_from_yaml(
        pool,
        &yaml_path,
        &parsed,
        step_index_dir,
        content,
        sync_timeout_secs,
        expected_etag,
        create_only,
    )
    .await
    {
        Ok(id) => id,
        Err(e) => {
            if let (Some(pid), Some((prev_source, prev_path))) =
                (bind_pipeline_id, bind_prev.as_ref())
            {
                let _ = crate::store::pipelines::restore_pipeline_binding(
                    pool,
                    pid,
                    prev_source,
                    prev_path.as_deref(),
                )
                .await;
            }
            if let Some(error) = e.downcast_ref::<YamlConflictError>() {
                return Err(AppError::Conflict(error.to_string()));
            }
            if is_unique_violation(&e) {
                return Err(AppError::Conflict(format!(
                    "definition {filename} was created concurrently — reload and retry"
                )));
            }
            if let Some(error) = e.downcast_ref::<StepResolutionError>() {
                return Err(AppError::ValidationFailed(error.to_string()));
            }
            return Err(AppError::Internal(format!("upsert failed: {e:#}")));
        }
    };

    Ok(YamlFileWriteResult {
        pipeline_id,
        filename: filename.to_string(),
        yaml_path,
        name: parsed.name.trim().to_string(),
    })
}

/// Delete a stored pipeline definition (hard delete).
pub async fn delete_yaml_file(pool: &DbPool, filename: &str) -> Result<Option<String>, AppError> {
    let yaml_path = filename.trim().to_string();
    validate_yaml_filename(&yaml_path)?;

    let pipeline_id = find_yaml_pipeline_row(pool, &yaml_path).await?;

    if let Some(ref id) = pipeline_id {
        crate::store::pipelines::delete(pool, id).await?;
    }

    Ok(pipeline_id)
}

/// Basename of a stored `yaml_path`, if it looks like a YAML file under the scan dir.
pub fn filename_from_yaml_path(yaml_path: &str) -> Option<String> {
    Path::new(yaml_path)
        .file_name()
        .and_then(|n| n.to_str())
        .map(|s| s.to_string())
        .filter(|s| validate_yaml_filename(s).is_ok())
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct YamlSyncStatus {
    pub filename: String,
    pub status: String,
    pub last_synced_at: Option<String>,
    pub file_mtime: Option<String>,
    pub etag: Option<String>,
    pub errors: Vec<String>,
}

/// Definition status for the detail page.
///
/// The database is authoritative, so there is nothing to drift from: a stored
/// definition is `in_sync` and a legacy row without `yaml_source` reports
/// `missing_file` until the startup backfill stores one. Kept for API/UI
/// compatibility with the file-backed model.
pub async fn yaml_sync_status(
    pool: &DbPool,
    pipeline_id: &str,
) -> Result<YamlSyncStatus, AppError> {
    let (yaml_path, source, updated_at) = crate::store::pipelines::yaml_sync_row(pool, pipeline_id)
        .await?
        .ok_or_else(|| AppError::NotFound("pipeline not found".into()))?;
    let updated_at = updated_at.unwrap_or_default();
    let filename = yaml_path
        .as_deref()
        .and_then(filename_from_yaml_path)
        .unwrap_or_default();
    let (status, errors) = if source.is_empty() {
        (
            "missing_file".to_string(),
            vec!["definition is not stored — re-save the pipeline".to_string()],
        )
    } else {
        ("in_sync".to_string(), Vec::new())
    };
    Ok(YamlSyncStatus {
        filename,
        status,
        last_synced_at: Some(updated_at),
        file_mtime: None,
        etag: (!source.is_empty()).then(|| yaml_content_etag(&source)),
        errors,
    })
}

/// Resolve YAML `team` / `owner_group` slug to `(team_id, slug)`.
/// Creates no teams — unknown slug fails (Validate / create / update must match).
async fn resolve_team_for_yaml(pool: &DbPool, slug: &str) -> anyhow::Result<(String, String)> {
    if let Some(row) = crate::store::teams::id_and_slug_by_slug(pool, slug)
        .await
        .map_err(|e| anyhow!("{e}"))?
    {
        return Ok(row);
    }

    anyhow::bail!(
        "team '{slug}' does not exist — create it under Teams or fix team: / owner_group:"
    )
}

/// YAML trigger inputs → the DB projection's `input_defs`.
fn project_input_defs(
    inputs: Option<&std::collections::BTreeMap<String, shared::pipeline_yaml::InputDef>>,
) -> Vec<PipelineInputDef> {
    inputs
        .map(|map| {
            map.iter()
                .map(|(name, def)| PipelineInputDef {
                    name: name.clone(),
                    description: def.description.clone(),
                    required: def.required,
                    default: def.default.clone(),
                    masking: def.masking.filter(|mask| *mask),
                    input_type: def.r#type.clone(),
                    pattern: def.pattern.clone(),
                    options: def.options.clone(),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Extract trigger information from the YAML `on` block.
/// Tuple order: (type, webhook path/secret/environment, cron schedule/environment, inputs).
#[allow(clippy::type_complexity)] // mirrors the trigger YAML shape one field per slot
fn extract_trigger(
    on: &Option<TriggerYaml>,
) -> (
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Vec<PipelineInputDef>,
) {
    let Some(on) = on else {
        return ("manual".into(), None, None, None, None, None, vec![]);
    };

    if let Some(ref manual) = on.manual {
        return (
            "manual".into(),
            None,
            None,
            None,
            None,
            None,
            project_input_defs(manual.inputs.as_ref()),
        );
    }

    if let Some(ref webhook) = on.webhook {
        let secret = webhook
            .secret
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        return (
            "webhook".into(),
            webhook.path.clone(),
            secret,
            webhook.environment.clone(),
            None,
            None,
            project_input_defs(webhook.inputs.as_ref()),
        );
    }

    if let Some(ref cron) = on.cron {
        return (
            "cron".into(),
            None,
            None,
            None,
            cron.schedule.clone(),
            cron.environment.clone(),
            project_input_defs(cron.inputs.as_ref()),
        );
    }

    ("manual".into(), None, None, None, None, None, vec![])
}

fn parse_concurrency_timeout_seconds(raw: Option<&str>) -> i64 {
    match raw.map(str::trim).filter(|s| !s.is_empty()) {
        None => 1800,
        Some(s) => shared::pipeline_yaml::parse_duration_seconds(s).unwrap_or(1800),
    }
}
