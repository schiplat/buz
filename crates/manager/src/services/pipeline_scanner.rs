use sqlx::{Row, SqlitePool};
use std::path::PathBuf;
use std::time::Duration;
use tokio::time;
use uuid::Uuid;

use shared::pipeline_yaml::{
    default_concurrency_mode, parse_and_validate, ConcurrencyDef, PipelineYaml, TriggerYaml,
};
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

    let mut found_paths: Vec<PathBuf> = Vec::new();

    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
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
        let file_path = path.to_string_lossy().to_string();

        match tokio::fs::read_to_string(&path).await {
            Ok(raw) => match serde_yaml::from_str::<PipelineYaml>(&raw) {
                Ok(parsed) => {
                    let result = parse_and_validate(&raw);
                    if !result.valid {
                        tracing::warn!(
                            "YAML scanner: {filename} has validation errors: {:?}",
                            result.errors
                        );
                    } else {
                        if let Err(e) = upsert_from_yaml(pool, &file_path, &parsed).await {
                            tracing::error!("YAML scanner: failed to upsert {filename}: {e}");
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

        found_paths.push(path);
    }

    // Deactivate pipelines whose source file was removed
    if let Err(e) = deactivate_removed(pool, &found_paths).await {
        tracing::warn!("YAML scanner: cleanup failed: {e}");
    }
}

/// Upsert a pipeline from its parsed YAML definition.
/// Match is by `yaml_path` + `source='yaml'` — same file = same pipeline.
async fn upsert_from_yaml(
    pool: &SqlitePool,
    file_path: &str,
    parsed: &PipelineYaml,
) -> anyhow::Result<()> {
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
        .and_then(|s| {
            if let Some(stripped) = s.strip_suffix('m') {
                stripped.parse::<i64>().ok().map(|v| v * 60)
            } else if let Some(stripped) = s.strip_suffix('h') {
                stripped.parse::<i64>().ok().map(|v| v * 3600)
            } else {
                s.parse::<i64>().ok()
            }
        })
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

    // Resolve team: YAML `team:` / `owner_group:` slug → team_id (fallback: default)
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
                dir: s.dir.clone(),
                timeout: s.timeout.clone(),
                retry: s.retry,
                continue_on_error: s.continue_on_error,
                secrets: s.secrets.clone(),
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

    // Pipeline-level secrets only — step-level secrets are stored per-task (step_secrets column)
    // and resolved at poll time per-step. This prevents cross-step secret leakage.
    let pipeline_secrets: Vec<String> = parsed.secrets.clone().unwrap_or_default();

    // Check if pipeline already exists by yaml_path
    let existing = sqlx::query("SELECT id FROM pipelines WHERE yaml_path = ? AND source = 'yaml'")
        .bind(file_path)
        .fetch_optional(pool)
        .await?;

    let labels_json = serde_json::to_string(&runner_labels).unwrap_or_else(|_| "[]".into());
    let input_defs_json = serde_json::to_string(&input_defs).unwrap_or_else(|_| "[]".into());

    if let Some(row) = existing {
        let id: String = row.get(0);

        sqlx::query(
            "UPDATE pipelines SET name = ?, description = ?, trigger_type = ?,
             webhook_path = ?, cron_expr = ?, runner_labels = ?, timeout_seconds = ?,
             concurrency_group = ?, concurrency_mode = ?, concurrency_max_queue = ?,
             concurrency_queue_timeout_seconds = ?,
             owner_group = ?, team_id = ?, input_defs = ?, is_active = 1,
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
        .bind(file_path)
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
    }

    Ok(())
}

/// Resolve YAML `team` / `owner_group` slug to `(team_id, slug)`.
/// Creates no teams — falls back to the bootstrapped `default` team when slug is unknown.
async fn resolve_team_for_yaml(pool: &SqlitePool, slug: &str) -> anyhow::Result<(String, String)> {
    if let Some(row) = sqlx::query("SELECT id, slug FROM teams WHERE slug = ?")
        .bind(slug)
        .fetch_optional(pool)
        .await?
    {
        return Ok((row.get(0), row.get(1)));
    }

    if slug != "default" {
        tracing::warn!("YAML scanner: unknown team slug '{slug}', falling back to 'default'");
    }

    if let Some(row) = sqlx::query("SELECT id, slug FROM teams WHERE slug = 'default'")
        .fetch_optional(pool)
        .await?
    {
        return Ok((row.get(0), row.get(1)));
    }

    anyhow::bail!("no team '{slug}' and no 'default' team — ensure RBAC bootstrap has run")
}

/// Deactivate pipelines whose YAML source file has been deleted from disk.
async fn deactivate_removed(pool: &SqlitePool, found_paths: &[PathBuf]) -> anyhow::Result<()> {
    let found: Vec<String> = found_paths
        .iter()
        .map(|p| p.to_string_lossy().to_string())
        .collect();

    let rows = sqlx::query(
        "SELECT id, name, yaml_path FROM pipelines WHERE source = 'yaml' AND is_active = 1",
    )
    .fetch_all(pool)
    .await?;

    for row in &rows {
        let yaml_path: Option<String> = row.get(2);
        if let Some(ref path) = yaml_path {
            if !found.contains(path) {
                let id: String = row.get(0);
                let name: String = row.get(1);
                sqlx::query(
                    "UPDATE pipelines SET is_active = 0, updated_at = datetime('now') WHERE id = ?",
                )
                .bind(&id)
                .execute(pool)
                .await?;
                tracing::info!("YAML scanner: deactivated {name} ({id}) — file removed: {path}");
            }
        }
    }

    Ok(())
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
    raw.and_then(|s| {
        let s = s.trim();
        if let Some(stripped) = s.strip_suffix('m') {
            stripped.parse::<i64>().ok().map(|v| v * 60)
        } else if let Some(stripped) = s.strip_suffix('h') {
            stripped.parse::<i64>().ok().map(|v| v * 3600)
        } else {
            s.parse::<i64>().ok()
        }
    })
    .unwrap_or(1800)
}
