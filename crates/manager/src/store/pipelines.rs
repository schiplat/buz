//! Pipeline row lookups shared across handlers.

use crate::store::exec::{self, DbPool, DbTx, SqlValue};
use uuid::Uuid;

use shared::error::AppError;

/// `(source, yaml_path, name)` for one pipeline — the definition-page header
/// facts (origin basename + storage source).
pub async fn pipeline_source_row(
    pool: &DbPool,
    id: &str,
) -> Result<(String, Option<String>, String), AppError> {
    let row = exec::fetch_optional(
        pool,
        "SELECT source, yaml_path, name FROM pipelines WHERE id = ?",
        &[id.into()],
    )
    .await?
    .ok_or_else(|| AppError::NotFound("pipeline not found".into()))?;
    Ok((row.text(0), row.opt_text(1), row.text(2)))
}

/// Pipeline concurrency decision inside the caller's write transaction:
/// `pending` (run now) or `queued`, or a conflict when the mode forbids it.
///
/// Guarded writes only — the caller owns the `BEGIN IMMEDIATE` scope and any
/// post-commit fan-out.
pub async fn resolve_concurrency_status_in_tx(
    tx: &mut DbTx<'_>,
    pipeline_id: &str,
    concurrency_group: Option<&str>,
    concurrency_mode: &str,
    concurrency_max_queue: i64,
) -> Result<&'static str, AppError> {
    let Some(group) = concurrency_group.filter(|g| !g.is_empty()) else {
        return Ok("pending");
    };

    let mode = if concurrency_mode.is_empty() {
        "discard-new"
    } else {
        concurrency_mode
    };

    match mode {
        "discard-new" => {
            let count = tx
                .fetch_optional(
                    "SELECT COUNT(*) as cnt FROM jobs
                 WHERE pipeline_id = ? AND status IN ('pending', 'running', 'queued')",
                    &[pipeline_id.into()],
                )
                .await?
                .map(|r| r.i64(0))
                .unwrap_or(0);
            if count > 0 {
                return Err(AppError::Conflict(format!(
                    "concurrency group '{group}' has {count} active/queued job(s)"
                )));
            }
            Ok("pending")
        }

        "discard-old" => {
            tx.execute(
                "UPDATE jobs SET status = 'cancelled', updated_at = datetime('now')
                 WHERE pipeline_id = ? AND status = 'queued'",
                &[pipeline_id.into()],
            )
            .await?;
            Ok("pending")
        }

        "queue" => {
            let queued_count = tx
                .fetch_optional(
                    "SELECT COUNT(*) FROM jobs
                 WHERE pipeline_id = ? AND status = 'queued'",
                    &[pipeline_id.into()],
                )
                .await?
                .map(|r| r.i64(0))
                .unwrap_or(0);

            if queued_count >= concurrency_max_queue {
                return Err(AppError::Conflict(format!(
                    "queue full: {queued_count} jobs already queued (max {concurrency_max_queue})"
                )));
            }

            let running_count = tx
                .fetch_optional(
                    "SELECT COUNT(*) FROM jobs
                 WHERE pipeline_id = ? AND status IN ('pending', 'running')",
                    &[pipeline_id.into()],
                )
                .await?
                .map(|r| r.i64(0))
                .unwrap_or(0);

            if running_count == 0 {
                Ok("pending")
            } else {
                Ok("queued")
            }
        }

        _ => Err(AppError::ValidationFailed(format!(
            "unknown concurrency mode '{mode}'"
        ))),
    }
}

/// Pipeline concurrency decision, owning its transaction — the read-only-gate
/// variant used by re-run and trigger when no other writes share the scope.
pub async fn resolve_concurrency_status(
    pool: &DbPool,
    pipeline_id: &str,
    concurrency_group: Option<&str>,
    concurrency_mode: &str,
    concurrency_max_queue: i64,
) -> Result<&'static str, AppError> {
    let mut tx = pool.begin_immediate().await?;
    let status = resolve_concurrency_status_in_tx(
        &mut tx,
        pipeline_id,
        concurrency_group,
        concurrency_mode,
        concurrency_max_queue,
    )
    .await?;
    tx.commit().await?;
    Ok(status)
}

/// The pipeline's declared environment slugs as stored JSON text — the
/// git-hook create/update path validates against it.
pub async fn declared_environments(pool: &DbPool, pipeline_id: &str) -> Result<String, AppError> {
    Ok(exec::fetch_optional(
        pool,
        "SELECT environments FROM pipelines WHERE id = ?",
        &[pipeline_id.into()],
    )
    .await?
    .ok_or(sqlx::Error::RowNotFound)?
    .text(0))
}

// ───────────────────────────── read models ─────────────────────────────
//
// Interim note: read handlers map wide rows into JSON payloads; until the PG
// port gives each of them a typed projection, these return the fetched rows
// verbatim (the mapping code stays with the handler). Every statement here is
// compiled against the schema by `store_sql_validation_test`.

/// One pipeline's full edit-form row (`None` = no such pipeline).
pub struct PipelineFullRow {
    pub id: String,
    pub name: String,
    pub description: String,
    pub trigger_type: String,
    pub webhook_path: Option<String>,
    pub webhook_secret: Option<String>,
    pub cron_expr: Option<String>,
    pub runner_labels: String,
    pub timeout_seconds: i64,
    pub source: String,
    pub input_defs: String,
    pub concurrency_group: Option<String>,
    pub is_active: i64,
    pub owner_group: String,
    pub team_id: String,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    pub concurrency_mode: Option<String>,
    pub concurrency_max_queue: Option<i64>,
    pub concurrency_queue_timeout_seconds: Option<i64>,
    pub yaml_path: Option<String>,
    pub environments: String,
    pub notify_json: String,
}

fn full_row_of(r: exec::GenericRow) -> PipelineFullRow {
    PipelineFullRow {
        id: r.text(0),
        name: r.text(1),
        description: r.text(2),
        trigger_type: r.text(3),
        webhook_path: r.opt_text(4),
        webhook_secret: r.opt_text(5),
        cron_expr: r.opt_text(6),
        runner_labels: r.text(7),
        timeout_seconds: r.i64(8),
        source: r.text(9),
        input_defs: r.text(10),
        concurrency_group: r.opt_text(11),
        is_active: r.i64(12),
        owner_group: r.text(13),
        team_id: r.text(14),
        created_at: r.opt_text(15),
        updated_at: r.opt_text(16),
        concurrency_mode: r.opt_text(17),
        concurrency_max_queue: r.opt_i64(18),
        concurrency_queue_timeout_seconds: r.opt_i64(19),
        yaml_path: r.opt_text(20),
        environments: r.text(21),
        notify_json: r.text(22),
    }
}

pub async fn full_row(pool: &DbPool, id: &str) -> Result<Option<PipelineFullRow>, AppError> {
    Ok(exec::fetch_optional(
        pool,
        "SELECT id, name, description, trigger_type, webhook_path, webhook_secret, cron_expr,
                runner_labels, timeout_seconds, source, input_defs, concurrency_group, is_active,
                owner_group, team_id, created_at, updated_at,
                concurrency_mode, concurrency_max_queue, concurrency_queue_timeout_seconds,
                yaml_path, environments, notify_json
         FROM pipelines WHERE id = ?",
        &[id.into()],
    )
    .await?
    .map(full_row_of))
}

/// Secret ids explicitly bound to a pipeline.
pub async fn secret_bind_ids(pool: &DbPool, pipeline_id: &str) -> Result<Vec<String>, AppError> {
    Ok(exec::fetch_all(
        pool,
        "SELECT secret_id FROM pipeline_secrets WHERE pipeline_id = ?",
        &[pipeline_id.into()],
    )
    .await?
    .into_iter()
    .map(|r| r.text(0))
    .collect())
}

/// Variable ids explicitly bound to a pipeline.
pub async fn var_bind_ids(pool: &DbPool, pipeline_id: &str) -> Result<Vec<String>, AppError> {
    Ok(exec::fetch_all(
        pool,
        "SELECT var_id FROM pipeline_vars WHERE pipeline_id = ?",
        &[pipeline_id.into()],
    )
    .await?
    .into_iter()
    .map(|r| r.text(0))
    .collect())
}

/// Inline (non-secret) `env:` pairs of a pipeline, name-ordered.
pub async fn inline_envs(
    pool: &DbPool,
    pipeline_id: &str,
) -> Result<Vec<(String, String)>, AppError> {
    Ok(exec::fetch_all(
        pool,
        "SELECT var_name, value FROM variables
         WHERE pipeline_id = ? AND source_type = 'inline' AND is_secret = 0
         ORDER BY var_name",
        &[pipeline_id.into()],
    )
    .await?
    .into_iter()
    .map(|r| (r.text(0), r.text(1)))
    .collect())
}

/// One edit-form task row.
pub struct PipelineTaskRow {
    pub id: String,
    pub step_order: i64,
    pub name: String,
    pub exec_type: String,
    pub host_id: Option<String>,
    pub host_names: String,
    pub host_labels: String,
    pub command_template: String,
    pub image: String,
    pub work_dir: String,
    pub retry_count: i64,
    pub timeout_seconds: i64,
    pub fail_strategy: String,
    pub volumes: String,
    pub step_secrets: String,
    pub step_env: String,
    pub privileged: i64,
    pub hosts_strategy: String,
    pub capabilities: String,
    pub hosts_max_parallel: Option<i64>,
    pub needs_json: String,
}

fn task_row_of(r: exec::GenericRow) -> PipelineTaskRow {
    PipelineTaskRow {
        id: r.text(0),
        step_order: r.i64(1),
        name: r.text(2),
        exec_type: r.text(3),
        host_id: r.opt_text(4),
        host_names: r.text(5),
        host_labels: r.text(6),
        command_template: r.text(7),
        image: r.text(8),
        work_dir: r.text(9),
        retry_count: r.i64(10),
        timeout_seconds: r.i64(11),
        fail_strategy: r.text(12),
        volumes: r.text(13),
        step_secrets: r.text(14),
        step_env: r.text(15),
        privileged: r.i64(16),
        hosts_strategy: r.text(17),
        capabilities: r.text(18),
        hosts_max_parallel: r.opt_i64(19),
        needs_json: r.text(20),
    }
}

/// The edit-form task rows of a pipeline, step-ordered.
pub async fn tasks_rows(
    pool: &DbPool,
    pipeline_id: &str,
) -> Result<Vec<PipelineTaskRow>, AppError> {
    Ok(exec::fetch_all(
        pool,
        "SELECT id, step_order, name, exec_type, host_id, host_names, host_labels, command_template, image,
                work_dir, retry_count, timeout_seconds, fail_strategy, volumes, step_secrets,
                step_env, privileged,
                hosts_strategy, capabilities, hosts_max_parallel, needs_json
         FROM tasks WHERE pipeline_id = ? ORDER BY step_order",
        &[pipeline_id.into()],
    )
    .await?
    .into_iter()
    .map(task_row_of)
    .collect())
}

/// One trigger-time snapshot task row (column order differs from the
/// edit-form shape).
pub struct PipelineTaskSnapshotRow {
    pub step_order: i64,
    pub name: String,
    pub exec_type: String,
    pub host_id: Option<String>,
    pub host_names: String,
    pub host_labels: String,
    pub command_template: String,
    pub image: String,
    pub work_dir: String,
    pub retry_count: i64,
    pub timeout_seconds: i64,
    pub fail_strategy: String,
    pub step_secrets: String,
    pub step_env: String,
    pub volumes: String,
    pub privileged: i64,
    pub hosts_strategy: String,
    pub id: String,
    pub capabilities: String,
    pub hosts_max_parallel: Option<i64>,
    pub needs_json: String,
}

fn task_snapshot_row_of(r: exec::GenericRow) -> PipelineTaskSnapshotRow {
    PipelineTaskSnapshotRow {
        step_order: r.i64(0),
        name: r.text(1),
        exec_type: r.text(2),
        host_id: r.opt_text(3),
        host_names: r.text(4),
        host_labels: r.text(5),
        command_template: r.text(6),
        image: r.text(7),
        work_dir: r.text(8),
        retry_count: r.i64(9),
        timeout_seconds: r.i64(10),
        fail_strategy: r.text(11),
        step_secrets: r.text(12),
        step_env: r.text(13),
        volumes: r.text(14),
        privileged: r.i64(15),
        hosts_strategy: r.text(16),
        id: r.text(17),
        capabilities: r.text(18),
        hosts_max_parallel: r.opt_i64(19),
        needs_json: r.text(20),
    }
}

/// The trigger-time snapshot rows of a pipeline, step-ordered.
pub async fn task_snapshot_rows(
    pool: &DbPool,
    pipeline_id: &str,
) -> Result<Vec<PipelineTaskSnapshotRow>, AppError> {
    Ok(exec::fetch_all(
        pool,
        "SELECT step_order, name, exec_type, host_id, host_names, host_labels, command_template,
                image, work_dir, retry_count, timeout_seconds, fail_strategy, step_secrets, step_env, volumes,
                privileged,
                hosts_strategy, id, capabilities, hosts_max_parallel, needs_json
         FROM tasks WHERE pipeline_id = ? ORDER BY step_order",
        &[pipeline_id.into()],
    )
    .await?
    .into_iter()
    .map(task_snapshot_row_of)
    .collect())
}

/// One marketplace step resolution.
pub struct PipelineStepResolutionRow {
    pub step_order: i64,
    pub uses_ref: String,
    pub resolved_version: String,
    pub content_sha256: String,
    pub source: String,
    pub reviewed: i64,
    pub input_defs_json: String,
}

/// Marketplace step resolutions of a pipeline.
pub async fn step_resolutions(
    pool: &DbPool,
    pipeline_id: &str,
) -> Result<Vec<PipelineStepResolutionRow>, AppError> {
    Ok(exec::fetch_all(
        pool,
        "SELECT step_order, uses_ref, resolved_version, content_sha256, source, reviewed, input_defs_json \
         FROM step_resolutions WHERE pipeline_id = ?",
        &[pipeline_id.into()],
    )
    .await?
    .into_iter()
    .map(|r| PipelineStepResolutionRow {
        step_order: r.i64(0),
        uses_ref: r.text(1),
        resolved_version: r.text(2),
        content_sha256: r.text(3),
        source: r.text(4),
        reviewed: r.i64(5),
        input_defs_json: r.text(6),
    })
    .collect())
}

/// `(kind, value)` allowlist entries of one environment.
pub async fn env_host_allowlist(
    pool: &DbPool,
    environment_id: &str,
) -> Result<Vec<(String, String)>, AppError> {
    Ok(exec::fetch_all(
        pool,
        "SELECT kind, value FROM environment_hosts WHERE environment_id = ?",
        &[environment_id.into()],
    )
    .await?
    .into_iter()
    .map(|r| (r.text(0), r.text(1)))
    .collect())
}

/// `(name, labels-json)` of every host — the label-membership preload.
pub async fn host_label_rows(pool: &DbPool) -> Result<Vec<(String, String)>, AppError> {
    Ok(exec::fetch_all(pool, "SELECT name, labels FROM hosts", &[])
        .await?
        .into_iter()
        .map(|r| (r.text(0), r.text(1)))
        .collect())
}

/// Registered host names — the trigger input validator's host universe.
pub async fn all_host_names(pool: &DbPool) -> Result<Vec<String>, AppError> {
    Ok(exec::fetch_all(pool, "SELECT name FROM hosts", &[])
        .await?
        .into_iter()
        .map(|r| r.text(0))
        .collect())
}

/// `(var_name, is_secret)` of a pipeline's inline variables — the snapshot's
/// env redaction flags.
/// One inline variable's redaction flag.
pub struct PipelineVarFlagRow {
    pub name: String,
    pub is_secret: i64,
}

pub async fn pipeline_var_flags(
    pool: &DbPool,
    pipeline_id: &str,
) -> Result<Vec<PipelineVarFlagRow>, AppError> {
    Ok(exec::fetch_all(
        pool,
        "SELECT var_name, is_secret FROM variables WHERE pipeline_id = ?",
        &[pipeline_id.into()],
    )
    .await?
    .into_iter()
    .map(|r| PipelineVarFlagRow {
        name: r.text(0),
        is_secret: r.i64(1),
    })
    .collect())
}

/// `(trigger_type, timeout_seconds, source)` of one pipeline.
/// `(trigger_type, timeout_seconds, source)` frozen into the job snapshot.
pub struct PipelineTriggerSnapshotRow {
    pub trigger_type: String,
    pub timeout_seconds: i64,
    pub source: String,
}

pub async fn trigger_snapshot_row(
    pool: &DbPool,
    pipeline_id: &str,
) -> Result<Option<PipelineTriggerSnapshotRow>, AppError> {
    Ok(exec::fetch_optional(
        pool,
        "SELECT trigger_type, timeout_seconds, source
         FROM pipelines WHERE id = ?",
        &[pipeline_id.into()],
    )
    .await?
    .map(|r| PipelineTriggerSnapshotRow {
        trigger_type: r.text(0),
        timeout_seconds: r.i64(1),
        source: r.text(2),
    }))
}

/// Stored notify channels JSON of one pipeline (`None` = unset/absent row).
pub async fn notify_json_of(pool: &DbPool, pipeline_id: &str) -> Result<Option<String>, AppError> {
    Ok(exec::fetch_optional(
        pool,
        "SELECT notify_json FROM pipelines WHERE id = ?",
        &[pipeline_id.into()],
    )
    .await?
    .map(|r| r.text(0)))
}

/// The trigger gate's pipeline row: identity, concurrency config, inputs,
/// team, environments, active flag, resource profile (`None` = no such row).
/// The trigger gate's pipeline row.
pub struct PipelineTriggerRow {
    pub id: String,
    pub name: String,
    pub runner_labels: String,
    pub concurrency_group: Option<String>,
    pub concurrency_mode: String,
    pub concurrency_max_queue: i64,
    pub concurrency_queue_timeout_seconds: i64,
    pub input_defs: String,
    pub team_id: String,
    pub environments: String,
    pub is_active: i64,
    pub resource_profile: String,
}

pub async fn trigger_row(
    pool: &DbPool,
    pipeline_id: &str,
) -> Result<Option<PipelineTriggerRow>, AppError> {
    Ok(exec::fetch_optional(
        pool,
        "SELECT id, name, runner_labels, concurrency_group, concurrency_mode,
                concurrency_max_queue, concurrency_queue_timeout_seconds, input_defs, team_id,
                environments, is_active, resource_profile
         FROM pipelines WHERE id = ?",
        &[pipeline_id.into()],
    )
    .await?
    .map(|r| PipelineTriggerRow {
        id: r.text(0),
        name: r.text(1),
        runner_labels: r.text(2),
        concurrency_group: r.opt_text(3),
        concurrency_mode: r.text(4),
        concurrency_max_queue: r.i64(5),
        concurrency_queue_timeout_seconds: r.i64(6),
        input_defs: r.text(7),
        team_id: r.text(8),
        environments: r.text(9),
        is_active: r.i64(10),
        resource_profile: r.text(11),
    }))
}

/// Pipeline display name (`None` = no such row).
pub async fn name_of(pool: &DbPool, id: &str) -> Result<Option<String>, AppError> {
    Ok(exec::fetch_optional(
        pool,
        "SELECT name FROM pipelines WHERE id = ?",
        &[id.into()],
    )
    .await?
    .map(|r| r.text(0)))
}

/// Stored input definitions JSON (`None` = no such row).
pub async fn input_defs_of(pool: &DbPool, id: &str) -> Result<Option<String>, AppError> {
    Ok(exec::fetch_optional(
        pool,
        "SELECT input_defs FROM pipelines WHERE id = ?",
        &[id.into()],
    )
    .await?
    .map(|r| r.text(0)))
}

/// Audit before-snapshot facts of an update (`None` = no such pipeline).
pub async fn update_facts(
    pool: &DbPool,
    id: &str,
) -> Result<Option<(String, String, String, String, bool)>, AppError> {
    let row = exec::fetch_optional(
        pool,
        "SELECT name, trigger_type, team_id, owner_group, is_active FROM pipelines WHERE id = ?",
        &[id.into()],
    )
    .await?;
    Ok(row.map(|r| (r.text(0), r.text(1), r.text(2), r.text(3), r.i64(4) != 0)))
}

/// `owner_group` slug of one pipeline (errors when absent).
pub async fn owner_group_of(pool: &DbPool, id: &str) -> Result<String, AppError> {
    Ok(exec::fetch_optional(
        pool,
        "SELECT owner_group FROM pipelines WHERE id = ?",
        &[id.into()],
    )
    .await?
    .ok_or(sqlx::Error::RowNotFound)?
    .text(0))
}

/// Stored team id of one pipeline (`None` = no such pipeline).
pub async fn team_id_of(pool: &DbPool, id: &str) -> Result<Option<String>, AppError> {
    Ok(exec::fetch_optional(
        pool,
        "SELECT team_id FROM pipelines WHERE id = ?",
        &[id.into()],
    )
    .await?
    .map(|r| r.text(0)))
}

/// Stored team id of one pipeline (errors when absent).
pub async fn team_of(pool: &DbPool, id: &str) -> Result<String, AppError> {
    Ok(exec::fetch_optional(
        pool,
        "SELECT team_id FROM pipelines WHERE id = ?",
        &[id.into()],
    )
    .await?
    .ok_or(sqlx::Error::RowNotFound)?
    .text(0))
}

/// Active (non-terminal) job count of a pipeline — the team-move guard.
pub async fn active_job_count(pool: &DbPool, pipeline_id: &str) -> Result<i64, AppError> {
    Ok(exec::fetch_scalar_i64(
        pool,
        "SELECT COUNT(*) FROM jobs WHERE pipeline_id = ? \
             AND status IN ('pending', 'queued', 'waiting_approval', 'running')",
        &[pipeline_id.into()],
    )
    .await?)
}

/// Active job count inside the caller's transaction (the re-check that makes
/// the team-move guard race-free; same statement, same scope).
pub async fn active_job_count_tx(tx: &mut DbTx<'_>, pipeline_id: &str) -> Result<i64, AppError> {
    Ok(tx
        .fetch_optional(
            "SELECT COUNT(*) FROM jobs WHERE pipeline_id = ? \
             AND status IN ('pending', 'queued', 'waiting_approval', 'running')",
            &[pipeline_id.into()],
        )
        .await?
        .map(|r| r.i64(0))
        .unwrap_or(0))
}

/// Audit before-snapshot facts of a delete (`None` = no such pipeline).
pub async fn delete_facts(
    pool: &DbPool,
    id: &str,
) -> Result<Option<(String, String, String, String, String, Option<String>)>, AppError> {
    Ok(exec::fetch_optional(
        pool,
        "SELECT name, trigger_type, team_id, owner_group, source, yaml_path FROM pipelines WHERE id = ?",
        &[id.into()],
    )
    .await?
    .map(|r| {
        (
            r.text(0),
            r.text(1),
            r.text(2),
            r.text(3),
            r.text(4),
            r.opt_text(5),
        )
    }))
}

/// Delete one pipeline row; `false` when it did not exist.
pub async fn delete(pool: &DbPool, id: &str) -> Result<bool, AppError> {
    let affected = exec::execute(pool, "DELETE FROM pipelines WHERE id = ?", &[id.into()]).await?;
    Ok(affected != 0)
}

/// `(name, is_active)` of one pipeline (`None` = no such row) — the pool
/// variant used by toggle-active.
pub async fn name_active_of(pool: &DbPool, id: &str) -> Result<Option<(String, bool)>, AppError> {
    Ok(exec::fetch_optional(
        pool,
        "SELECT name, is_active FROM pipelines WHERE id = ?",
        &[id.into()],
    )
    .await?
    .map(|r| (r.text(0), r.i64(1) != 0)))
}

/// `(name, is_active)` inside the caller's transaction — disable/enable's
/// read under the write lock.
pub async fn name_active_tx(
    tx: &mut DbTx<'_>,
    id: &str,
) -> Result<Option<(String, bool)>, AppError> {
    let row = tx
        .fetch_optional(
            "SELECT name, is_active FROM pipelines WHERE id = ?",
            &[id.into()],
        )
        .await?;
    Ok(row.map(|r| (r.text(0), r.i64(1) != 0)))
}

/// Flip a pipeline's active flag (toggle-active).
pub async fn set_active(pool: &DbPool, id: &str, active: bool) -> Result<(), AppError> {
    exec::execute(
        pool,
        "UPDATE pipelines SET is_active = ?, updated_at = datetime('now') WHERE id = ?",
        &[SqlValue::Int(active as i64), id.into()],
    )
    .await?;
    Ok(())
}

/// Disable inside the caller's `BEGIN IMMEDIATE` scope — the flip must be
/// serialized with the concurrent-trigger re-check (see
/// `transaction-boundaries.md`).
pub async fn set_active_tx(tx: &mut DbTx<'_>, id: &str, active: bool) -> Result<(), AppError> {
    let sql = if active {
        "UPDATE pipelines SET is_active = 1, updated_at = datetime('now') WHERE id = ?"
    } else {
        "UPDATE pipelines SET is_active = 0, updated_at = datetime('now') WHERE id = ?"
    };
    tx.execute(sql, &[id.into()]).await?;
    Ok(())
}

/// Ids of a pipeline's in-flight jobs, queue-fair order — the disable
/// cancellation scan, inside the caller's transaction.
pub async fn in_flight_job_ids_tx(
    tx: &mut DbTx<'_>,
    pipeline_id: &str,
) -> Result<Vec<String>, AppError> {
    Ok(tx
        .fetch_all(
            "SELECT id FROM jobs
             WHERE pipeline_id = ? AND status IN ('pending', 'running', 'queued', 'waiting_approval')
             ORDER BY created_at, seq",
            &[pipeline_id.into()],
        )
        .await?
        .into_iter()
        .map(|r| r.text(0))
        .collect())
}

// ───────────────────────────── write models ─────────────────────────────

/// Everything needed to insert one pipeline row.
#[derive(Debug, Default)]
pub struct NewPipeline {
    pub id: String,
    pub name: String,
    pub description: String,
    pub trigger_type: String,
    pub webhook_path: Option<String>,
    pub webhook_secret: Option<String>,
    pub cron_expr: Option<String>,
    pub runner_labels: String,
    pub timeout_seconds: i64,
    pub concurrency_group: Option<String>,
    pub concurrency_mode: String,
    pub concurrency_max_queue: i64,
    pub concurrency_queue_timeout_seconds: i64,
    pub owner_group: String,
    pub team_id: String,
    pub input_defs: String,
    pub environments: String,
    pub notify_json: String,
}

/// Insert one pipeline row inside the caller's transaction (the create path's
/// shell row; the YAML bind retargets `source` right after).
pub async fn insert_tx(tx: &mut DbTx<'_>, p: &NewPipeline) -> Result<(), AppError> {
    tx.execute(
        "INSERT INTO pipelines (id, name, description, trigger_type, webhook_path, webhook_secret,
         cron_expr, runner_labels, timeout_seconds, concurrency_group, concurrency_mode,
         concurrency_max_queue, concurrency_queue_timeout_seconds,
         owner_group, team_id, input_defs, environments, notify_json, is_active)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 1)",
        &[
            p.id.as_str().into(),
            p.name.as_str().into(),
            p.description.as_str().into(),
            p.trigger_type.as_str().into(),
            p.webhook_path.as_deref().into(),
            p.webhook_secret.as_deref().into(),
            p.cron_expr.as_deref().into(),
            p.runner_labels.as_str().into(),
            SqlValue::Int(p.timeout_seconds),
            p.concurrency_group.as_deref().into(),
            p.concurrency_mode.as_str().into(),
            SqlValue::Int(p.concurrency_max_queue),
            SqlValue::Int(p.concurrency_queue_timeout_seconds),
            p.owner_group.as_str().into(),
            p.team_id.as_str().into(),
            p.input_defs.as_str().into(),
            p.environments.as_str().into(),
            p.notify_json.as_str().into(),
        ],
    )
    .await?;
    Ok(())
}

/// Everything the structured update writes.
#[derive(Debug, Default)]
pub struct UpdatePipeline {
    pub name: String,
    pub description: String,
    pub trigger_type: String,
    pub webhook_path: Option<String>,
    pub webhook_secret: Option<String>,
    pub cron_expr: Option<String>,
    pub runner_labels: String,
    pub timeout_seconds: i64,
    pub concurrency_group: Option<String>,
    pub concurrency_mode: String,
    pub concurrency_max_queue: i64,
    pub concurrency_queue_timeout_seconds: i64,
    pub owner_group: String,
    pub team_id: String,
    pub input_defs: String,
    pub environments: String,
    pub notify_json: String,
}

/// Replace a pipeline's editable fields inside the caller's `BEGIN IMMEDIATE`
/// scope (the team-move guard re-check shares the lock).
pub async fn update_tx(tx: &mut DbTx<'_>, id: &str, p: &UpdatePipeline) -> Result<u64, AppError> {
    let affected = tx
        .execute(
            "UPDATE pipelines SET name = ?, description = ?, trigger_type = ?, webhook_path = ?,
         webhook_secret = ?, cron_expr = ?, runner_labels = ?, timeout_seconds = ?,
         concurrency_group = ?, concurrency_mode = ?, concurrency_max_queue = ?,
         concurrency_queue_timeout_seconds = ?, owner_group = ?, team_id = ?, input_defs = ?,
         environments = ?, notify_json = ?, updated_at = datetime('now')
         WHERE id = ?",
            &[
                p.name.as_str().into(),
                p.description.as_str().into(),
                p.trigger_type.as_str().into(),
                p.webhook_path.as_deref().into(),
                p.webhook_secret.as_deref().into(),
                p.cron_expr.as_deref().into(),
                p.runner_labels.as_str().into(),
                SqlValue::Int(p.timeout_seconds),
                p.concurrency_group.as_deref().into(),
                p.concurrency_mode.as_str().into(),
                SqlValue::Int(p.concurrency_max_queue),
                SqlValue::Int(p.concurrency_queue_timeout_seconds),
                p.owner_group.as_str().into(),
                p.team_id.as_str().into(),
                p.input_defs.as_str().into(),
                p.environments.as_str().into(),
                p.notify_json.as_str().into(),
                id.into(),
            ],
        )
        .await?;
    Ok(affected)
}

/// Stored value of one same-team pipeline secret (`None` = no such binding).
pub async fn team_secret_value_of(
    pool: &DbPool,
    pipeline_id: &str,
    secret_name: &str,
) -> Result<Option<String>, AppError> {
    Ok(exec::fetch_optional(
        pool,
        "SELECT s.value FROM secrets s
         JOIN pipelines p ON p.team_id = s.team_id AND p.team_id != ''
         WHERE p.id = ? AND s.name = ?",
        &[pipeline_id.into(), secret_name.into()],
    )
    .await?
    .map(|r| r.text(0)))
}

/// `Some(name)` when a team secret with this name exists — the
/// webhook-secret binding check. `None` = no such secret in the team.
pub async fn team_secret_id_by_name(
    pool: &DbPool,
    team_id: &str,
    name: &str,
) -> Result<Option<String>, AppError> {
    Ok(exec::fetch_optional(
        pool,
        "SELECT id FROM secrets WHERE team_id = ? AND name = ?",
        &[team_id.into(), name.into()],
    )
    .await?
    .map(|r| r.text(0)))
}

/// Id of the host with this exact name, inside the caller's transaction
/// (`None` = unknown host — step resolution logs and defers).
pub async fn host_id_by_name(tx: &mut DbTx<'_>, name: &str) -> Result<Option<String>, AppError> {
    Ok(tx
        .fetch_optional("SELECT id FROM hosts WHERE name = ?", &[name.into()])
        .await?
        .map(|r| r.text(0)))
}

/// Ids of hosts whose `labels` JSON array contains `label`, inside the
/// caller's transaction.
///
/// PG-port note: `json_each` has no mechanical rewrite (see `store::dialect`),
/// so this statement needs an explicit Postgres variant.
pub async fn host_ids_by_label(tx: &mut DbTx<'_>, label: &str) -> Result<Vec<String>, AppError> {
    Ok(tx
        // `hosts.id` must be qualified: `json_each` also exposes an `id`
        // column, so the unqualified form is ambiguous at compile time.
        .fetch_all(
            "SELECT hosts.id FROM hosts, json_each(hosts.labels) WHERE json_each.value = ?",
            &[label.into()],
        )
        .await?
        .into_iter()
        .map(|r| r.text(0))
        .collect())
}

/// Everything needed to insert one task row.
#[derive(Debug, Default)]
pub struct TaskInsert {
    pub id: String,
    pub pipeline_id: String,
    pub step_order: i64,
    pub name: String,
    pub exec_type: String,
    pub host_id: Option<String>,
    pub host_names: String,
    pub host_labels: String,
    pub command_template: String,
    pub image: String,
    pub work_dir: String,
    pub retry_count: i64,
    pub timeout_seconds: i64,
    pub fail_strategy: String,
    pub step_secrets: String,
    pub step_env: String,
    pub volumes: String,
    pub privileged: i64,
    pub hosts_strategy: String,
    pub capabilities: String,
    pub hosts_max_parallel: Option<i64>,
    pub needs_json: String,
}

/// Insert one task row inside the caller's transaction.
pub async fn insert_task_tx(tx: &mut DbTx<'_>, t: &TaskInsert) -> Result<(), AppError> {
    tx.execute(
        "INSERT INTO tasks (id, pipeline_id, step_order, name, exec_type, host_id, host_names, host_labels,
             command_template, image, work_dir, retry_count, timeout_seconds, fail_strategy, step_secrets,
             step_env, volumes, privileged,
             hosts_strategy, capabilities, hosts_max_parallel, needs_json)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        &[
            t.id.as_str().into(),
            t.pipeline_id.as_str().into(),
            SqlValue::Int(t.step_order),
            t.name.as_str().into(),
            t.exec_type.as_str().into(),
            t.host_id.as_deref().into(),
            t.host_names.as_str().into(),
            t.host_labels.as_str().into(),
            t.command_template.as_str().into(),
            t.image.as_str().into(),
            t.work_dir.as_str().into(),
            SqlValue::Int(t.retry_count),
            SqlValue::Int(t.timeout_seconds),
            t.fail_strategy.as_str().into(),
            t.step_secrets.as_str().into(),
            t.step_env.as_str().into(),
            t.volumes.as_str().into(),
            SqlValue::Int(t.privileged),
            t.hosts_strategy.as_str().into(),
            t.capabilities.as_str().into(),
            t.hosts_max_parallel.into(),
            t.needs_json.as_str().into(),
        ],
    )
    .await?;
    Ok(())
}

/// Insert one inline (non-secret) env var of a pipeline.
pub async fn insert_inline_env_tx(
    tx: &mut DbTx<'_>,
    pipeline_id: &str,
    name: &str,
    value: &str,
) -> Result<(), AppError> {
    tx.execute(
        "INSERT INTO variables (id, pipeline_id, var_name, source_type, is_secret, value)
             VALUES (?, ?, ?, 'inline', 0, ?)",
        &[
            Uuid::new_v4().to_string().into(),
            pipeline_id.into(),
            name.into(),
            value.into(),
        ],
    )
    .await?;
    Ok(())
}

/// Bind one variable id to a pipeline (no-op when already bound).
pub async fn insert_var_binding_tx(
    tx: &mut DbTx<'_>,
    pipeline_id: &str,
    var_id: &str,
) -> Result<(), AppError> {
    tx.execute(
        "INSERT OR IGNORE INTO pipeline_vars (pipeline_id, var_id)
             VALUES (?, ?)",
        &[pipeline_id.into(), var_id.into()],
    )
    .await?;
    Ok(())
}

/// Bind one secret id to a pipeline (no-op when already bound).
pub async fn insert_secret_binding_tx(
    tx: &mut DbTx<'_>,
    pipeline_id: &str,
    secret_id: &str,
) -> Result<(), AppError> {
    tx.execute(
        "INSERT OR IGNORE INTO pipeline_secrets (pipeline_id, secret_id)
             VALUES (?, ?)",
        &[pipeline_id.into(), secret_id.into()],
    )
    .await?;
    Ok(())
}

/// Name of one team secret (the YAML emit path resolves ids to names).
pub async fn secret_name_in_team(
    pool: &DbPool,
    secret_id: &str,
    team_id: &str,
) -> Result<Option<String>, AppError> {
    Ok(exec::fetch_optional(
        pool,
        "SELECT name FROM secrets WHERE id = ? AND team_id = ?",
        &[secret_id.into(), team_id.into()],
    )
    .await?
    .map(|r| r.text(0)))
}

/// One pipeline-card list row (latest job + run totals).
pub struct PipelineListRow {
    pub id: String,
    pub name: String,
    pub description: String,
    pub trigger_type: String,
    pub runner_labels: String,
    pub timeout_seconds: i64,
    pub concurrency_group: Option<String>,
    pub input_defs: String,
    pub is_active: i64,
    pub source: String,
    pub team_id: String,
    pub owner_group: String,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    pub latest_job_id: Option<String>,
    pub latest_job_status: Option<String>,
    pub latest_job_start: Option<String>,
    pub job_total: i64,
    pub job_success: i64,
    pub concurrency_mode: Option<String>,
    pub concurrency_max_queue: Option<i64>,
    pub concurrency_queue_timeout_seconds: Option<i64>,
    pub yaml_path: Option<String>,
    pub environments: String,
}

fn list_row_of(r: exec::GenericRow) -> PipelineListRow {
    PipelineListRow {
        id: r.text(0),
        name: r.text(1),
        description: r.text(2),
        trigger_type: r.text(3),
        runner_labels: r.text(4),
        timeout_seconds: r.i64(5),
        concurrency_group: r.opt_text(6),
        input_defs: r.text(7),
        is_active: r.i64(8),
        source: r.text(9),
        team_id: r.text(10),
        owner_group: r.text(11),
        created_at: r.opt_text(12),
        updated_at: r.opt_text(13),
        latest_job_id: r.opt_text(14),
        latest_job_status: r.opt_text(15),
        latest_job_start: r.opt_text(16),
        job_total: r.i64(17),
        job_success: r.i64(18),
        concurrency_mode: r.opt_text(19),
        concurrency_max_queue: r.opt_i64(20),
        concurrency_queue_timeout_seconds: r.opt_i64(21),
        yaml_path: r.opt_text(22),
        environments: r.text(23),
    }
}

/// The pipeline-card list: one row per pipeline with its latest job and run
/// totals, newest-first. `visible_team_ids = None` is unrestricted (platform
/// admin); `Some([])` yields nothing; `Some(ids)` filters to those teams.
/// `include_debug = false` hides debug (unredacted) jobs from latest-job and
/// run-total subqueries — they are admin-only records.
pub async fn list_rows(
    pool: &DbPool,
    visible_team_ids: Option<&[String]>,
    include_debug: bool,
) -> Result<Vec<PipelineListRow>, AppError> {
    let Some(ids) = visible_team_ids else {
        let debug_scope = if include_debug {
            ""
        } else {
            "WHERE debug_unredact = 0 "
        };
        let sql = format!(
        "SELECT p.id, p.name, p.description, p.trigger_type, p.runner_labels,
                p.timeout_seconds, p.concurrency_group, p.input_defs, p.is_active,
                p.source, p.team_id, p.owner_group, p.created_at, p.updated_at,
                j.id as latest_job_id, j.status as latest_job_status,
                j.start_time as latest_job_start,
                COALESCE(js.job_total, 0) as job_total,
                COALESCE(js.job_success, 0) as job_success,
                p.concurrency_mode, p.concurrency_max_queue, p.concurrency_queue_timeout_seconds,
                p.yaml_path, p.environments
         FROM pipelines p
         LEFT JOIN (
             SELECT pipeline_id, MAX(created_at) as max_created
             FROM jobs {debug_scope}GROUP BY pipeline_id
         ) latest ON p.id = latest.pipeline_id
         LEFT JOIN jobs j ON j.pipeline_id = latest.pipeline_id AND j.created_at = latest.max_created
         LEFT JOIN (
             SELECT pipeline_id,
                    COUNT(*) as job_total,
                    SUM(CASE WHEN status = 'success' THEN 1 ELSE 0 END) as job_success
             FROM jobs {debug_scope}GROUP BY pipeline_id
         ) js ON p.id = js.pipeline_id
         WHERE 1=1"
        );
        return Ok(
            exec::fetch_all(pool, &format!("{sql} ORDER BY p.created_at DESC"), &[])
                .await?
                .into_iter()
                .map(list_row_of)
                .collect(),
        );
    };
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let debug_scope = if include_debug {
        ""
    } else {
        "WHERE debug_unredact = 0 "
    };
    let base = format!(
        "SELECT p.id, p.name, p.description, p.trigger_type, p.runner_labels,
                p.timeout_seconds, p.concurrency_group, p.input_defs, p.is_active,
                p.source, p.team_id, p.owner_group, p.created_at, p.updated_at,
                j.id as latest_job_id, j.status as latest_job_status,
                j.start_time as latest_job_start,
                COALESCE(js.job_total, 0) as job_total,
                COALESCE(js.job_success, 0) as job_success,
                p.concurrency_mode, p.concurrency_max_queue, p.concurrency_queue_timeout_seconds,
                p.yaml_path, p.environments
         FROM pipelines p
         LEFT JOIN (
             SELECT pipeline_id, MAX(created_at) as max_created
             FROM jobs {debug_scope}GROUP BY pipeline_id
         ) latest ON p.id = latest.pipeline_id
         LEFT JOIN jobs j ON j.pipeline_id = latest.pipeline_id AND j.created_at = latest.max_created
         LEFT JOIN (
             SELECT pipeline_id,
                    COUNT(*) as job_total,
                    SUM(CASE WHEN status = 'success' THEN 1 ELSE 0 END) as job_success
             FROM jobs {debug_scope}GROUP BY pipeline_id
         ) js ON p.id = js.pipeline_id
         WHERE 1=1"
    );
    let placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let sql = format!("{base} AND p.team_id IN ({placeholders}) ORDER BY p.created_at DESC");
    let binds: Vec<SqlValue> = ids.iter().map(|id| id.as_str().into()).collect();
    Ok(exec::fetch_all(pool, &sql, &binds)
        .await?
        .into_iter()
        .map(list_row_of)
        .collect())
}

/// Result of a pipeline disable: the display name and the ids cancelled in the
/// same transaction.
pub struct DisableOutcome {
    pub name: String,
    pub cancelled: Vec<String>,
}

/// Idempotent emergency stop: flips `is_active` off and, when asked, cancels
/// every in-flight job — all under one `BEGIN IMMEDIATE`, with the audit entry.
///
/// Why one lock scope: the flip must be serialised with any concurrent trigger
/// that re-checks `is_active` inside its own insert transaction, so a trigger
/// cannot slip a job past the cancellation scan after the commit. The scan
/// runs after the flip (same lock), and audit-inside keeps an unauditable
/// disable from ever committing.
///
/// Post-commit fan-out (poller wake, per-job WebSocket announce) is the
/// caller's — nothing here notifies outside the transaction.
pub async fn disable(
    pool: &DbPool,
    auth: &crate::middleware::AuthUser,
    team_id: &str,
    pipeline_id: &str,
    cancel_in_flight: bool,
) -> Result<DisableOutcome, AppError> {
    let mut tx = pool
        .begin_immediate()
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;
    let (name, was_active) = name_active_tx(&mut tx, pipeline_id)
        .await?
        .ok_or_else(|| AppError::NotFound("pipeline not found".into()))?;

    if was_active {
        set_active_tx(&mut tx, pipeline_id, false).await?;
    }

    let mut cancelled: Vec<String> = Vec::new();
    if cancel_in_flight {
        for job_id in in_flight_job_ids_tx(&mut tx, pipeline_id).await? {
            if let Some(info) =
                crate::store::jobs::cancel_job_in_tx(&mut tx, &job_id, &auth.id, &auth.username)
                    .await?
            {
                tracing::info!(
                    operator = %auth.username,
                    job_id = %job_id,
                    pipeline = %name,
                    prev_status = %info.old_status,
                    "Job cancelled by pipeline disable"
                );
                cancelled.push(job_id);
            }
        }
    }

    crate::audit::record_in_tx(
        &mut tx,
        crate::audit::AuditEvent::from_auth(auth, crate::audit::Action::PipelineDisable)
            .resource("pipeline", pipeline_id, &name)
            .team(Some(team_id.to_string()))
            .before(Some(serde_json::json!({ "is_active": was_active })))
            .after(Some(serde_json::json!({ "is_active": false })))
            .meta(Some(serde_json::json!({
                "cancel_in_flight": cancel_in_flight,
                "cancelled_count": cancelled.len(),
                "cancelled_jobs": cancelled.iter().map(String::as_str).collect::<Vec<_>>(),
            }))),
    )
    .await
    .map_err(|e| AppError::Internal(e.to_string()))?;

    tx.commit()
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;
    Ok(DisableOutcome { name, cancelled })
}

/// Idempotent re-enable under one `BEGIN IMMEDIATE`; the audit entry is inside
/// the transaction (an unauditable enable is a failed enable). No-op when
/// already active. Returns the pipeline name.
pub async fn enable(
    pool: &DbPool,
    auth: &crate::middleware::AuthUser,
    team_id: &str,
    pipeline_id: &str,
) -> Result<String, AppError> {
    let mut tx = pool
        .begin_immediate()
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;
    let (name, was_active) = name_active_tx(&mut tx, pipeline_id)
        .await?
        .ok_or_else(|| AppError::NotFound("pipeline not found".into()))?;
    if !was_active {
        set_active_tx(&mut tx, pipeline_id, true).await?;
    }
    crate::audit::record_in_tx(
        &mut tx,
        crate::audit::AuditEvent::from_auth(auth, crate::audit::Action::PipelineEnable)
            .resource("pipeline", pipeline_id, &name)
            .team(Some(team_id.to_string()))
            .before(Some(serde_json::json!({ "is_active": was_active })))
            .after(Some(serde_json::json!({ "is_active": true }))),
    )
    .await
    .map_err(|e| AppError::Internal(e.to_string()))?;
    tx.commit()
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;
    Ok(name)
}

/// The structured update: one `BEGIN IMMEDIATE` that re-checks the team-move
/// guard under the write lock and rewrites every editable field. A refused
/// team move rolls back (nothing is written). Returns `rows_affected` so the
/// caller can 404 a vanished row.
///
/// Post-commit work (the definition YAML rewrite, the audit record) stays with
/// the handler — see `transaction-boundaries.md`.
pub async fn update_guarded(
    pool: &DbPool,
    id: &str,
    team_changed: bool,
    p: &UpdatePipeline,
) -> Result<u64, AppError> {
    let mut tx = pool
        .begin_immediate()
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;
    if team_changed {
        let active_jobs = active_job_count_tx(&mut tx, id).await?;
        if active_jobs > 0 {
            tx.rollback().await.ok();
            return Err(AppError::Conflict(format!(
                "pipeline team cannot change while {active_jobs} active job(s) exist; finish or cancel them first"
            )));
        }
    }
    let affected = update_tx(&mut tx, id, p).await?;
    tx.commit()
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;
    Ok(affected)
}

/// One task row still carrying a retired column value (deprecated-key scan).
pub struct DeprecatedTaskColumns {
    pub pipeline_name: String,
    pub step_name: String,
    pub registry_secret: String,
    pub config_source: String,
    pub config_key_path: String,
}

/// Tasks whose retired columns still hold data — the migration report source.
pub async fn deprecated_task_columns(
    pool: &DbPool,
) -> Result<Vec<DeprecatedTaskColumns>, AppError> {
    Ok(exec::fetch_all(
        pool,
        "SELECT p.name AS pipeline_name, t.name AS step_name,
                t.registry_secret, t.config_source, t.config_key_path
         FROM tasks t
         JOIN pipelines p ON p.id = t.pipeline_id
         WHERE t.registry_secret != '' OR t.config_source != '' OR t.config_key_path != ''
         ORDER BY p.name, t.step_order",
        &[],
    )
    .await?
    .into_iter()
    .map(|r| DeprecatedTaskColumns {
        pipeline_name: r.text_of("pipeline_name"),
        step_name: r.text_of("step_name"),
        registry_secret: r.text_of("registry_secret"),
        config_source: r.text_of("config_source"),
        config_key_path: r.text_of("config_key_path"),
    })
    .collect())
}

/// One legacy pipeline row read for the definition backfill
/// (`yaml_source = ''`).
pub struct BackfillRow {
    pub id: String,
    pub name: String,
    pub source: String,
    pub yaml_path: Option<String>,
    pub description: String,
    pub trigger_type: String,
    pub webhook_path: Option<String>,
    pub webhook_secret: Option<String>,
    pub webhook_environment: Option<String>,
    pub cron_expr: Option<String>,
    pub cron_environment: Option<String>,
    pub runner_labels: String,
    pub timeout_seconds: i64,
    pub concurrency_group: Option<String>,
    pub concurrency_mode: String,
    pub concurrency_max_queue: i64,
    pub concurrency_queue_timeout_seconds: i64,
    pub owner_group: String,
    pub environments: String,
    pub notify_json: String,
    pub input_defs: String,
}

/// Rows still missing their stored definition, creation-ordered.
pub async fn definition_backfill_rows(pool: &DbPool) -> Result<Vec<BackfillRow>, AppError> {
    Ok(exec::fetch_all(
        pool,
        "SELECT id, name, source, yaml_path, description, trigger_type, webhook_path,
                webhook_secret, webhook_environment, cron_expr, cron_environment,
                runner_labels, timeout_seconds, concurrency_group, concurrency_mode,
                concurrency_max_queue, concurrency_queue_timeout_seconds, owner_group,
                environments, notify_json, input_defs
         FROM pipelines WHERE yaml_source = '' ORDER BY created_at ASC",
        &[],
    )
    .await?
    .into_iter()
    .map(|r| BackfillRow {
        id: r.text(0),
        name: r.text(1),
        source: r.text(2),
        yaml_path: r.opt_text(3),
        description: r.text(4),
        trigger_type: r.text(5),
        webhook_path: r.opt_text(6),
        webhook_secret: r.opt_text(7),
        webhook_environment: r.opt_text(8),
        cron_expr: r.opt_text(9),
        cron_environment: r.opt_text(10),
        runner_labels: r.text(11),
        timeout_seconds: r.i64(12),
        concurrency_group: r.opt_text(13),
        concurrency_mode: r.text(14),
        concurrency_max_queue: r.i64(15),
        concurrency_queue_timeout_seconds: r.i64(16),
        owner_group: r.text(17),
        environments: r.text(18),
        notify_json: r.text(19),
        input_defs: r.text(20),
    })
    .collect())
}

/// One task row read for the backfill's YAML rendering.
pub struct BackfillTask {
    pub step_order: i64,
    pub name: String,
    pub exec_type: String,
    pub command_template: String,
    pub image: String,
    pub work_dir: String,
    pub timeout_seconds: i64,
    pub retry_count: i64,
    pub fail_strategy: String,
    pub step_secrets: String,
    pub step_env_json: String,
    pub volumes: String,
    pub privileged: i64,
    pub hosts_strategy: String,
    pub hosts_max_parallel: i64,
    pub needs_json: String,
    pub host_names: String,
    pub host_labels: String,
}

/// The relational parts the backfill renders back into YAML.
pub struct BackfillSourceParts {
    pub env_pairs: Vec<(String, String)>,
    pub secret_names: Vec<String>,
    pub var_names: Vec<String>,
    pub resolutions: Vec<(i64, String, String)>,
    pub tasks: Vec<BackfillTask>,
}

/// Read one legacy pipeline's relational projection parts.
pub async fn backfill_source_parts(
    pool: &DbPool,
    pipeline_id: &str,
) -> Result<BackfillSourceParts, AppError> {
    let env_pairs = exec::fetch_all(
        pool,
        "SELECT var_name, value FROM variables
         WHERE pipeline_id = ? AND source_type = 'inline' AND is_secret = 0
         ORDER BY var_name",
        &[pipeline_id.into()],
    )
    .await?
    .into_iter()
    .map(|r| (r.text(0), r.text(1)))
    .collect();

    let secret_names = exec::fetch_all(
        pool,
        "SELECT s.name FROM pipeline_secrets ps
         JOIN secrets s ON s.id = ps.secret_id
         WHERE ps.pipeline_id = ? ORDER BY s.name",
        &[pipeline_id.into()],
    )
    .await?
    .into_iter()
    .map(|r| r.text(0))
    .collect();

    let var_names = exec::fetch_all(
        pool,
        "SELECT pv.name FROM pipeline_vars pvars
         JOIN platform_vars pv ON pv.id = pvars.var_id
         WHERE pvars.pipeline_id = ? ORDER BY pv.name",
        &[pipeline_id.into()],
    )
    .await?
    .into_iter()
    .map(|r| r.text(0))
    .collect();

    let resolutions = exec::fetch_all(
        pool,
        "SELECT step_order, uses_ref, resolved_version FROM step_resolutions
         WHERE pipeline_id = ? ORDER BY step_order",
        &[pipeline_id.into()],
    )
    .await?
    .into_iter()
    .map(|r| (r.i64(0), r.text(1), r.text(2)))
    .collect();

    let tasks = exec::fetch_all(
        pool,
        "SELECT step_order, name, exec_type, COALESCE(command_template, ''), COALESCE(image, ''),
                COALESCE(work_dir, ''), COALESCE(timeout_seconds, 0), COALESCE(retry_count, 0),
                COALESCE(fail_strategy, 'stop'), COALESCE(step_secrets, ''),
                COALESCE(step_env, '{}'), COALESCE(volumes, '[]'), COALESCE(privileged, 0),
                COALESCE(hosts_strategy, 'chain'), COALESCE(hosts_max_parallel, 0),
                COALESCE(needs_json, '[]'), COALESCE(host_names, ''), COALESCE(host_labels, '')
         FROM tasks WHERE pipeline_id = ? ORDER BY step_order",
        &[pipeline_id.into()],
    )
    .await?
    .into_iter()
    .map(|r| BackfillTask {
        step_order: r.i64(0),
        name: r.text(1),
        exec_type: r.text(2),
        command_template: r.text(3),
        image: r.text(4),
        work_dir: r.text(5),
        timeout_seconds: r.i64(6),
        retry_count: r.i64(7),
        fail_strategy: r.text(8),
        step_secrets: r.text(9),
        step_env_json: r.text(10),
        volumes: r.text(11),
        privileged: r.i64(12),
        hosts_strategy: r.text(13),
        hosts_max_parallel: r.i64(14),
        needs_json: r.text(15),
        host_names: r.text(16),
        host_labels: r.text(17),
    })
    .collect();

    Ok(BackfillSourceParts {
        env_pairs,
        secret_names,
        var_names,
        resolutions,
        tasks,
    })
}

/// `(step_order, input_defs_json)` of a pipeline's marketplace resolutions —
/// the claim payload's per-step inputs when the job has no frozen tasks.
pub async fn step_resolution_defs(
    pool: &DbPool,
    pipeline_id: &str,
) -> Result<Vec<(i64, String)>, AppError> {
    Ok(exec::fetch_all(
        pool,
        "SELECT step_order, input_defs_json FROM step_resolutions WHERE pipeline_id = ?",
        &[pipeline_id.into()],
    )
    .await?
    .into_iter()
    .map(|r| (r.i64(0), r.text(1)))
    .collect())
}

// ─────────────────────── YAML definition projection ───────────────────────

/// One step-resolution row to persist (the resolver's output, store-neutral).
pub struct StepResolutionWrite {
    pub step_order: i64,
    pub uses_ref: String,
    pub resolved_version: String,
    pub content_sha256: String,
    pub source: String,
    pub expanded_json: String,
    pub input_defs_json: String,
    pub reviewed: bool,
}

/// Replace a pipeline's provenance rows inside the caller's transaction.
pub async fn replace_step_resolutions_tx(
    tx: &mut DbTx<'_>,
    pipeline_id: &str,
    resolutions: &[StepResolutionWrite],
) -> Result<(), AppError> {
    tx.execute(
        "DELETE FROM step_resolutions WHERE pipeline_id = ?",
        &[pipeline_id.into()],
    )
    .await?;
    for resolution in resolutions {
        tx.execute(
            "INSERT INTO step_resolutions
             (id, pipeline_id, step_order, uses_ref, resolved_version, content_sha256, source, expanded_json, input_defs_json, reviewed)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            &[
                Uuid::new_v4().to_string().into(),
                pipeline_id.into(),
                SqlValue::Int(resolution.step_order),
                resolution.uses_ref.as_str().into(),
                resolution.resolved_version.as_str().into(),
                resolution.content_sha256.as_str().into(),
                resolution.source.as_str().into(),
                resolution.expanded_json.as_str().into(),
                resolution.input_defs_json.as_str().into(),
                SqlValue::Int(i64::from(resolution.reviewed)),
            ],
        )
        .await?;
    }
    Ok(())
}

/// Drop the expandable projection (tasks, resolutions, inline vars, bindings)
/// before a YAML save re-projects it. One named operation so the five child
/// tables cannot drift apart.
pub async fn clear_projection_tx(tx: &mut DbTx<'_>, pipeline_id: &str) -> Result<(), AppError> {
    for sql in [
        "DELETE FROM step_resolutions WHERE pipeline_id = ?",
        "DELETE FROM tasks WHERE pipeline_id = ?",
        "DELETE FROM variables WHERE pipeline_id = ?",
        "DELETE FROM pipeline_secrets WHERE pipeline_id = ?",
        "DELETE FROM pipeline_vars WHERE pipeline_id = ?",
    ] {
        tx.execute(sql, &[pipeline_id.into()]).await?;
    }
    Ok(())
}

/// Everything the YAML upsert writes on an existing pipeline row.
pub struct YamlPipelineUpdate<'a> {
    pub name: &'a str,
    pub description: &'a str,
    pub trigger_type: &'a str,
    pub webhook_path: Option<&'a str>,
    pub webhook_secret: &'a str,
    pub webhook_environment: &'a str,
    pub cron_expr: Option<&'a str>,
    pub cron_environment: &'a str,
    pub runner_labels: &'a str,
    pub timeout_seconds: i64,
    pub resource_profile: &'a str,
    pub concurrency_group: Option<&'a str>,
    pub concurrency_mode: &'a str,
    pub concurrency_max_queue: i64,
    pub concurrency_queue_timeout_seconds: i64,
    pub owner_group: &'a str,
    pub team_id: &'a str,
    pub input_defs: &'a str,
    pub environments: &'a str,
    pub notify_json: &'a str,
    pub yaml_path: &'a str,
    pub yaml_sha256: &'a str,
    pub yaml_source: &'a str,
}

/// Re-project one YAML definition onto its existing pipeline row.
pub async fn yaml_update_tx(
    tx: &mut DbTx<'_>,
    id: &str,
    p: &YamlPipelineUpdate<'_>,
) -> Result<(), AppError> {
    tx.execute(
        "UPDATE pipelines SET name = ?, description = ?, trigger_type = ?,
         webhook_path = ?, webhook_secret = ?, webhook_environment = ?, cron_expr = ?, cron_environment = ?, runner_labels = ?, timeout_seconds = ?,
         resource_profile = ?,
         concurrency_group = ?, concurrency_mode = ?, concurrency_max_queue = ?,
         concurrency_queue_timeout_seconds = ?,
         owner_group = ?, team_id = ?, input_defs = ?, environments = ?,
         notify_json = ?,
         yaml_path = ?, yaml_sha256 = ?, yaml_source = ?, updated_at = datetime('now')
         WHERE id = ?",
        &[
            p.name.into(),
            p.description.into(),
            p.trigger_type.into(),
            p.webhook_path.into(),
            p.webhook_secret.into(),
            p.webhook_environment.into(),
            p.cron_expr.into(),
            p.cron_environment.into(),
            p.runner_labels.into(),
            SqlValue::Int(p.timeout_seconds),
            p.resource_profile.into(),
            p.concurrency_group.into(),
            p.concurrency_mode.into(),
            SqlValue::Int(p.concurrency_max_queue),
            SqlValue::Int(p.concurrency_queue_timeout_seconds),
            p.owner_group.into(),
            p.team_id.into(),
            p.input_defs.into(),
            p.environments.into(),
            p.notify_json.into(),
            p.yaml_path.into(),
            p.yaml_sha256.into(),
            p.yaml_source.into(),
            id.into(),
        ],
    )
    .await?;
    Ok(())
}

/// Everything the YAML create path inserts.
pub struct YamlPipelineInsert<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub description: &'a str,
    pub trigger_type: &'a str,
    pub webhook_path: Option<&'a str>,
    pub webhook_secret: &'a str,
    pub webhook_environment: &'a str,
    pub cron_expr: Option<&'a str>,
    pub cron_environment: &'a str,
    pub runner_labels: &'a str,
    pub timeout_seconds: i64,
    pub resource_profile: &'a str,
    pub concurrency_group: Option<&'a str>,
    pub concurrency_mode: &'a str,
    pub concurrency_max_queue: i64,
    pub concurrency_queue_timeout_seconds: i64,
    pub owner_group: &'a str,
    pub team_id: &'a str,
    pub yaml_path: &'a str,
    pub yaml_sha256: &'a str,
    pub yaml_source: &'a str,
    pub input_defs: &'a str,
    pub environments: &'a str,
    pub notify_json: &'a str,
}

/// Insert one fully-projected YAML pipeline row (`source = 'yaml'`, active).
pub async fn yaml_insert_tx(tx: &mut DbTx<'_>, p: &YamlPipelineInsert<'_>) -> Result<(), AppError> {
    tx.execute(
        "INSERT INTO pipelines (id, name, description, trigger_type, webhook_path,
         webhook_secret, webhook_environment, cron_expr, cron_environment, runner_labels, timeout_seconds, resource_profile, concurrency_group, concurrency_mode,
         concurrency_max_queue, concurrency_queue_timeout_seconds,
        owner_group, team_id, source, yaml_path, yaml_sha256, yaml_source, input_defs, environments, notify_json, is_active)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'yaml', ?, ?, ?, ?, ?, ?, 1)",
        &[
            p.id.into(),
            p.name.into(),
            p.description.into(),
            p.trigger_type.into(),
            p.webhook_path.into(),
            p.webhook_secret.into(),
            p.webhook_environment.into(),
            p.cron_expr.into(),
            p.cron_environment.into(),
            p.runner_labels.into(),
            SqlValue::Int(p.timeout_seconds),
            p.resource_profile.into(),
            p.concurrency_group.into(),
            p.concurrency_mode.into(),
            SqlValue::Int(p.concurrency_max_queue),
            SqlValue::Int(p.concurrency_queue_timeout_seconds),
            p.owner_group.into(),
            p.team_id.into(),
            p.yaml_path.into(),
            p.yaml_sha256.into(),
            p.yaml_source.into(),
            p.input_defs.into(),
            p.environments.into(),
            p.notify_json.into(),
        ],
    )
    .await?;
    Ok(())
}

/// Same-team secret ids for the given names, inside the caller's transaction.
pub async fn secret_ids_by_names_tx(
    tx: &mut DbTx<'_>,
    team_id: &str,
    names: &[String],
) -> Result<Vec<String>, AppError> {
    if names.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = vec!["?"; names.len()].join(",");
    let sql = format!("SELECT id FROM secrets WHERE team_id = ? AND name IN ({placeholders})");
    let mut binds: Vec<SqlValue> = vec![team_id.into()];
    binds.extend(names.iter().map(|n| SqlValue::from(n.as_str())));
    Ok(tx
        .fetch_all(&sql, &binds)
        .await?
        .into_iter()
        .map(|r| r.text(0))
        .collect())
}

/// Platform-variable ids for the given names, inside the caller's transaction.
pub async fn platform_var_ids_by_names_tx(
    tx: &mut DbTx<'_>,
    names: &[String],
) -> Result<Vec<String>, AppError> {
    if names.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = vec!["?"; names.len()].join(",");
    let sql = format!("SELECT id FROM platform_vars WHERE name IN ({placeholders})");
    let binds: Vec<SqlValue> = names.iter().map(|n| SqlValue::from(n.as_str())).collect();
    Ok(tx
        .fetch_all(&sql, &binds)
        .await?
        .into_iter()
        .map(|r| r.text(0))
        .collect())
}

/// Pipeline id by definition basename (`source = 'yaml'`).
pub async fn pipeline_id_by_yaml_path(
    pool: &DbPool,
    yaml_path: &str,
) -> Result<Option<String>, AppError> {
    Ok(exec::fetch_optional(
        pool,
        "SELECT id FROM pipelines WHERE yaml_path = ? AND source = 'yaml'",
        &[yaml_path.into()],
    )
    .await?
    .map(|r| r.text(0)))
}

/// `(id, yaml_path)` of every YAML-sourced pipeline — legacy path matching.
pub async fn legacy_yaml_rows(pool: &DbPool) -> Result<Vec<(String, Option<String>)>, AppError> {
    Ok(exec::fetch_all(
        pool,
        "SELECT id, yaml_path FROM pipelines WHERE source = 'yaml'",
        &[],
    )
    .await?
    .into_iter()
    .map(|r| (r.text(0), r.opt_text(1)))
    .collect())
}

/// Rewrite one pipeline's origin basename (legacy path migration).
pub async fn set_yaml_path(pool: &DbPool, id: &str, yaml_path: &str) -> Result<(), AppError> {
    exec::execute(
        pool,
        "UPDATE pipelines SET yaml_path = ? WHERE id = ?",
        &[yaml_path.into(), id.into()],
    )
    .await?;
    Ok(())
}

/// `cron_last_fired_at` inside the caller's transaction (`None` = unset).
pub async fn cron_last_fired_tx(tx: &mut DbTx<'_>, id: &str) -> Result<Option<String>, AppError> {
    Ok(tx
        .fetch_optional(
            "SELECT cron_last_fired_at FROM pipelines WHERE id = ?",
            &[id.into()],
        )
        .await?
        .map(|r| r.text(0)))
}

/// Stamp `cron_last_fired_at` inside the caller's transaction.
pub async fn stamp_cron_fired_tx(tx: &mut DbTx<'_>, id: &str) -> Result<(), AppError> {
    tx.execute(
        "UPDATE pipelines SET cron_last_fired_at = datetime('now') WHERE id = ?",
        &[id.into()],
    )
    .await?;
    Ok(())
}

/// Active cron pipelines: `(id, name, cron_expr, cron_environment)`.
pub async fn active_cron_pipelines(
    pool: &DbPool,
) -> Result<Vec<(String, String, Option<String>, String)>, AppError> {
    Ok(exec::fetch_all(
        pool,
        "SELECT p.id, p.name, p.cron_expr, p.cron_environment
         FROM pipelines p
         WHERE p.trigger_type = 'cron'
           AND p.is_active = 1
           AND p.cron_expr IS NOT NULL
           AND p.cron_expr != ''",
        &[],
    )
    .await?
    .into_iter()
    .map(|r| (r.text(0), r.text(1), r.opt_text(2), r.text(3)))
    .collect())
}

/// `(id, name, yaml_path, yaml_source)` of YAML-sourced pipelines, ordered by
/// their origin basename — the definition export source.
pub async fn yaml_definition_rows(
    pool: &DbPool,
) -> Result<Vec<(String, String, Option<String>, String)>, AppError> {
    Ok(exec::fetch_all(
        pool,
        "SELECT id, name, yaml_path, yaml_source FROM pipelines
         WHERE source = 'yaml' ORDER BY yaml_path",
        &[],
    )
    .await?
    .into_iter()
    .map(|r| (r.text(0), r.text(1), r.opt_text(2), r.text(3)))
    .collect())
}

/// Stored canonical YAML of one pipeline (`None` = no such row).
pub async fn yaml_source_of(pool: &DbPool, id: &str) -> Result<Option<String>, AppError> {
    Ok(exec::fetch_optional(
        pool,
        "SELECT yaml_source FROM pipelines WHERE id = ?",
        &[id.into()],
    )
    .await?
    .map(|r| r.text(0)))
}

/// Stored canonical YAML of one pipeline inside the caller's transaction.
pub async fn yaml_source_tx(tx: &mut DbTx<'_>, id: &str) -> Result<Option<String>, AppError> {
    Ok(tx
        .fetch_optional(
            "SELECT yaml_source FROM pipelines WHERE id = ?",
            &[id.into()],
        )
        .await?
        .map(|r| r.text(0)))
}

/// One YAML-sourced pipeline row for the file list.
pub struct YamlFileRow {
    pub yaml_path: String,
    pub id: String,
    pub name: String,
    pub team_id: String,
    pub is_active: bool,
}

/// YAML-sourced pipelines with an origin basename, ordered by it.
pub async fn yaml_file_rows(pool: &DbPool) -> Result<Vec<YamlFileRow>, AppError> {
    Ok(exec::fetch_all(
        pool,
        "SELECT yaml_path, id, name, team_id, is_active FROM pipelines \
         WHERE source = 'yaml' AND yaml_path NOT IN ('', 'form') ORDER BY yaml_path",
        &[],
    )
    .await?
    .into_iter()
    .map(|r| YamlFileRow {
        yaml_path: r.text(0),
        id: r.text(1),
        name: r.text(2),
        team_id: r.text(3),
        is_active: r.i64(4) != 0,
    })
    .collect())
}

/// `(yaml_path, yaml_source, updated_at)` — the definition sync status.
pub async fn yaml_sync_row(
    pool: &DbPool,
    id: &str,
) -> Result<Option<(Option<String>, String, Option<String>)>, AppError> {
    Ok(exec::fetch_optional(
        pool,
        "SELECT yaml_path, yaml_source, updated_at FROM pipelines WHERE id = ?",
        &[id.into()],
    )
    .await?
    .map(|r| (r.opt_text(0), r.text(1), r.opt_text(2))))
}

/// `(id, source, yaml_path)` of one pipeline — the write path's ownership check.
pub async fn yaml_row_identity(
    pool: &DbPool,
    id: &str,
) -> Result<Option<(String, String, Option<String>)>, AppError> {
    Ok(exec::fetch_optional(
        pool,
        "SELECT id, source, yaml_path FROM pipelines WHERE id = ?",
        &[id.into()],
    )
    .await?
    .map(|r| (r.text(0), r.text(1), r.opt_text(2))))
}

/// Retarget an existing pipeline row at a YAML definition (the API create
/// path binds a Form/API row to its stored definition).
pub async fn bind_pipeline_to_yaml(
    pool: &DbPool,
    id: &str,
    yaml_path: &str,
) -> Result<(), AppError> {
    exec::execute(
        pool,
        "UPDATE pipelines SET source = 'yaml', yaml_path = ?, updated_at = datetime('now') WHERE id = ?",
        &[yaml_path.into(), id.into()],
    )
    .await?;
    Ok(())
}

/// Restore a pipeline row's pre-bind `(source, yaml_path)` after a failed bind.
pub async fn restore_pipeline_binding(
    pool: &DbPool,
    id: &str,
    source: &str,
    yaml_path: Option<&str>,
) -> Result<(), AppError> {
    exec::execute(
        pool,
        "UPDATE pipelines SET source = ?, yaml_path = ?, updated_at = datetime('now') WHERE id = ?",
        &[source.into(), yaml_path.into(), id.into()],
    )
    .await?;
    Ok(())
}
