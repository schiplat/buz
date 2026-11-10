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

use sqlx::Row;

use super::pipeline_yaml_emit::{yaml_from_row_parts, RowSource, RowStep};

#[allow(clippy::too_many_arguments)] // migration context: pool, paths, settings
pub async fn backfill_definitions(
    pool: &sqlx::SqlitePool,
    legacy_scan_dir: &str,
    step_index_dir: &std::path::Path,
    sync_timeout_secs: u64,
) {
    let rows = match sqlx::query(
        "SELECT id, name, source, yaml_path, description, trigger_type, webhook_path,
                webhook_secret, webhook_environment, cron_expr, cron_environment,
                runner_labels, timeout_seconds, concurrency_group, concurrency_mode,
                concurrency_max_queue, concurrency_queue_timeout_seconds, owner_group,
                environments, notify_json, input_defs
         FROM pipelines WHERE yaml_source = '' ORDER BY created_at ASC",
    )
    .fetch_all(pool)
    .await
    {
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
                let id: String = row.get(0);
                let name: String = row.get(1);
                tracing::error!(
                    pipeline = %id,
                    name = %name,
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
    pool: &sqlx::SqlitePool,
    legacy_scan_dir: &str,
    step_index_dir: &std::path::Path,
    sync_timeout_secs: u64,
    row: &sqlx::sqlite::SqliteRow,
) -> anyhow::Result<()> {
    let id: String = row.get(0);
    let name: String = row.get(1);
    let source: String = row.get(2);
    let yaml_path: Option<String> = row.get(3);

    // Prefer the legacy file (it was the authority before this release).
    let legacy_filename = if source == "yaml" {
        yaml_path
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
        .unwrap_or_else(|| super::pipeline_scanner::filename_from_pipeline_name(&name));
    let yaml = match rendered {
        Some(text) => text,
        None => yaml_from_row_parts(&read_row_source(pool, row).await?)?,
    };

    super::pipeline_scanner::write_and_upsert_yaml(
        pool,
        &filename,
        &yaml,
        step_index_dir,
        Some(&id),
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

async fn read_row_source(
    pool: &sqlx::SqlitePool,
    row: &sqlx::sqlite::SqliteRow,
) -> anyhow::Result<RowSource> {
    let id: String = row.get(0);
    let runner_labels: Vec<String> =
        serde_json::from_str(&row.get::<String, _>(11)).unwrap_or_default();
    let environments: Vec<String> =
        serde_json::from_str(&row.get::<String, _>(18)).unwrap_or_default();
    let notify: Vec<shared::pipeline_yaml::NotifyChannelYaml> =
        serde_json::from_str(&row.get::<String, _>(19)).unwrap_or_default();
    let input_defs: Vec<shared::protocol::PipelineInputDef> =
        serde_json::from_str(&row.get::<String, _>(20)).unwrap_or_default();

    let env_pairs: Vec<(String, String)> = sqlx::query(
        "SELECT var_name, value FROM variables
         WHERE pipeline_id = ? AND source_type = 'inline' AND is_secret = 0
         ORDER BY var_name",
    )
    .bind(&id)
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|r| (r.get::<String, _>(0), r.get::<String, _>(1)))
    .collect();

    let secret_names: Vec<String> = sqlx::query(
        "SELECT s.name FROM pipeline_secrets ps
         JOIN secrets s ON s.id = ps.secret_id
         WHERE ps.pipeline_id = ? ORDER BY s.name",
    )
    .bind(&id)
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|r| r.get(0))
    .collect();
    let var_names: Vec<String> = sqlx::query(
        "SELECT pv.name FROM pipeline_vars pvars
         JOIN platform_vars pv ON pv.id = pvars.var_id
         WHERE pvars.pipeline_id = ? ORDER BY pv.name",
    )
    .bind(&id)
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|r| r.get(0))
    .collect();

    let resolutions: Vec<(i64, String, String)> = sqlx::query(
        "SELECT step_order, uses_ref, resolved_version FROM step_resolutions
         WHERE pipeline_id = ? ORDER BY step_order",
    )
    .bind(&id)
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|r| {
        (
            r.get::<i64, _>(0),
            r.get::<String, _>(1),
            r.get::<String, _>(2),
        )
    })
    .collect();

    let task_rows = sqlx::query(
        "SELECT step_order, name, exec_type, COALESCE(command_template, ''), COALESCE(image, ''),
                COALESCE(work_dir, ''), COALESCE(timeout_seconds, 0), COALESCE(retry_count, 0),
                COALESCE(fail_strategy, 'stop'), COALESCE(step_secrets, ''),
                COALESCE(step_env, '{}'), COALESCE(volumes, '[]'), COALESCE(privileged, 0),
                COALESCE(hosts_strategy, 'chain'), COALESCE(hosts_max_parallel, 0),
                COALESCE(needs_json, '[]'), COALESCE(host_names, ''), COALESCE(host_labels, '')
         FROM tasks WHERE pipeline_id = ? ORDER BY step_order",
    )
    .bind(&id)
    .fetch_all(pool)
    .await?;

    let mut steps = Vec::with_capacity(task_rows.len());
    for task in &task_rows {
        steps.push(RowStep {
            step_order: task.get::<i64, _>(0),
            name: task.get(1),
            exec_type: task.get(2),
            command_template: task.get(3),
            image: task.get(4),
            work_dir: task.get(5),
            timeout_seconds: task.get(6),
            retry_count: task.get(7),
            fail_strategy: task.get(8),
            step_secrets: task.get(9),
            step_env_json: task.get(10),
            volumes: serde_json::from_str(&task.get::<String, _>(11)).unwrap_or_default(),
            privileged: task.get::<i64, _>(12) != 0,
            hosts_strategy: task.get(13),
            hosts_max_parallel: task.get(14),
            needs: serde_json::from_str(&task.get::<String, _>(15)).unwrap_or_default(),
            host_names: task.get(16),
            host_labels: task.get(17),
        });
    }

    Ok(RowSource {
        name: row.get(1),
        description: row.get(4),
        trigger_type: row.get(5),
        webhook_path: row.get(6),
        webhook_secret: row.get(7),
        webhook_environment: row.get(8),
        cron_expr: row.get(9),
        cron_environment: row.get(10),
        runner_labels,
        timeout_seconds: row.get(12),
        concurrency_group: row.get(13),
        concurrency_mode: row.get(14),
        concurrency_max_queue: row.get(15),
        concurrency_queue_timeout_seconds: row.get(16),
        owner_group: row.get(17),
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
