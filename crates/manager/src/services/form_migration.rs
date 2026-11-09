//! One-shot startup migration: every `source = 'form'` pipeline gets its
//! YAML file written and bound (file becomes the authority, `source='yaml'`).
//!
//! After form retirement the API only ever creates file-backed pipelines,
//! so this converges the pre-retirement rows. A pipeline whose migration
//! fails (e.g. its legacy content no longer validates) is left in place and
//! reported — it keeps working from the DB projection and the migration
//! retries on the next startup.

use sqlx::Row;

use super::pipeline_yaml_emit::{yaml_from_row_parts, RowSource, RowStep};
#[allow(clippy::too_many_arguments)] // migration context: pool, paths, settings
pub async fn migrate_form_pipelines(
    pool: &sqlx::SqlitePool,
    yaml_scan_dir: &str,
    step_index_dir: &std::path::Path,
    sync_timeout_secs: u64,
) {
    let rows = match sqlx::query(
        "SELECT id, name, description, trigger_type, webhook_path, webhook_secret,
                webhook_environment, cron_expr, cron_environment, runner_labels, timeout_seconds,
                concurrency_group, concurrency_mode, concurrency_max_queue,
                concurrency_queue_timeout_seconds, owner_group, environments, notify_json, input_defs
         FROM pipelines WHERE source = 'form' ORDER BY created_at ASC",
    )
    .fetch_all(pool)
    .await
    {
        Ok(rows) => rows,
        Err(error) => {
            tracing::error!("form migration: cannot list form pipelines: {error}");
            return;
        }
    };
    if rows.is_empty() {
        return;
    }
    tracing::info!("form migration: converting {} form pipeline(s)", rows.len());

    let mut migrated = 0usize;
    let mut failed = 0usize;
    for row in &rows {
        match migrate_one(pool, yaml_scan_dir, step_index_dir, sync_timeout_secs, row).await {
            Ok(()) => migrated += 1,
            Err(error) => {
                failed += 1;
                let id: String = row.get(0);
                let name: String = row.get(1);
                tracing::error!(
                    pipeline = %id,
                    name = %name,
                    error = %error,
                    "form migration: pipeline left as form — it keeps working from the DB; \
                     fix the reported problem and restart to retry"
                );
            }
        }
    }
    if migrated > 0 || failed > 0 {
        tracing::info!(
            migrated,
            failed,
            "form migration finished — pipelines are file-backed now"
        );
    }
}

async fn migrate_one(
    pool: &sqlx::SqlitePool,
    yaml_scan_dir: &str,
    step_index_dir: &std::path::Path,
    sync_timeout_secs: u64,
    row: &sqlx::sqlite::SqliteRow,
) -> anyhow::Result<()> {
    let id: String = row.get(0);
    let name: String = row.get(1);
    let runner_labels: Vec<String> =
        serde_json::from_str(&row.get::<String, _>(9)).unwrap_or_default();
    let environments: Vec<String> =
        serde_json::from_str(&row.get::<String, _>(16)).unwrap_or_default();
    let notify_json: String = row.get(17);
    let notify: Vec<shared::pipeline_yaml::NotifyChannelYaml> =
        serde_json::from_str(&notify_json).unwrap_or_default();
    let input_defs: Vec<shared::protocol::PipelineInputDef> =
        serde_json::from_str(&row.get::<String, _>(18)).unwrap_or_default();

    // Inline env pairs.
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

    // Secret / variable binding NAMES (YAML references names, not IDs).
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

    // Marketplace resolutions for `uses:` reconstruction.
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

    let source = RowSource {
        name: name.clone(),
        description: row.get(2),
        trigger_type: row.get(3),
        webhook_path: row.get(4),
        webhook_secret: row.get(5),
        webhook_environment: row.get(6),
        cron_expr: row.get(7),
        cron_environment: row.get(8),
        runner_labels,
        timeout_seconds: row.get(10),
        concurrency_group: row.get(11),
        concurrency_mode: row.get(12),
        concurrency_max_queue: row.get(13),
        concurrency_queue_timeout_seconds: row.get(14),
        owner_group: row.get(15),
        environments,
        notify,
        input_defs,
        env_pairs,
        secret_names,
        var_names,
        resolutions,
        steps,
    };
    let yaml = yaml_from_row_parts(&source)?;

    let filename = super::pipeline_scanner::filename_from_pipeline_name(&name);
    let guard = super::pipeline_scanner::lock_yaml_file(yaml_scan_dir, &filename).await?;
    super::pipeline_scanner::write_and_upsert_yaml_locked(
        pool,
        yaml_scan_dir,
        &filename,
        &yaml,
        step_index_dir,
        Some(&id),
        sync_timeout_secs,
        guard,
    )
    .await?;
    tracing::info!(
        pipeline = %id,
        name = %name,
        file = %filename,
        "form migration: pipeline converted to a YAML file"
    );
    Ok(())
}
