use sqlx::{Row, SqlitePool};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::time;
use uuid::Uuid;

use shared::error::AppError;
use shared::pipeline_yaml::{default_concurrency_mode, ConcurrencyDef, PipelineYaml, TriggerYaml};
use shared::protocol::{EnvPair, PipelineInputDef, StepDef};

use crate::api::{
    insert_env_vars, insert_pipeline_secret_bindings, insert_pipeline_var_bindings, insert_steps,
};
/// Periodically scan `yaml_scan_dir` for `.yml`/`.yaml` files,
/// parsing them into pipeline definitions and upserting into the DB.
pub fn spawn_scanner(pool: SqlitePool, dir: String, interval_secs: u64) {
    tokio::spawn(async move {
        tracing::info!("Pipeline YAML scanner started (dir: {dir}, interval: {interval_secs}s)");

        let mut tick = time::interval(Duration::from_secs(interval_secs));
        // Do an immediate scan on startup
        tokio::task::yield_now().await;
        scan_and_sync(&pool, &dir).await;

        loop {
            tick.tick().await;
            scan_and_sync(&pool, &dir).await;
        }
    });
}

async fn scan_and_sync(pool: &SqlitePool, dir: &str) {
    let Ok(mut entries) = tokio::fs::read_dir(dir).await else {
        tracing::warn!("YAML scanner: cannot read directory {dir}");
        return;
    };

    // Flat scan only — no private subdirectories under yaml_scan_dir.
    let mut found_names: Vec<String> = Vec::new();

    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        let Ok(ft) = entry.file_type().await else {
            continue;
        };
        if ft.is_dir() {
            tracing::debug!(
                "YAML scanner: skip subdirectory {} (flat scan_dir only)",
                path.display()
            );
            continue;
        }
        let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
            continue;
        };
        if ext != "yml" && ext != "yaml" {
            continue;
        }

        let filename = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();
        // Temp / hidden files (e.g. `.foo.yml.buz-tmp`) are not pipelines.
        if filename.starts_with('.') {
            continue;
        }
        if validate_yaml_filename(&filename).is_err() {
            tracing::warn!("YAML scanner: skip invalid filename {filename}");
            continue;
        }

        // DB key is the basename (normalized) — never absolute/relative path strings.
        let yaml_key = filename.clone();

        match tokio::fs::read_to_string(&path).await {
            Ok(raw) => match serde_yaml::from_str::<PipelineYaml>(&raw) {
                Ok(parsed) => {
                    match crate::services::yaml_validate::validate_pipeline_yaml(pool, &raw).await {
                        Ok(result) if !result.valid => {
                            tracing::warn!(
                                "YAML scanner: {filename} has validation errors: {:?}",
                                result.errors
                            );
                        }
                        Ok(_) => {
                            if let Err(e) = upsert_from_yaml(pool, &yaml_key, &parsed).await {
                                tracing::error!("YAML scanner: failed to upsert {filename}: {e:#}");
                            }
                        }
                        Err(e) => {
                            tracing::warn!("YAML scanner: validate {filename} failed: {e}");
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("YAML scanner: failed to parse {filename}: {e}");
                }
            },
            Err(e) => {
                tracing::warn!("YAML scanner: failed to read {filename}: {e}");
            }
        }

        found_names.push(yaml_key);
    }

    // Deactivate pipelines whose source file was removed
    if let Err(e) = deactivate_removed(pool, &found_names).await {
        tracing::warn!("YAML scanner: cleanup failed: {e}");
    }
}

/// Upsert a pipeline from its parsed YAML definition.
/// Match is by `yaml_path` + `source='yaml'` — same file = same pipeline.
/// Returns the pipeline id.
pub async fn upsert_from_yaml(
    pool: &SqlitePool,
    file_path: &str,
    parsed: &PipelineYaml,
) -> anyhow::Result<String> {
    let pipeline_name = parsed.name.trim();
    let description = parsed.description.clone().unwrap_or_default();

    let (trigger_type, webhook_path, cron_expr, input_defs) = extract_trigger(&parsed.on);

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

    let steps: Vec<StepDef> = parsed
        .steps
        .iter()
        .map(|s| {
            let (config_source, config_key_path, config_dest, config_render) =
                shared::pipeline_yaml::flatten_config(s.config.as_ref());
            StepDef {
                name: s.name.clone().unwrap_or_else(|| "unnamed".into()),
                run: s.run.clone().unwrap_or_default(),
                uses: s.uses.clone(),
                image: s.image.clone(),
                volumes: s.volumes.clone(),
                host: s.host.clone(),
                host_labels: s.host_labels.clone(),
                hosts_strategy: s.hosts_strategy.clone(),
                dir: s.dir.clone(),
                timeout: s.timeout.clone(),
                retry: s.retry,
                continue_on_error: s.continue_on_error,
                secrets: s.secrets.clone(),
                env: s.env.clone(),
                privileged: s.privileged,
                registry_secret: s.registry_secret.clone(),
                config: s.config.clone(),
                config_source,
                config_key_path,
                config_dest,
                config_render,
            }
        })
        .collect();

    // Effective allowlist: optional top-level `secrets:` ∪ step lists ∪ purpose
    // slots ∪ `${secrets.X}` in env maps. Top-level list is optional (GHA-style).
    // Step `secrets:` column still stores per-step narrowing for claim-time scope.
    let pipeline_secrets: Vec<String> =
        shared::pipeline_yaml::collect_effective_secret_names(parsed);

    // Normalize key to basename and migrate legacy absolute/relative paths.
    let yaml_key = normalize_yaml_path_key(file_path);
    let existing = find_yaml_pipeline_row(pool, &yaml_key).await?;

    let labels_json = serde_json::to_string(&runner_labels).unwrap_or_else(|_| "[]".into());
    let input_defs_json = serde_json::to_string(&input_defs).unwrap_or_else(|_| "[]".into());

    if let Some(id) = existing {
        // Do not force is_active=1 — respect deactivate / UI freeze across rescans.
        sqlx::query(
            "UPDATE pipelines SET name = ?, description = ?, trigger_type = ?,
             webhook_path = ?, cron_expr = ?, runner_labels = ?, timeout_seconds = ?,
             concurrency_group = ?, concurrency_mode = ?, concurrency_max_queue = ?,
             concurrency_queue_timeout_seconds = ?,
             owner_group = ?, team_id = ?, input_defs = ?, yaml_path = ?,
             updated_at = datetime('now')
             WHERE id = ?",
        )
        .bind(pipeline_name)
        .bind(&description)
        .bind(&trigger_type)
        .bind(&webhook_path)
        .bind(&cron_expr)
        .bind(&labels_json)
        .bind(timeout_seconds)
        .bind(&concurrency_group)
        .bind(concurrency_mode.as_deref().unwrap_or(""))
        .bind(concurrency_max_queue.map(|v| v as i64).unwrap_or(3))
        .bind(parse_concurrency_timeout_seconds(
            concurrency_queue_timeout.as_deref(),
        ))
        .bind(&resolved_slug)
        .bind(&team_id)
        .bind(&input_defs_json)
        .bind(&yaml_key)
        .bind(&id)
        .execute(pool)
        .await?;

        // Replace steps
        sqlx::query("DELETE FROM tasks WHERE pipeline_id = ?")
            .bind(&id)
            .execute(pool)
            .await?;
        sqlx::query("DELETE FROM variables WHERE pipeline_id = ?")
            .bind(&id)
            .execute(pool)
            .await?;
        sqlx::query("DELETE FROM pipeline_secrets WHERE pipeline_id = ?")
            .bind(&id)
            .execute(pool)
            .await?;
        sqlx::query("DELETE FROM pipeline_vars WHERE pipeline_id = ?")
            .bind(&id)
            .execute(pool)
            .await?;

        insert_steps(pool, &id, &steps).await?;
        tracing::debug!("YAML scanner: updated pipeline {pipeline_name} ({id})");

        if let Some(ref envs) = parsed.env {
            let pairs: Vec<EnvPair> = envs
                .iter()
                .map(|(k, v)| EnvPair {
                    name: k.clone(),
                    value: v.clone(),
                })
                .collect();
            insert_env_vars(pool, &id, &pairs).await?;
        }
        if !pipeline_secrets.is_empty() {
            // Same-team name resolve only (never bind another team's secret)
            let placeholders = vec!["?"; pipeline_secrets.len()].join(",");
            let query =
                format!("SELECT id FROM secrets WHERE team_id = ? AND name IN ({placeholders})");
            let mut q = sqlx::query_as::<_, (String,)>(&query).bind(&team_id);
            for name in &pipeline_secrets {
                q = q.bind(name);
            }
            let secret_ids: Vec<String> =
                q.fetch_all(pool).await?.into_iter().map(|r| r.0).collect();
            if secret_ids.len() != pipeline_secrets.len() {
                tracing::warn!(
                    pipeline = %pipeline_name,
                    team = %resolved_slug,
                    requested = ?pipeline_secrets,
                    bound = secret_ids.len(),
                    "YAML secrets: some names missing or not in this pipeline's team — skipped"
                );
            }
            insert_pipeline_secret_bindings(pool, &id, &secret_ids).await?;
        }
        if let Some(ref var_names) = parsed.variables {
            if !var_names.is_empty() {
                let placeholders: Vec<String> = var_names
                    .iter()
                    .enumerate()
                    .map(|(i, _)| format!("?{}", i + 1))
                    .collect();
                let query = format!(
                    "SELECT id FROM platform_vars WHERE name IN ({})",
                    placeholders.join(",")
                );
                let mut q = sqlx::query_as::<_, (String,)>(&query);
                for name in var_names {
                    q = q.bind(name);
                }
                let var_ids: Vec<String> =
                    q.fetch_all(pool).await?.into_iter().map(|r| r.0).collect();
                insert_pipeline_var_bindings(pool, &id, &var_ids).await?;
            }
        }

        Ok(id)
    } else {
        let id = Uuid::new_v4().to_string();

        sqlx::query(
            "INSERT INTO pipelines (id, name, description, trigger_type, webhook_path,
             cron_expr, runner_labels, timeout_seconds, concurrency_group, concurrency_mode,
             concurrency_max_queue, concurrency_queue_timeout_seconds,
             owner_group, team_id, source, yaml_path, input_defs, is_active)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'yaml', ?, ?, 1)",
        )
        .bind(&id)
        .bind(pipeline_name)
        .bind(&description)
        .bind(&trigger_type)
        .bind(&webhook_path)
        .bind(&cron_expr)
        .bind(&labels_json)
        .bind(timeout_seconds)
        .bind(&concurrency_group)
        .bind(concurrency_mode.as_deref().unwrap_or(""))
        .bind(concurrency_max_queue.map(|v| v as i64).unwrap_or(3))
        .bind(parse_concurrency_timeout_seconds(
            concurrency_queue_timeout.as_deref(),
        ))
        .bind(&resolved_slug)
        .bind(&team_id)
        .bind(&yaml_key)
        .bind(&input_defs_json)
        .execute(pool)
        .await?;

        insert_steps(pool, &id, &steps).await?;

        if let Some(ref envs) = parsed.env {
            let pairs: Vec<EnvPair> = envs
                .iter()
                .map(|(k, v)| EnvPair {
                    name: k.clone(),
                    value: v.clone(),
                })
                .collect();
            insert_env_vars(pool, &id, &pairs).await?;
        }
        if !pipeline_secrets.is_empty() {
            // Same-team name resolve only (never bind another team's secret)
            let placeholders = vec!["?"; pipeline_secrets.len()].join(",");
            let query =
                format!("SELECT id FROM secrets WHERE team_id = ? AND name IN ({placeholders})");
            let mut q = sqlx::query_as::<_, (String,)>(&query).bind(&team_id);
            for name in &pipeline_secrets {
                q = q.bind(name);
            }
            let secret_ids: Vec<String> =
                q.fetch_all(pool).await?.into_iter().map(|r| r.0).collect();
            if secret_ids.len() != pipeline_secrets.len() {
                tracing::warn!(
                    pipeline = %pipeline_name,
                    team = %resolved_slug,
                    requested = ?pipeline_secrets,
                    bound = secret_ids.len(),
                    "YAML secrets: some names missing or not in this pipeline's team — skipped"
                );
            }
            insert_pipeline_secret_bindings(pool, &id, &secret_ids).await?;
        }
        if let Some(ref var_names) = parsed.variables {
            if !var_names.is_empty() {
                let placeholders: Vec<String> = var_names
                    .iter()
                    .enumerate()
                    .map(|(i, _)| format!("?{}", i + 1))
                    .collect();
                let query = format!(
                    "SELECT id FROM platform_vars WHERE name IN ({})",
                    placeholders.join(",")
                );
                let mut q = sqlx::query_as::<_, (String,)>(&query);
                for name in var_names {
                    q = q.bind(name);
                }
                let var_ids: Vec<String> =
                    q.fetch_all(pool).await?.into_iter().map(|r| r.0).collect();
                insert_pipeline_var_bindings(pool, &id, &var_ids).await?;
            }
        }

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
    pool: &SqlitePool,
    filename: &str,
) -> Result<Option<String>, AppError> {
    validate_yaml_filename(filename)?;
    if let Some(id) = sqlx::query_scalar::<_, String>(
        "SELECT id FROM pipelines WHERE yaml_path = ? AND source = 'yaml'",
    )
    .bind(filename)
    .fetch_optional(pool)
    .await?
    {
        return Ok(Some(id));
    }
    // Legacy rows may store absolute/relative paths — match by basename and rewrite.
    let rows = sqlx::query("SELECT id, yaml_path FROM pipelines WHERE source = 'yaml'")
        .fetch_all(pool)
        .await?;
    for row in rows {
        let id: String = row.get(0);
        let path: Option<String> = row.get(1);
        let Some(path) = path else { continue };
        if normalize_yaml_path_key(&path) == filename {
            sqlx::query("UPDATE pipelines SET yaml_path = ? WHERE id = ?")
                .bind(filename)
                .bind(&id)
                .execute(pool)
                .await?;
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
pub async fn read_yaml_file(scan_dir: &str, filename: &str) -> Result<String, AppError> {
    let path = resolve_yaml_path(scan_dir, filename)?;
    assert_path_under_scan_dir(scan_dir, &path)?;
    tokio::fs::read_to_string(&path)
        .await
        .map_err(|e| AppError::NotFound(format!("cannot read {filename}: {e}")))
}

/// List `.yml`/`.yaml` files under the scan directory, joined with DB projection rows.
pub async fn list_yaml_files(
    pool: &SqlitePool,
    scan_dir: &str,
) -> Result<Vec<YamlFileInfo>, AppError> {
    let mut files: Vec<YamlFileInfo> = Vec::new();
    let Ok(mut entries) = tokio::fs::read_dir(scan_dir).await else {
        return Ok(files);
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        let Ok(ft) = entry.file_type().await else {
            continue;
        };
        if ft.is_dir()
            || path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with('.'))
        {
            continue;
        }
        let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
            continue;
        };
        if ext != "yml" && ext != "yaml" {
            continue;
        }
        let filename = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();
        if validate_yaml_filename(&filename).is_err() {
            continue;
        }
        let yaml_path = filename.clone();
        let pipeline_id = find_yaml_pipeline_row(pool, &filename).await?;
        files.push(match pipeline_id {
            Some(id) => {
                let r = sqlx::query("SELECT name, team_id, is_active FROM pipelines WHERE id = ?")
                    .bind(&id)
                    .fetch_one(pool)
                    .await?;
                YamlFileInfo {
                    filename,
                    yaml_path,
                    pipeline_id: Some(id),
                    pipeline_name: Some(r.get(0)),
                    team_id: Some(r.get(1)),
                    is_active: Some(r.get::<i64, _>(2) != 0),
                }
            }
            None => YamlFileInfo {
                filename,
                yaml_path,
                pipeline_id: None,
                pipeline_name: None,
                team_id: None,
                is_active: None,
            },
        });
    }
    files.sort_by(|a, b| a.filename.cmp(&b.filename));
    Ok(files)
}

/// Validate YAML, write to scan dir, and immediately upsert the DB projection.
///
/// Write order: backup → temp file → rename → (optional bind) → upsert.
/// On upsert failure the previous file content is restored when possible.
///
/// When `bind_pipeline_id` is set (migrating a Form/API pipeline), that row is
/// retargeted to `source=yaml` + this path before upsert so the id is preserved.
/// `is_active` is never forced on.
pub async fn write_and_upsert_yaml(
    pool: &SqlitePool,
    scan_dir: &str,
    filename: &str,
    content: &str,
    bind_pipeline_id: Option<&str>,
) -> Result<YamlFileWriteResult, AppError> {
    let filename = filename.trim();
    validate_yaml_filename(filename)?;
    let path = resolve_yaml_path(scan_dir, filename)?;
    assert_path_under_scan_dir(scan_dir, &path)?;

    let validation = crate::services::yaml_validate::validate_pipeline_yaml(pool, content).await?;
    crate::services::yaml_validate::reject_if_invalid(&validation)?;
    let parsed: PipelineYaml = serde_yaml::from_str(content).map_err(|e| {
        AppError::ValidationFailed(format!("YAML syntax error after validate: {e}"))
    })?;

    tokio::fs::create_dir_all(scan_dir)
        .await
        .map_err(|e| AppError::Internal(format!("cannot create yaml_scan_dir: {e}")))?;

    // DB key = basename (flat scan_dir; no subdirectory paths).
    let yaml_path = filename.to_string();

    // Preflight bind conflicts before touching the filesystem.
    let mut bind_prev: Option<(String, Option<String>)> = None; // (source, yaml_path)
    if let Some(pid) = bind_pipeline_id {
        let existing = sqlx::query("SELECT id, source, yaml_path FROM pipelines WHERE id = ?")
            .bind(pid)
            .fetch_optional(pool)
            .await?
            .ok_or_else(|| AppError::NotFound("pipeline not found".into()))?;
        let prev_source: String = existing.get(1);
        let prev_path: Option<String> = existing.get(2);
        if let Some(other_id) = find_yaml_pipeline_row(pool, &yaml_path).await? {
            if other_id != pid {
                return Err(AppError::Conflict(format!(
                    "file {filename} already bound to pipeline {other_id}"
                )));
            }
        }
        bind_prev = Some((prev_source, prev_path));
    }

    let file_backup = if path.exists() {
        Some(
            tokio::fs::read(&path)
                .await
                .map_err(|e| AppError::Internal(format!("failed to backup {filename}: {e}")))?,
        )
    } else {
        None
    };

    // Temp name is not a public YAML basename (leading '.') — bypass filename rules.
    let tmp_path = {
        let p = std::path::Path::new(scan_dir).join(format!(".{filename}.buz-tmp"));
        assert_path_under_scan_dir(scan_dir, &p)?;
        p
    };

    tokio::fs::write(&tmp_path, content)
        .await
        .map_err(|e| AppError::Internal(format!("failed to write temp {filename}: {e}")))?;
    tokio::fs::rename(&tmp_path, &path).await.map_err(|e| {
        let _ = std::fs::remove_file(&tmp_path);
        AppError::Internal(format!("failed to replace {filename}: {e}"))
    })?;

    if let Some(pid) = bind_pipeline_id {
        // Preserve pipeline id; do not force is_active.
        if let Err(e) = sqlx::query(
            "UPDATE pipelines SET source = 'yaml', yaml_path = ?, updated_at = datetime('now') WHERE id = ?",
        )
        .bind(&yaml_path)
        .bind(pid)
        .execute(pool)
        .await
        {
            restore_yaml_file(&path, file_backup.as_deref()).await;
            return Err(AppError::Internal(format!("failed to bind pipeline: {e}")));
        }
    }

    let pipeline_id = match upsert_from_yaml(pool, &yaml_path, &parsed).await {
        Ok(id) => id,
        Err(e) => {
            restore_yaml_file(&path, file_backup.as_deref()).await;
            if let (Some(pid), Some((prev_source, prev_path))) =
                (bind_pipeline_id, bind_prev.as_ref())
            {
                let _ = sqlx::query(
                    "UPDATE pipelines SET source = ?, yaml_path = ?, updated_at = datetime('now') WHERE id = ?",
                )
                .bind(prev_source)
                .bind(prev_path)
                .bind(pid)
                .execute(pool)
                .await;
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

async fn restore_yaml_file(path: &std::path::Path, backup: Option<&[u8]>) {
    match backup {
        Some(bytes) => {
            if let Err(e) = tokio::fs::write(path, bytes).await {
                tracing::error!("failed to restore {}: {e}", path.display());
            }
        }
        None => {
            if let Err(e) = tokio::fs::remove_file(path).await {
                tracing::warn!("failed to remove incomplete {}: {e}", path.display());
            }
        }
    }
}

/// Delete a YAML file and remove its DB projection row (hard delete).
pub async fn delete_yaml_file(
    pool: &SqlitePool,
    scan_dir: &str,
    filename: &str,
) -> Result<Option<String>, AppError> {
    let path = resolve_yaml_path(scan_dir, filename)?;
    assert_path_under_scan_dir(scan_dir, &path)?;
    let yaml_path = filename.trim().to_string();
    validate_yaml_filename(&yaml_path)?;

    let pipeline_id = find_yaml_pipeline_row(pool, &yaml_path).await?;

    if path.exists() {
        tokio::fs::remove_file(&path)
            .await
            .map_err(|e| AppError::Internal(format!("failed to delete {filename}: {e}")))?;
    }

    if let Some(ref id) = pipeline_id {
        sqlx::query("DELETE FROM pipelines WHERE id = ?")
            .bind(id)
            .execute(pool)
            .await?;
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

/// Resolve YAML `team` / `owner_group` slug to `(team_id, slug)`.
/// Creates no teams — unknown slug fails (Validate / create / update must match).
async fn resolve_team_for_yaml(pool: &SqlitePool, slug: &str) -> anyhow::Result<(String, String)> {
    if let Some(row) = sqlx::query("SELECT id, slug FROM teams WHERE slug = ?")
        .bind(slug)
        .fetch_optional(pool)
        .await?
    {
        return Ok((row.get(0), row.get(1)));
    }

    anyhow::bail!(
        "team '{slug}' does not exist — create it under Teams or fix team: / owner_group:"
    )
}

/// Deactivate pipelines whose YAML source file has been deleted from disk.
async fn deactivate_removed(pool: &SqlitePool, found_basenames: &[String]) -> anyhow::Result<()> {
    let found: std::collections::HashSet<&str> =
        found_basenames.iter().map(String::as_str).collect();

    let rows = sqlx::query(
        "SELECT id, name, yaml_path FROM pipelines WHERE source = 'yaml' AND is_active = 1",
    )
    .fetch_all(pool)
    .await?;

    for row in &rows {
        let yaml_path: Option<String> = row.get(2);
        if let Some(ref path) = yaml_path {
            let key = normalize_yaml_path_key(path);
            if !found.contains(key.as_str()) {
                let id: String = row.get(0);
                let name: String = row.get(1);
                sqlx::query(
                    "UPDATE pipelines SET is_active = 0, updated_at = datetime('now') WHERE id = ?",
                )
                .bind(&id)
                .execute(pool)
                .await?;
                tracing::info!("YAML scanner: deactivated {name} ({id}) — file removed: {key}");
            }
        }
    }

    Ok(())
}

/// Before trigger: re-read file, validate, and refresh DB projection (reject if invalid).
pub async fn sync_yaml_before_trigger(
    pool: &SqlitePool,
    scan_dir: &str,
    pipeline_id: &str,
) -> Result<(), AppError> {
    let row = sqlx::query("SELECT source, yaml_path FROM pipelines WHERE id = ?")
        .bind(pipeline_id)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| AppError::NotFound("pipeline not found".into()))?;
    let source: String = row.get(0);
    if source != "yaml" {
        return Ok(());
    }
    let yaml_path: Option<String> = row.get(1);
    let yaml_path =
        yaml_path.ok_or_else(|| AppError::NotFound("pipeline has no yaml_path".into()))?;
    let filename = normalize_yaml_path_key(&yaml_path);
    validate_yaml_filename(&filename)?;

    let content = read_yaml_file(scan_dir, &filename)
        .await
        .map_err(|e| match e {
            AppError::NotFound(_) => AppError::ValidationFailed(format!(
                "YAML file '{filename}' is missing under yaml_scan_dir — cannot trigger"
            )),
            other => other,
        })?;

    let validation = crate::services::yaml_validate::validate_pipeline_yaml(pool, &content).await?;
    if !validation.valid {
        let msg = validation
            .errors
            .iter()
            .map(|e| format!("{}: {}", e.field, e.message))
            .collect::<Vec<_>>()
            .join("; ");
        return Err(AppError::ValidationFailed(format!(
            "YAML file '{filename}' failed validation — fix the file before triggering: {msg}"
        )));
    }

    let parsed: PipelineYaml = serde_yaml::from_str(&content).map_err(|e| {
        AppError::ValidationFailed(format!("YAML syntax error in '{filename}': {e}"))
    })?;
    upsert_from_yaml(pool, &filename, &parsed)
        .await
        .map_err(|e| AppError::Internal(format!("failed to sync YAML projection: {e:#}")))?;
    Ok(())
}

/// Status of the on-disk YAML vs DB projection (for Definition UI).
#[derive(Debug, Clone, serde::Serialize)]
pub struct YamlSyncStatus {
    pub filename: String,
    /// `in_sync` | `stale` | `validation_failed` | `missing_file` | `not_file_backed`
    pub status: String,
    pub last_synced_at: Option<String>,
    pub file_mtime: Option<String>,
    pub etag: Option<String>,
    pub errors: Vec<String>,
}

pub async fn yaml_sync_status(
    pool: &SqlitePool,
    scan_dir: &str,
    pipeline_id: &str,
) -> Result<YamlSyncStatus, AppError> {
    let row = sqlx::query("SELECT source, yaml_path, updated_at FROM pipelines WHERE id = ?")
        .bind(pipeline_id)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| AppError::NotFound("pipeline not found".into()))?;

    let source: String = row.get(0);
    let yaml_path: Option<String> = row.get(1);
    let updated_at: Option<String> = row.get(2);

    if source != "yaml" {
        return Ok(YamlSyncStatus {
            filename: String::new(),
            status: "not_file_backed".into(),
            last_synced_at: updated_at,
            file_mtime: None,
            etag: None,
            errors: vec![],
        });
    }

    let filename = yaml_path
        .as_deref()
        .map(normalize_yaml_path_key)
        .filter(|s| !s.is_empty())
        .unwrap_or_default();
    if filename.is_empty() || validate_yaml_filename(&filename).is_err() {
        return Ok(YamlSyncStatus {
            filename,
            status: "missing_file".into(),
            last_synced_at: updated_at,
            file_mtime: None,
            etag: None,
            errors: vec!["pipeline has no valid yaml_path".into()],
        });
    }

    let path = resolve_yaml_path(scan_dir, &filename)?;
    let meta = match tokio::fs::metadata(&path).await {
        Ok(m) => m,
        Err(_) => {
            let err = format!("file '{filename}' not found under yaml_scan_dir");
            return Ok(YamlSyncStatus {
                filename,
                status: "missing_file".into(),
                last_synced_at: updated_at,
                file_mtime: None,
                etag: None,
                errors: vec![err],
            });
        }
    };

    let file_mtime = meta.modified().ok().map(|t| {
        let dt: chrono::DateTime<chrono::Utc> = t.into();
        dt.format("%Y-%m-%d %H:%M:%S").to_string()
    });

    let content = tokio::fs::read_to_string(&path)
        .await
        .map_err(|e| AppError::Internal(format!("read {filename}: {e}")))?;
    let etag = yaml_content_etag(&content);

    let validation = crate::services::yaml_validate::validate_pipeline_yaml(pool, &content).await?;
    if !validation.valid {
        let errors = validation
            .errors
            .iter()
            .map(|e| format!("{}: {}", e.field, e.message))
            .collect();
        return Ok(YamlSyncStatus {
            filename,
            status: "validation_failed".into(),
            last_synced_at: updated_at,
            file_mtime,
            etag: Some(etag),
            errors,
        });
    }

    // Stale when file mtime is clearly newer than last DB projection update.
    let stale = match (&file_mtime, &updated_at) {
        (Some(fm), Some(db)) => fm.as_str() > db.as_str(),
        _ => false,
    };

    Ok(YamlSyncStatus {
        filename,
        status: if stale {
            "stale".into()
        } else {
            "in_sync".into()
        },
        last_synced_at: updated_at,
        file_mtime,
        etag: Some(etag),
        errors: vec![],
    })
}

/// Extract trigger information from the YAML `on` block.
fn extract_trigger(
    on: &Option<TriggerYaml>,
) -> (
    String,
    Option<String>,
    Option<String>,
    Vec<PipelineInputDef>,
) {
    let Some(on) = on else {
        return ("manual".into(), None, None, vec![]);
    };

    if let Some(ref manual) = on.manual {
        let input_defs: Vec<PipelineInputDef> = manual
            .inputs
            .as_ref()
            .map(|map| {
                map.iter()
                    .map(|(name, def)| PipelineInputDef {
                        name: name.clone(),
                        description: def.description.clone(),
                        required: def.required,
                        default: def.default.clone(),
                        masking: def.masking.filter(|m| *m),
                        input_type: def.r#type.clone(),
                        pattern: def.pattern.clone(),
                        options: def.options.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        return ("manual".into(), None, None, input_defs);
    }

    if let Some(ref webhook) = on.webhook {
        return ("webhook".into(), webhook.path.clone(), None, vec![]);
    }

    if let Some(ref cron) = on.cron {
        return ("cron".into(), None, cron.schedule.clone(), vec![]);
    }

    ("manual".into(), None, None, vec![])
}

fn parse_concurrency_timeout_seconds(raw: Option<&str>) -> i64 {
    match raw.map(str::trim).filter(|s| !s.is_empty()) {
        None => 1800,
        Some(s) => shared::pipeline_yaml::parse_duration_seconds(s).unwrap_or(1800),
    }
}

#[cfg(test)]
#[path = "tests/pipeline_scanner_path_tests.rs"]
mod path_tests;
