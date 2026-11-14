//! One-shot startup backfill: every pipeline row without a stored definition
//! gets its canonical YAML persisted (`pipelines.yaml_source`).
//!
//! The database is the authority for pipeline definitions. Rows written before
//! that change carry only the relational projection. Two sources, in order:
//!
//! 1. the legacy file at `<yaml_scan_dir>/<yaml_path>` when it still exists —
//!    it was the authority until this release, so its bytes win;
//! 2. otherwise the relational projection, rendered back to YAML.
//!
//! A row that cannot be backfilled keeps working from the projection (jobs
//! trigger from `tasks`), is logged, and is retried on the next startup. The
//! legacy directory is never written: it is an import/export location only.

use anyhow::anyhow;

use crate::store::exec::DbPool;
use crate::store::pipelines::{BackfillRow, BackfillSourceParts};

use super::pipeline_yaml_emit::{yaml_from_row_parts, RowSource, RowStep};

#[allow(clippy::too_many_arguments)] // migration context: pool, paths, settings
pub async fn backfill_definitions(
    pool: &DbPool,
    legacy_scan_dir: &str,
    step_index_dir: &std::path::Path,
    sync_timeout_secs: u64,
) {
    let rows = match crate::store::pipelines::definition_backfill_rows(pool).await {
        Ok(rows) => rows,
        Err(error) => {
            tracing::error!("definition backfill: cannot list pipelines: {error}");
            return;
        }
    };
    if rows.is_empty() {
        return;
    }
    tracing::info!(
        "definition backfill: storing YAML for {} pipeline(s)",
        rows.len()
    );

    let mut stored = 0usize;
    let mut failed = 0usize;
    for row in &rows {
        match backfill_one(
            pool,
            legacy_scan_dir,
            step_index_dir,
            sync_timeout_secs,
            row,
        )
        .await
        {
            Ok(()) => stored += 1,
            Err(error) => {
                failed += 1;
                tracing::error!(
                    pipeline = %row.id,
                    name = %row.name,
                    error = %error,
                    "definition backfill: pipeline keeps running from its projection; \
                     re-save it to store a definition"
                );
            }
        }
    }
    tracing::info!(stored, failed, "definition backfill finished");
}

async fn backfill_one(
    pool: &DbPool,
    legacy_scan_dir: &str,
    step_index_dir: &std::path::Path,
    sync_timeout_secs: u64,
    row: &BackfillRow,
) -> anyhow::Result<()> {
    let id = &row.id;
    let name = &row.name;

    // Prefer the legacy file (it was the authority before this release).
    let legacy_filename = if row.source == "yaml" {
        row.yaml_path
            .as_deref()
            .and_then(super::pipeline_scanner::filename_from_yaml_path)
    } else {
        None
    };
    let rendered = match legacy_filename.as_deref() {
        Some(filename) => {
            let path = std::path::Path::new(legacy_scan_dir).join(filename);
            match tokio::fs::read_to_string(&path).await {
                Ok(text) => match super::yaml_validate::validate_pipeline_yaml(pool, &text).await {
                    Ok(validation) if validation.valid => Some(text),
                    Ok(_) => {
                        tracing::warn!(
                            pipeline = %id,
                            file = %path.display(),
                            "definition backfill: legacy file no longer validates — using the stored projection"
                        );
                        None
                    }
                    Err(error) => {
                        tracing::warn!(
                            pipeline = %id,
                            file = %path.display(),
                            error = %error,
                            "definition backfill: cannot validate legacy file — using the stored projection"
                        );
                        None
                    }
                },
                Err(_) => None,
            }
        }
        None => None,
    };

    let filename = legacy_filename
        .clone()
        .unwrap_or_else(|| super::pipeline_scanner::filename_from_pipeline_name(name));
    let yaml = match rendered {
        Some(text) => text,
        None => yaml_from_row_parts(&read_row_source(pool, row).await?)?,
    };

    super::pipeline_scanner::write_and_upsert_yaml(
        pool,
        &filename,
        &yaml,
        step_index_dir,
        Some(id),
        sync_timeout_secs,
        None,
        false,
    )
    .await?;
    tracing::info!(
        pipeline = %id,
        name = %name,
        file = %filename,
        "definition backfill: stored YAML"
    );
    Ok(())
}

async fn read_row_source(pool: &DbPool, row: &BackfillRow) -> anyhow::Result<RowSource> {
    let BackfillSourceParts {
        env_pairs,
        secret_names,
        var_names,
        resolutions,
        tasks,
    } = crate::store::pipelines::backfill_source_parts(pool, &row.id)
        .await
        .map_err(|e| anyhow!("{e}"))?;

    let runner_labels: Vec<String> = serde_json::from_str(&row.runner_labels).unwrap_or_default();
    let environments: Vec<String> = serde_json::from_str(&row.environments).unwrap_or_default();
    let notify: Vec<shared::pipeline_yaml::NotifyChannelYaml> =
        serde_json::from_str(&row.notify_json).unwrap_or_default();
    let input_defs: Vec<shared::protocol::PipelineInputDef> =
        serde_json::from_str(&row.input_defs).unwrap_or_default();

    let mut steps = Vec::with_capacity(tasks.len());
    for task in tasks {
        steps.push(RowStep {
            step_order: task.step_order,
            name: task.name,
            exec_type: task.exec_type,
            command_template: task.command_template,
            image: task.image,
            work_dir: task.work_dir,
            timeout_seconds: task.timeout_seconds,
            retry_count: task.retry_count,
            fail_strategy: task.fail_strategy,
            step_secrets: task.step_secrets,
            step_env_json: task.step_env_json,
            volumes: serde_json::from_str(&task.volumes).unwrap_or_default(),
            privileged: task.privileged != 0,
            hosts_strategy: task.hosts_strategy,
            hosts_max_parallel: Some(task.hosts_max_parallel),
            needs: serde_json::from_str(&task.needs_json).unwrap_or_default(),
            host_names: task.host_names,
            host_labels: task.host_labels,
        });
    }

    Ok(RowSource {
        name: row.name.clone(),
        description: row.description.clone(),
        trigger_type: row.trigger_type.clone(),
        webhook_path: row.webhook_path.clone(),
        webhook_secret: row.webhook_secret.clone().unwrap_or_default(),
        webhook_environment: row.webhook_environment.clone().unwrap_or_default(),
        cron_expr: row.cron_expr.clone(),
        cron_environment: row.cron_environment.clone().unwrap_or_default(),
        runner_labels,
        timeout_seconds: row.timeout_seconds,
        concurrency_group: row.concurrency_group.clone(),
        concurrency_mode: row.concurrency_mode.clone(),
        concurrency_max_queue: row.concurrency_max_queue,
        concurrency_queue_timeout_seconds: row.concurrency_queue_timeout_seconds,
        owner_group: row.owner_group.clone(),
        environments,
        notify,
        input_defs,
        env_pairs,
        secret_names,
        var_names,
        resolutions,
        steps,
    })
}
