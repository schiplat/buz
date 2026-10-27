use axum::{
    extract::{Path, State},
    Json,
};
use serde::Deserialize;
use shared::error::AppError;
use shared::protocol::response::ApiResponse;
use shared::protocol::{
    masking_input_names, redact_input_map, validate_input_values, CreatePipelineRequest,
    PipelineInputDef, StepDef,
};
use shared::rbac::Perm;
use sqlx::Row;
use uuid::Uuid;

use crate::audit::{self, Action, AuditEvent};
use crate::crypto;
use crate::middleware::AuthUser;
use crate::rbac::{pipeline_team_id, resolve_required_team_id, secret_team_id, team_in_clause};
use crate::state::AppState;

/// Success jobs ÷ total jobs for a pipeline. `None` when there are no builds yet.
pub(crate) fn job_success_rate(success_count: i64, total_count: i64) -> Option<f64> {
    if total_count <= 0 {
        None
    } else {
        Some(success_count as f64 / total_count as f64)
    }
}

pub async fn list(
    auth: AuthUser,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<Vec<serde_json::Value>>>, AppError> {
    // Debug (unredacted) jobs are admin-only end to end: non-admins must not
    // even see them as "latest job" or in the run totals of a pipeline card.
    let debug_scope = if auth.is_platform_admin {
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

    let team_filter = team_in_clause(&auth, "p.team_id");
    let sql = match &team_filter {
        None => format!("{base} ORDER BY p.created_at DESC"),
        Some((clause, _)) if clause == "AND 1=0" => {
            return Ok(Json(ApiResponse::success(vec![])));
        }
        Some((clause, _)) => format!("{base} {clause} ORDER BY p.created_at DESC"),
    };

    let mut q = sqlx::query(&sql);
    if let Some((_, ids)) = &team_filter {
        for id in ids {
            q = q.bind(id);
        }
    }

    let rows = q.fetch_all(&state.pool).await?;

    let pipelines: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|r| {
            let labels: String = r.get(4);
            let input_defs_raw: String = r.get(7);
            let input_defs = serde_json::from_str::<serde_json::Value>(&input_defs_raw).ok();
            let job_total: i64 = r.get(17);
            let job_success: i64 = r.get(18);
            serde_json::json!({
                "id": r.get::<String, _>(0),
                "name": r.get::<String, _>(1),
                "description": r.get::<String, _>(2),
                "trigger_type": r.get::<String, _>(3),
                "runner_labels": serde_json::from_str::<Vec<String>>(&labels).unwrap_or_default(),
                "timeout_seconds": r.get::<i64, _>(5),
                "concurrency_group": r.get::<Option<String>, _>(6),
                "input_defs": input_defs,
                "is_active": r.get::<i64, _>(8) != 0,
                "source": r.get::<String, _>(9),
                "team_id": r.get::<String, _>(10),
                "owner_group": r.get::<String, _>(11),
                "created_at": r.get::<Option<String>, _>(12),
                "updated_at": r.get::<Option<String>, _>(13),
                "latest_job_id": r.get::<Option<String>, _>(14),
                "latest_job_status": r.get::<Option<String>, _>(15),
                "latest_job_start": r.get::<Option<String>, _>(16),
                "job_total": job_total,
                "job_success": job_success,
                "success_rate": job_success_rate(job_success, job_total),
                "concurrency_mode": r.get::<Option<String>, _>(19),
                "concurrency_max_queue": r.get::<Option<i64>, _>(20),
                "concurrency_queue_timeout_seconds": r.get::<Option<i64>, _>(21),
                "yaml_path": r.get::<Option<String>, _>(22),
                "environments": serde_json::from_str::<Vec<String>>(&r.get::<String, _>(23)).unwrap_or_default(),
            })
        })
        .collect();

    Ok(Json(ApiResponse::success(pipelines)))
}

pub async fn create(
    auth: AuthUser,
    State(state): State<AppState>,
    Json(req): Json<CreatePipelineRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let (team_id, owner_group) = resolve_required_team_id(
        &state.pool,
        req.team_id.as_deref(),
        req.owner_group.as_deref(),
    )
    .await?;
    auth.check_team(&team_id, Perm::PipelineWrite)?;
    verify_pipeline_secret_bindings(&state.pool, &auth, &team_id, req.secret_ids.as_ref()).await?;
    super::environments::validate_environment_slugs(&state.pool, &req.environments).await?;

    let pipeline_id = Uuid::new_v4().to_string();
    let environments_json =
        serde_json::to_string(&req.environments).unwrap_or_else(|_| "[]".into());
    let labels_json = serde_json::to_string(&req.runner_labels).unwrap_or_else(|_| "[]".into());
    let input_defs_json =
        serde_json::to_string(&req.input_defs.unwrap_or_default()).unwrap_or_else(|_| "[]".into());

    let mut tx = state.pool.begin().await?;
    sqlx::query(
        "INSERT INTO pipelines (id, name, description, trigger_type, webhook_path, cron_expr,
         runner_labels, timeout_seconds, concurrency_group, concurrency_mode,
         concurrency_max_queue, concurrency_queue_timeout_seconds,
         owner_group, team_id, input_defs, environments, is_active)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 1)",
    )
    .bind(&pipeline_id)
    .bind(&req.name)
    .bind(req.description.as_deref().unwrap_or(""))
    .bind(&req.trigger_type)
    .bind(&req.webhook_path)
    .bind(&req.cron_expr)
    .bind(&labels_json)
    .bind(req.timeout_seconds.unwrap_or(3600))
    .bind(&req.concurrency_group)
    .bind(req.concurrency_mode.as_deref().unwrap_or(""))
    .bind(req.concurrency_max_queue.unwrap_or(3))
    .bind(req.concurrency_queue_timeout_seconds.unwrap_or(1800))
    .bind(&owner_group)
    .bind(&team_id)
    .bind(&input_defs_json)
    .bind(&environments_json)
    .execute(&mut *tx)
    .await?;

    // Steps / env / bindings are inserted after the pipeline row. If any of
    // them fails (e.g. a `uses:` step that cannot resolve), clean up so a
    // failed save never leaves a half-created pipeline behind.
    let create_result = async {
        let http = crate::services::plugin_pool::http_client(
            crate::services::platform_settings::effective(
                &state.pool,
                crate::services::platform_settings::KEY_PLUGIN_SYNC_TIMEOUT,
                &state.config.plugin_index.sync_timeout_seconds.to_string(),
            )
            .await
            .parse()
            .unwrap_or(crate::services::platform_settings::DEFAULT_STEP_SYNC_TIMEOUT_SECS),
        );
        let resolved = crate::services::plugin_resolver::resolve_uses_steps_async(
            req.steps,
            crate::services::plugin_resolver::effective_local_dir(
                &state.config.storage.data_dir,
                &state.config.plugin_index.local_dir,
            )
            .map_err(|e| AppError::Internal(e.to_string()))?,
            &state.pool,
            &http,
        )
        .await
        .map_err(AppError::ValidationFailed)?;
        let steps = insert_steps(&mut *tx, &pipeline_id, &resolved.steps).await?;
        crate::services::plugin_resolver::replace_step_resolutions(
            &mut *tx,
            &pipeline_id,
            &resolved.resolutions,
        )
        .await
        .map_err(AppError::Internal)?;

        // Insert env variables
        if let Some(envs) = &req.env {
            insert_env_vars(&mut *tx, &pipeline_id, envs).await?;
        }

        // Insert explicit secret bindings
        if let Some(secret_ids) = &req.secret_ids {
            insert_pipeline_secret_bindings(&mut *tx, &pipeline_id, secret_ids).await?;
        }

        // Insert explicit variable bindings
        if let Some(var_ids) = &req.var_ids {
            insert_pipeline_var_bindings(&mut *tx, &pipeline_id, var_ids).await?;
        }
        Ok(steps)
    }
    .await;

    let steps = match create_result {
        Ok(steps) => {
            tx.commit().await?;
            steps
        }
        Err(e) => {
            tx.rollback().await?;
            return Err(e);
        }
    };

    tracing::info!("Pipeline created: {} ({pipeline_id})", req.name);

    audit::record(
        &state.pool,
        AuditEvent::from_auth(&auth, Action::PipelineCreate)
            .resource("pipeline", &pipeline_id, &req.name)
            .team(Some(team_id.clone()))
            .after(Some(serde_json::json!({
                "id": pipeline_id,
                "name": req.name,
                "trigger_type": req.trigger_type,
                "team_id": team_id,
                "owner_group": owner_group,
                "step_count": steps.len(),
            }))),
    )
    .await;

    Ok(Json(ApiResponse::success(serde_json::json!({
        "id": pipeline_id,
        "name": req.name,
        "description": req.description,
        "trigger_type": req.trigger_type,
        "runner_labels": req.runner_labels,
        "timeout_seconds": req.timeout_seconds.unwrap_or(3600),
        "concurrency_group": req.concurrency_group,
        "owner_group": owner_group,
        "team_id": team_id,
        "environments": req.environments,
        "steps": steps,
    }))))
}

pub async fn get_one(
    auth: AuthUser,
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let team_id = pipeline_team_id(&state.pool, &id).await?;
    auth.check_team(&team_id, Perm::TeamView)?;

    let r = sqlx::query(
        "SELECT id, name, description, trigger_type, webhook_path, cron_expr,
                runner_labels, timeout_seconds, source, input_defs, concurrency_group, is_active,
                owner_group, team_id, created_at, updated_at,
                concurrency_mode, concurrency_max_queue, concurrency_queue_timeout_seconds,
                yaml_path, environments
         FROM pipelines WHERE id = ?",
    )
    .bind(&id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound("pipeline not found".into()))?;

    let labels: String = r.get(6);
    let input_defs_raw: String = r.get(9);
    let input_defs = serde_json::from_str::<serde_json::Value>(&input_defs_raw).ok();

    // Fetch explicit secret bindings
    let secret_bind_rows =
        sqlx::query("SELECT secret_id FROM pipeline_secrets WHERE pipeline_id = ?")
            .bind(&id)
            .fetch_all(&state.pool)
            .await?;
    let secret_ids: Vec<String> = secret_bind_rows.iter().map(|r| r.get(0)).collect();

    // Fetch explicit variable bindings
    let var_bind_rows = sqlx::query("SELECT var_id FROM pipeline_vars WHERE pipeline_id = ?")
        .bind(&id)
        .fetch_all(&state.pool)
        .await?;
    let var_ids: Vec<String> = var_bind_rows.iter().map(|r| r.get(0)).collect();

    // Fetch inline env vars (pipeline-level `env:` block) for the edit form.
    let env_rows = sqlx::query(
        "SELECT var_name, value FROM variables
         WHERE pipeline_id = ? AND source_type = 'inline' AND is_secret = 0
         ORDER BY var_name",
    )
    .bind(&id)
    .fetch_all(&state.pool)
    .await?;
    let envs: Vec<serde_json::Value> = env_rows
        .iter()
        .map(|e| {
            serde_json::json!({
                "name": e.get::<String, _>(0),
                "value": e.get::<String, _>(1),
            })
        })
        .collect();

    let tasks = sqlx::query(
        "SELECT id, step_order, name, exec_type, host_id, host_names, host_labels, command_template, image,
                work_dir, retry_count, timeout_seconds, fail_strategy, volumes, step_secrets,
                step_env, privileged, registry_secret, config_source, config_key_path, config_render, config_dest,
                hosts_strategy, capabilities
         FROM tasks WHERE pipeline_id = ? ORDER BY step_order",
    )
    .bind(&id)
    .fetch_all(&state.pool)
    .await?
    .into_iter()
    .map(|t| {
        serde_json::json!({
            "id": t.get::<String, _>(0),
            "step_order": t.get::<i64, _>(1),
            "name": t.get::<String, _>(2),
            "exec_type": t.get::<String, _>(3),
            "host_id": t.get::<Option<String>, _>(4),
            "host_names": t.get::<String, _>(5),
            "host_labels": t.get::<String, _>(6),
            "command_template": t.get::<String, _>(7),
            "image": t.get::<String, _>(8),
            "work_dir": t.get::<String, _>(9),
            "retry_count": t.get::<i64, _>(10),
            "timeout_seconds": t.get::<i64, _>(11),
            "fail_strategy": t.get::<String, _>(12),
            "volumes": serde_json::from_str::<serde_json::Value>(
                &t.get::<String, _>(13),
            ).unwrap_or(serde_json::Value::Array(vec![])),
            "step_secrets": t.get::<String, _>(14),
            "step_env": serde_json::from_str::<serde_json::Value>(
                &t.get::<String, _>(15),
            ).ok().filter(|v| v.is_object()),
            "privileged": t.get::<i64, _>(16) != 0,
            "registry_secret": t.get::<String, _>(17),
            "config_source": t.get::<String, _>(18),
            "config_key_path": t.get::<String, _>(19),
            "config_render": serde_json::from_str::<serde_json::Value>(
                &t.get::<String, _>(20),
            ).ok().filter(|v| !v.is_null()),
            "config_dest": t.get::<String, _>(21),
            "hosts_strategy": t.get::<String, _>(22),
            "capabilities": serde_json::from_str::<Vec<String>>(&t.get::<String, _>(23)).unwrap_or_default(),
        })
    })
    .collect::<Vec<_>>();

    Ok(Json(ApiResponse::success(serde_json::json!({
        "id": r.get::<String, _>(0),
        "name": r.get::<String, _>(1),
        "description": r.get::<String, _>(2),
        "trigger_type": r.get::<String, _>(3),
        "webhook_path": r.get::<Option<String>, _>(4),
        "cron_expr": r.get::<Option<String>, _>(5),
        "runner_labels": serde_json::from_str::<Vec<String>>(&labels).unwrap_or_default(),
        "timeout_seconds": r.get::<i64, _>(7),
        "source": r.get::<String, _>(8),
        "input_defs": input_defs,
        "concurrency_group": r.get::<Option<String>, _>(10),
        "is_active": r.get::<i64, _>(11) != 0,
        "owner_group": r.get::<String, _>(12),
        "team_id": r.get::<String, _>(13),
        "secret_ids": secret_ids,
        "var_ids": var_ids,
        "created_at": r.get::<Option<String>, _>(14),
        "updated_at": r.get::<Option<String>, _>(15),
        "concurrency_mode": r.get::<Option<String>, _>(16),
        "concurrency_max_queue": r.get::<Option<i64>, _>(17),
        "concurrency_queue_timeout_seconds": r.get::<Option<i64>, _>(18),
        "yaml_path": r.get::<Option<String>, _>(19),
        "environments": serde_json::from_str::<Vec<String>>(&r.get::<String, _>(20)).unwrap_or_default(),
        "tasks": tasks,
        "env": envs,
        "yaml_sync": crate::services::pipeline_scanner::yaml_sync_status(
            &state.pool,
            &state.config.pipeline.yaml_scan_dir,
            &id,
        )
        .await
        .ok(),
    }))))
}

pub async fn update(
    auth: AuthUser,
    Path(id): Path<String>,
    State(state): State<AppState>,
    Json(req): Json<CreatePipelineRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let current_team_id = pipeline_team_id(&state.pool, &id).await?;
    auth.check_team(&current_team_id, Perm::PipelineWrite)?;

    // YAML-first: file is authority — refuse Form/API mutation that would split DB vs disk.
    let source: String = sqlx::query_scalar("SELECT source FROM pipelines WHERE id = ?")
        .bind(&id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(|| AppError::NotFound("pipeline not found".into()))?;
    if source == "yaml" {
        return Err(AppError::Conflict(
            "pipeline is file-backed (source=yaml) — edit via PUT /pipelines/yaml-files/{filename} or the YAML editor"
                .into(),
        ));
    }

    let before_row = sqlx::query(
        "SELECT name, trigger_type, team_id, owner_group, is_active FROM pipelines WHERE id = ?",
    )
    .bind(&id)
    .fetch_optional(&state.pool)
    .await?;
    let before = before_row.as_ref().map(|r| {
        serde_json::json!({
            "id": id,
            "name": r.get::<String, _>(0),
            "trigger_type": r.get::<String, _>(1),
            "team_id": r.get::<String, _>(2),
            "owner_group": r.get::<String, _>(3),
            "is_active": r.get::<i64, _>(4) != 0,
        })
    });

    let (team_id, owner_group) = if req.team_id.is_some() || req.owner_group.is_some() {
        let resolved = resolve_required_team_id(
            &state.pool,
            req.team_id.as_deref(),
            req.owner_group.as_deref(),
        )
        .await?;
        auth.check_team(&resolved.0, Perm::PipelineWrite)?;
        resolved
    } else {
        let slug: String = sqlx::query_scalar("SELECT owner_group FROM pipelines WHERE id = ?")
            .bind(&id)
            .fetch_one(&state.pool)
            .await?;
        (current_team_id, slug)
    };

    verify_pipeline_secret_bindings(&state.pool, &auth, &team_id, req.secret_ids.as_ref()).await?;
    super::environments::validate_environment_slugs(&state.pool, &req.environments).await?;

    let stored_team_id: String = sqlx::query_scalar("SELECT team_id FROM pipelines WHERE id = ?")
        .bind(&id)
        .fetch_one(&state.pool)
        .await?;
    if team_id != stored_team_id {
        let active_jobs: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM jobs WHERE pipeline_id = ? \
             AND status IN ('pending', 'queued', 'waiting_approval', 'running')",
        )
        .bind(&id)
        .fetch_one(&state.pool)
        .await?;
        if active_jobs > 0 {
            return Err(AppError::Conflict(format!(
                "pipeline team cannot change while {active_jobs} active job(s) exist; finish or cancel them first"
            )));
        }
    }

    let labels_json = serde_json::to_string(&req.runner_labels).unwrap_or_else(|_| "[]".into());
    let input_defs_json =
        serde_json::to_string(&req.input_defs.unwrap_or_default()).unwrap_or_else(|_| "[]".into());
    let environments_json =
        serde_json::to_string(&req.environments).unwrap_or_else(|_| "[]".into());

    let http = crate::services::plugin_pool::http_client(
        crate::services::platform_settings::effective(
            &state.pool,
            crate::services::platform_settings::KEY_PLUGIN_SYNC_TIMEOUT,
            &state.config.plugin_index.sync_timeout_seconds.to_string(),
        )
        .await
        .parse()
        .unwrap_or(crate::services::platform_settings::DEFAULT_STEP_SYNC_TIMEOUT_SECS),
    );
    let resolved_steps = crate::services::plugin_resolver::resolve_uses_steps_async(
        req.steps,
        crate::services::plugin_resolver::effective_local_dir(
            &state.config.storage.data_dir,
            &state.config.plugin_index.local_dir,
        )
        .map_err(|e| AppError::Internal(e.to_string()))?,
        &state.pool,
        &http,
    )
    .await
    .map_err(AppError::ValidationFailed)?;
    let mut tx = state.pool.begin_with("BEGIN IMMEDIATE").await?;
    if team_id != stored_team_id {
        let active_jobs: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM jobs WHERE pipeline_id = ? \
             AND status IN ('pending', 'queued', 'waiting_approval', 'running')",
        )
        .bind(&id)
        .fetch_one(&mut *tx)
        .await?;
        if active_jobs > 0 {
            tx.rollback().await?;
            return Err(AppError::Conflict(format!(
                "pipeline team cannot change while {active_jobs} active job(s) exist; finish or cancel them first"
            )));
        }
    }
    let result = sqlx::query(
        "UPDATE pipelines SET name = ?, description = ?, trigger_type = ?, webhook_path = ?,
         cron_expr = ?, runner_labels = ?, timeout_seconds = ?, concurrency_group = ?,
         concurrency_mode = ?, concurrency_max_queue = ?, concurrency_queue_timeout_seconds = ?,
         owner_group = ?, team_id = ?, input_defs = ?, environments = ?,
         updated_at = datetime('now')
         WHERE id = ?",
    )
    .bind(&req.name)
    .bind(req.description.as_deref().unwrap_or(""))
    .bind(&req.trigger_type)
    .bind(&req.webhook_path)
    .bind(&req.cron_expr)
    .bind(&labels_json)
    .bind(req.timeout_seconds.unwrap_or(3600))
    .bind(&req.concurrency_group)
    .bind(req.concurrency_mode.as_deref().unwrap_or(""))
    .bind(req.concurrency_max_queue.unwrap_or(3))
    .bind(req.concurrency_queue_timeout_seconds.unwrap_or(1800))
    .bind(&owner_group)
    .bind(&team_id)
    .bind(&input_defs_json)
    .bind(&environments_json)
    .bind(&id)
    .execute(&mut *tx)
    .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("pipeline not found".into()));
    }

    sqlx::query("DELETE FROM step_resolutions WHERE pipeline_id = ?")
        .bind(&id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM tasks WHERE pipeline_id = ?")
        .bind(&id)
        .execute(&mut *tx)
        .await?;

    // Delete old variables
    sqlx::query("DELETE FROM variables WHERE pipeline_id = ?")
        .bind(&id)
        .execute(&mut *tx)
        .await?;

    // Delete old explicit secret bindings
    sqlx::query("DELETE FROM pipeline_secrets WHERE pipeline_id = ?")
        .bind(&id)
        .execute(&mut *tx)
        .await?;

    // Delete old explicit variable bindings
    sqlx::query("DELETE FROM pipeline_vars WHERE pipeline_id = ?")
        .bind(&id)
        .execute(&mut *tx)
        .await?;

    let steps = insert_steps(&mut *tx, &id, &resolved_steps.steps).await?;
    crate::services::plugin_resolver::replace_step_resolutions(
        &mut *tx,
        &id,
        &resolved_steps.resolutions,
    )
    .await
    .map_err(AppError::Internal)?;

    if let Some(envs) = &req.env {
        insert_env_vars(&mut *tx, &id, envs).await?;
    }
    if let Some(secret_ids) = &req.secret_ids {
        insert_pipeline_secret_bindings(&mut *tx, &id, secret_ids).await?;
    }
    if let Some(var_ids) = &req.var_ids {
        insert_pipeline_var_bindings(&mut *tx, &id, var_ids).await?;
    }
    tx.commit().await?;

    tracing::info!("Pipeline updated: {id}");

    audit::record(
        &state.pool,
        AuditEvent::from_auth(&auth, Action::PipelineUpdate)
            .resource("pipeline", &id, &req.name)
            .team(Some(team_id.clone()))
            .before(before)
            .after(Some(serde_json::json!({
                "id": id,
                "name": req.name,
                "trigger_type": req.trigger_type,
                "team_id": team_id,
                "owner_group": owner_group,
                "step_count": steps.len(),
            }))),
    )
    .await;

    Ok(Json(ApiResponse::success(serde_json::json!({
        "id": id,
        "name": req.name,
        "environments": req.environments,
        "steps": steps,
    }))))
}

pub async fn delete(
    auth: AuthUser,
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    let team_id = pipeline_team_id(&state.pool, &id).await?;
    auth.check_team(&team_id, Perm::PipelineDelete)?;

    let before_row = sqlx::query(
        "SELECT name, trigger_type, team_id, owner_group, source, yaml_path FROM pipelines WHERE id = ?",
    )
    .bind(&id)
    .fetch_optional(&state.pool)
    .await?;
    let (name, before, yaml_path_to_remove) = match before_row {
        Some(r) => {
            let name: String = r.get(0);
            let source: String = r.get(4);
            let yaml_path: Option<String> = r.get(5);
            let path = if source == "yaml" {
                yaml_path.clone()
            } else {
                None
            };
            (
                name.clone(),
                Some(serde_json::json!({
                    "id": id,
                    "name": name,
                    "trigger_type": r.get::<String, _>(1),
                    "team_id": r.get::<String, _>(2),
                    "owner_group": r.get::<String, _>(3),
                    "source": source,
                    "yaml_path": &path,
                })),
                path,
            )
        }
        None => (id.clone(), None, None),
    };

    let result = sqlx::query("DELETE FROM pipelines WHERE id = ?")
        .bind(&id)
        .execute(&state.pool)
        .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("pipeline not found".into()));
    }

    // File is authority — remove the YAML when deleting a file-backed pipeline.
    if let Some(ref yaml_path) = yaml_path_to_remove {
        let scan_dir = &state.config.pipeline.yaml_scan_dir;
        if let Some(filename) =
            crate::services::pipeline_scanner::filename_from_yaml_path(yaml_path)
        {
            if let Ok(path) =
                crate::services::pipeline_scanner::resolve_yaml_path(scan_dir, &filename)
            {
                if path.exists() {
                    if let Err(e) = tokio::fs::remove_file(&path).await {
                        tracing::warn!("Failed to remove YAML file {yaml_path}: {e}");
                    }
                }
            }
        }
    }

    audit::record(
        &state.pool,
        AuditEvent::from_auth(&auth, Action::PipelineDelete)
            .resource("pipeline", &id, &name)
            .team(Some(team_id))
            .before(before),
    )
    .await;

    tracing::info!("Pipeline deleted: {id}");
    Ok(Json(ApiResponse::success(())))
}

pub async fn toggle_active(
    auth: AuthUser,
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let team_id = pipeline_team_id(&state.pool, &id).await?;
    auth.check_team(&team_id, Perm::PipelineWrite)?;

    let row = sqlx::query("SELECT name, is_active FROM pipelines WHERE id = ?")
        .bind(&id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(|| AppError::NotFound("pipeline not found".into()))?;

    let current_active: i64 = row.get(1);
    let new_active = if current_active != 0 { 0 } else { 1 };

    sqlx::query("UPDATE pipelines SET is_active = ?, updated_at = datetime('now') WHERE id = ?")
        .bind(new_active)
        .bind(&id)
        .execute(&state.pool)
        .await?;

    let action = if new_active != 0 {
        "activated"
    } else {
        "deactivated"
    };
    let name: String = row.get(0);
    tracing::info!("Pipeline {name} ({id}) {action}");

    audit::record(
        &state.pool,
        AuditEvent::from_auth(&auth, Action::PipelineToggleActive)
            .resource("pipeline", &id, &name)
            .team(Some(team_id))
            .before(Some(
                serde_json::json!({ "is_active": current_active != 0 }),
            ))
            .after(Some(serde_json::json!({ "is_active": new_active != 0 }))),
    )
    .await;

    Ok(Json(ApiResponse::success(serde_json::json!({
        "is_active": new_active != 0
    }))))
}

#[derive(Debug, Default, Deserialize)]
pub struct DisablePipelineRequest {
    /// Also cancel every in-flight (pending / queued / running /
    /// waiting_approval) job of this pipeline.
    #[serde(default)]
    pub cancel_in_flight: bool,
}

/// Idempotent emergency stop: ensures the pipeline is disabled and, on
/// request, cancels every in-flight job in the same transaction. Unlike
/// `toggle-active`, repeating this call never re-enables the pipeline.
pub async fn disable(
    auth: AuthUser,
    Path(id): Path<String>,
    State(state): State<AppState>,
    body: Option<Json<DisablePipelineRequest>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let req = body.map(|Json(b)| b).unwrap_or_default();
    let team_id = pipeline_team_id(&state.pool, &id).await?;
    auth.check_team(&team_id, Perm::PipelineWrite)?;

    let mut tx = state.pool.begin_with("BEGIN IMMEDIATE").await?;
    let row = sqlx::query("SELECT name, is_active FROM pipelines WHERE id = ?")
        .bind(&id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| AppError::NotFound("pipeline not found".into()))?;
    let name: String = row.get(0);
    let was_active: bool = row.get::<i64, _>(1) != 0;

    // Flip first, then scan: the write lock is held from BEGIN IMMEDIATE, so
    // a trigger that re-checks is_active inside its own insert transaction
    // cannot slip a new job past this scan after we commit.
    if was_active {
        sqlx::query(
            "UPDATE pipelines SET is_active = 0, updated_at = datetime('now') WHERE id = ?",
        )
        .bind(&id)
        .execute(&mut *tx)
        .await?;
    }

    // (job_id, debug_unredact, step_statuses) for post-commit fan-out.
    let mut cancelled: Vec<(String, bool, Vec<serde_json::Value>)> = Vec::new();
    if req.cancel_in_flight {
        let targets = sqlx::query(
            "SELECT id, debug_unredact FROM jobs
             WHERE pipeline_id = ? AND status IN ('pending', 'running', 'queued', 'waiting_approval')
             ORDER BY created_at, rowid",
        )
        .bind(&id)
        .fetch_all(&mut *tx)
        .await?;
        for t in targets {
            let job_id: String = t.get(0);
            let debug_unredact: bool = t.get::<i64, _>(1) != 0;
            if let Some(info) =
                super::jobs::cancel_job_in_tx(&mut tx, &job_id, &auth.id, &auth.username).await?
            {
                tracing::info!(
                    operator = %auth.username,
                    job_id = %job_id,
                    pipeline = %name,
                    prev_status = %info.old_status,
                    "Job cancelled by pipeline disable"
                );
                cancelled.push((job_id, debug_unredact, info.step_statuses));
            }
        }
    }
    tx.commit().await?;
    if !cancelled.is_empty() {
        crate::services::poll_events::wake_all();
    }

    // Post-commit fan-out per cancelled job: notify channels + WebSocket.
    for (job_id, debug_unredact, step_statuses) in &cancelled {
        crate::services::notify::spawn_job_notify(
            state.pool.clone(),
            state.config.auth.jwt_secret.clone(),
            state.config.server.public_base_url.clone(),
            job_id.clone(),
            "cancelled".to_string(),
        );
        let payload = serde_json::json!({
            "id": job_id,
            "status": "cancelled",
            "step_statuses": step_statuses,
            "end_time": super::jobs::now_utc(),
            "debug_unredact": debug_unredact,
        });
        state.log_broadcast.broadcast_status(
            job_id,
            &id,
            &serde_json::to_string(&payload).unwrap_or_default(),
        );
    }

    let cancelled_ids: Vec<&str> = cancelled.iter().map(|(j, _, _)| j.as_str()).collect();
    audit::record(
        &state.pool,
        AuditEvent::from_auth(&auth, Action::PipelineDisable)
            .resource("pipeline", &id, &name)
            .team(Some(team_id))
            .before(Some(serde_json::json!({ "is_active": was_active })))
            .after(Some(serde_json::json!({ "is_active": false })))
            .meta(Some(serde_json::json!({
                "cancel_in_flight": req.cancel_in_flight,
                "cancelled_count": cancelled.len(),
                "cancelled_jobs": cancelled_ids,
            }))),
    )
    .await;

    tracing::info!(
        "Pipeline {name} ({id}) disabled (cancelled {} in-flight jobs)",
        cancelled.len()
    );

    Ok(Json(ApiResponse::success(serde_json::json!({
        "is_active": false,
        "cancelled_count": cancelled.len(),
        "cancelled_jobs": cancelled_ids,
    }))))
}

/// Idempotent re-enable. No-op when already active; never touches jobs —
/// runs cancelled by `disable` stay cancelled.
pub async fn enable(
    auth: AuthUser,
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let team_id = pipeline_team_id(&state.pool, &id).await?;
    auth.check_team(&team_id, Perm::PipelineWrite)?;

    let mut tx = state.pool.begin_with("BEGIN IMMEDIATE").await?;
    let row = sqlx::query("SELECT name, is_active FROM pipelines WHERE id = ?")
        .bind(&id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| AppError::NotFound("pipeline not found".into()))?;
    let name: String = row.get(0);
    let was_active: bool = row.get::<i64, _>(1) != 0;
    if !was_active {
        sqlx::query(
            "UPDATE pipelines SET is_active = 1, updated_at = datetime('now') WHERE id = ?",
        )
        .bind(&id)
        .execute(&mut *tx)
        .await?;
    }
    // Audit inside the transaction: an unauditable enable is a failed enable.
    audit::record_in(
        &mut *tx,
        AuditEvent::from_auth(&auth, Action::PipelineEnable)
            .resource("pipeline", &id, &name)
            .team(Some(team_id))
            .before(Some(serde_json::json!({ "is_active": was_active })))
            .after(Some(serde_json::json!({ "is_active": true }))),
    )
    .await?;
    tx.commit().await?;

    tracing::info!("Pipeline {name} ({id}) enabled");

    Ok(Json(ApiResponse::success(serde_json::json!({
        "is_active": true
    }))))
}

pub async fn trigger(
    auth: AuthUser,
    Path(id): Path<String>,
    State(state): State<AppState>,
    Json(body): Json<shared::protocol::TriggerPipelineRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let team_id = pipeline_team_id(&state.pool, &id).await?;
    auth.check_team(&team_id, Perm::PipelineTrigger)?;

    // Admin-only debug trigger: skips secret redaction in the Runner's logs for this job.
    // Non-admins are rejected outright; the flag is never honored silently.
    let debug_unredact = body.debug.unwrap_or(false);
    if debug_unredact && !auth.is_platform_admin {
        return Err(AppError::Unauthorized(
            "debug (no-redaction) trigger requires a platform admin".into(),
        ));
    }

    // Verbose execution mode is available to everyone — it only adds detail,
    // it never reduces redaction.
    let verbose = body.verbose.unwrap_or(false);

    let result = do_trigger(
        &state,
        &id,
        &auth.username,
        &body.inputs,
        debug_unredact,
        verbose,
        body.environment.as_deref(),
        Some(auth.id.as_str()),
        Some(&auth),
    )
    .await?;
    let name: String = sqlx::query_scalar("SELECT name FROM pipelines WHERE id = ?")
        .bind(&id)
        .fetch_optional(&state.pool)
        .await?
        .unwrap_or_else(|| id.clone());
    let job_id = result
        .0
        .data
        .as_ref()
        .and_then(|d| d.get("job_id"))
        .cloned();

    let input_defs_raw: String =
        sqlx::query_scalar("SELECT input_defs FROM pipelines WHERE id = ?")
            .bind(&id)
            .fetch_optional(&state.pool)
            .await?
            .unwrap_or_else(|| "[]".into());
    let defs: Vec<PipelineInputDef> = serde_json::from_str(&input_defs_raw).unwrap_or_default();
    let masking_keys = masking_input_names(&defs);
    let audit_inputs = redact_input_map(&body.inputs, &masking_keys);

    audit::record(
        &state.pool,
        AuditEvent::from_auth(&auth, Action::PipelineTrigger)
            .resource("pipeline", &id, &name)
            .team(Some(team_id))
            .after(Some(serde_json::json!({
                "job_id": job_id,
                "inputs": audit_inputs,
                "debug_unredact": debug_unredact,
                "verbose": verbose,
                "environment": body.environment,
            }))),
    )
    .await;
    Ok(result)
}

pub(crate) async fn resolve_concurrency_status_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
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
            let running = sqlx::query(
                "SELECT COUNT(*) as cnt FROM jobs
                 WHERE pipeline_id = ? AND status IN ('pending', 'running', 'queued')",
            )
            .bind(pipeline_id)
            .fetch_one(&mut **tx)
            .await?;
            let count: i64 = running.get(0);
            if count > 0 {
                return Err(AppError::Conflict(format!(
                    "concurrency group '{group}' has {count} active/queued job(s)"
                )));
            }
            Ok("pending")
        }

        "discard-old" => {
            sqlx::query(
                "UPDATE jobs SET status = 'cancelled', updated_at = datetime('now')
                 WHERE pipeline_id = ? AND status = 'queued'",
            )
            .bind(pipeline_id)
            .execute(&mut **tx)
            .await?;
            Ok("pending")
        }

        "queue" => {
            let queued_count: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM jobs
                 WHERE pipeline_id = ? AND status = 'queued'",
            )
            .bind(pipeline_id)
            .fetch_one(&mut **tx)
            .await?;

            if queued_count >= concurrency_max_queue {
                return Err(AppError::Conflict(format!(
                    "queue full: {queued_count} jobs already queued (max {concurrency_max_queue})"
                )));
            }

            let running_count: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM jobs
                 WHERE pipeline_id = ? AND status IN ('pending', 'running')",
            )
            .bind(pipeline_id)
            .fetch_one(&mut **tx)
            .await?;

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

/// Shared trigger logic — used by admin trigger, webhooks, and cron scheduler.
///
/// `trigger_user_id` / `auth` are `Some` for manual UI triggers (ACL + self-approve);
/// automated sources (webhook / cron / git) pass `None`.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn do_trigger(
    state: &AppState,
    pipeline_id: &str,
    triggered_by: &str,
    inputs: &std::collections::HashMap<String, String>,
    debug_unredact: bool,
    verbose: bool,
    environment: Option<&str>,
    trigger_user_id: Option<&str>,
    auth: Option<&AuthUser>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    // File-backed pipelines: re-validate on-disk YAML and refresh DB projection
    // so a broken/stale file cannot silently run the previous good snapshot.
    crate::services::pipeline_scanner::sync_yaml_before_trigger(
        &state.pool,
        &state.config.pipeline.yaml_scan_dir,
        &crate::services::plugin_resolver::effective_local_dir(
            &state.config.storage.data_dir,
            &state.config.plugin_index.local_dir,
        )
        .map_err(|e| AppError::Internal(e.to_string()))?,
        pipeline_id,
        crate::services::platform_settings::effective(
            &state.pool,
            crate::services::platform_settings::KEY_PLUGIN_SYNC_TIMEOUT,
            &state.config.plugin_index.sync_timeout_seconds.to_string(),
        )
        .await
        .parse()
        .unwrap_or(crate::services::platform_settings::DEFAULT_STEP_SYNC_TIMEOUT_SECS),
    )
    .await?;

    let pipeline = sqlx::query(
        "SELECT id, name, runner_labels, concurrency_group, concurrency_mode,
                concurrency_max_queue, concurrency_queue_timeout_seconds, input_defs, team_id,
                environments, is_active
         FROM pipelines WHERE id = ?",
    )
    .bind(pipeline_id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound("pipeline not found".into()))?;
    if pipeline.get::<i64, _>(10) == 0 {
        return Err(AppError::Conflict(
            "pipeline is disabled — enable it before triggering".into(),
        ));
    }

    let concurrency_group: Option<String> = pipeline.get(3);
    let concurrency_mode: String = pipeline.get(4);
    let concurrency_max_queue: i64 = pipeline.get(5);
    let concurrency_queue_timeout: i64 = pipeline.get(6);
    let input_defs_raw: String = pipeline.get(7);
    let team_id: String = pipeline.get(8);
    let declared_environments: Vec<String> =
        serde_json::from_str(&pipeline.get::<String, _>(9)).unwrap_or_default();
    let input_defs: Vec<PipelineInputDef> =
        serde_json::from_str(&input_defs_raw).unwrap_or_default();
    let masking_keys = masking_input_names(&input_defs);

    // Merge declared defaults, then validate inputs against the pipeline's
    // declared constraints (type / pattern / choice / host / required) before
    // anything is scheduled. Fail fast with field-level errors so the UI can
    // point at the offending input.
    let mut resolved_inputs = inputs.clone();
    for def in &input_defs {
        let present = resolved_inputs
            .get(&def.name)
            .map(|v| !v.trim().is_empty())
            .unwrap_or(false);
        if !present {
            if let Some(d) = def.default.as_deref() {
                resolved_inputs.insert(def.name.clone(), d.to_string());
            }
        }
    }
    // Host references are validated against the hosts registry. Load the
    // name set once up-front so the shared validator stays DB-agnostic.
    let host_names: std::collections::HashSet<String> =
        sqlx::query_scalar("SELECT name FROM hosts")
            .fetch_all(&state.pool)
            .await?
            .into_iter()
            .collect();
    if let Err(field_errors) = validate_input_values(&input_defs, &resolved_inputs, |host_name| {
        host_names.contains(host_name)
    }) {
        let detail = field_errors
            .iter()
            .map(|e| format!("{}: {}", e.name, e.message))
            .collect::<Vec<_>>()
            .join("; ");
        return Err(AppError::ValidationFailed(format!(
            "invalid trigger inputs — {detail}"
        )));
    }
    let inputs = &resolved_inputs;

    let inputs_for_snapshot = redact_input_map(inputs, &masking_keys);

    // Effective mode frozen into the job snapshot (Job Detail). Empty mode with
    // a non-empty group matches legacy trigger behavior → discard-new.
    let effective_concurrency_mode: Option<String> = concurrency_group
        .as_ref()
        .filter(|g| !g.is_empty())
        .map(|_| {
            if concurrency_mode.is_empty() {
                "discard-new".to_string()
            } else {
                concurrency_mode.clone()
            }
        });

    // Multi-env governance: every run must explicitly pick one of the
    // pipeline's declared environments (GHA binds environment in code; buz
    // binds it in the pipeline definition and enforces at trigger time).
    let environment = if declared_environments.is_empty() {
        // Legacy unbound pipeline (pre-migration row) — keep old behavior.
        environment
    } else {
        let Some(env_slug) = environment.map(str::trim).filter(|s| !s.is_empty()) else {
            return Err(AppError::ValidationFailed(format!(
                "environment is required — pipeline targets {}",
                declared_environments.join(", ")
            )));
        };
        if !declared_environments.iter().any(|e| e == env_slug) {
            return Err(AppError::ValidationFailed(format!(
                "environment '{env_slug}' is not in this pipeline's declared environments [{}]",
                declared_environments.join(", ")
            )));
        }
        Some(env_slug)
    };

    let env_resolved = super::environments::resolve_environment(&state.pool, environment).await?;
    let (environment_id, environment_slug, env_protection) = match &env_resolved {
        Some(e) => (
            Some(e.id.as_str()),
            Some(e.slug.as_str()),
            Some(&e.protection),
        ),
        None => (None, None, None),
    };

    // ── Protection gate: waiting_approval skips concurrency until approved ──
    let needs_approval = env_protection.map(|p| p.enabled).unwrap_or(false);
    let job_status: &str = if needs_approval {
        let protection = env_protection.expect("checked above");
        match auth {
            None => {
                return Err(AppError::Unauthorized(
                    "protected environment rejects automated triggers".into(),
                ));
            }
            Some(user) => {
                user.check_team_role(&team_id, protection.min_trigger_role)?;
            }
        }
        "waiting_approval"
    } else {
        let mut tx = state.pool.begin_with("BEGIN IMMEDIATE").await?;
        let status = resolve_concurrency_status_in_tx(
            &mut tx,
            pipeline_id,
            concurrency_group.as_deref(),
            &concurrency_mode,
            concurrency_max_queue,
        )
        .await?;
        tx.commit().await?;
        status
    };

    // ── Freeze snapshot ──
    let job_id = Uuid::new_v4().to_string();
    let pipeline_name: String = pipeline.get(1);
    let runner_labels: String = pipeline.get(2);

    // Capture the full pipeline definition so historical runs are
    // reproducible even if the pipeline is later modified or deleted.
    let mut tasks_snapshot = sqlx::query(
        "SELECT step_order, name, exec_type, host_id, host_names, host_labels, command_template,
                image, work_dir, retry_count, timeout_seconds, fail_strategy, step_secrets, step_env, volumes,
                privileged, registry_secret, config_source, config_key_path, config_render, config_dest,
                hosts_strategy, id, capabilities
         FROM tasks WHERE pipeline_id = ? ORDER BY step_order",
    )
    .bind(pipeline_id)
    .fetch_all(&state.pool)
    .await?
    .into_iter()
    .map(|t| {
        serde_json::json!({
            "step_order": t.get::<i64, _>(0),
            "name": t.get::<String, _>(1),
            "exec_type": t.get::<String, _>(2),
            "host_id": t.get::<Option<String>, _>(3),
            "host_names": t.get::<String, _>(4),
            "host_labels": t.get::<String, _>(5),
            "command_template": t.get::<String, _>(6),
            "image": t.get::<String, _>(7),
            "work_dir": t.get::<String, _>(8),
            "retry_count": t.get::<i64, _>(9),
            "timeout_seconds": t.get::<i64, _>(10),
            "fail_strategy": t.get::<String, _>(11),
            "step_secrets": t.get::<String, _>(12),
            "step_env": serde_json::from_str::<serde_json::Value>(
                &t.get::<String, _>(13),
            ).ok().filter(|v| v.is_object()),
            "volumes": serde_json::from_str::<serde_json::Value>(
                &t.get::<String, _>(14),
            ).unwrap_or(serde_json::Value::Array(vec![])),
            "privileged": t.get::<i64, _>(15) != 0,
            "registry_secret": t.get::<String, _>(16),
            "config_source": t.get::<String, _>(17),
            "config_key_path": t.get::<String, _>(18),
            "config_render": serde_json::from_str::<serde_json::Value>(
                &t.get::<String, _>(19),
            ).ok().filter(|v| !v.is_null()),
            "config_dest": t.get::<String, _>(20),
            "hosts_strategy": t.get::<String, _>(21),
            "id": t.get::<String, _>(22),
            "capabilities": serde_json::from_str::<Vec<String>>(&t.get::<String, _>(23)).unwrap_or_default(),
        })
    })
    .collect::<Vec<_>>();

    let resolutions = sqlx::query(
        "SELECT step_order, uses_ref, resolved_version, content_sha256, source, reviewed, input_defs_json \
         FROM step_resolutions WHERE pipeline_id = ?",
    )
    .bind(pipeline_id)
    .fetch_all(&state.pool)
    .await?;
    for resolution in resolutions {
        let step_order: i64 = resolution.get(0);
        if let Some(task) = tasks_snapshot
            .iter_mut()
            .find(|task| task["step_order"].as_i64() == Some(step_order))
        {
            task["marketplace"] = serde_json::json!({
                "uses_ref": resolution.get::<String, _>(1),
                "resolved_version": resolution.get::<String, _>(2),
                "content_sha256": resolution.get::<String, _>(3),
                "source": resolution.get::<String, _>(4),
                "reviewed": resolution.get::<i64, _>(5) != 0,
                "inputs": serde_json::from_str::<serde_json::Value>(
                    &resolution.get::<String, _>(6),
                ).unwrap_or(serde_json::json!([])),
            });
        }
    }

    // ── Environment host allowlist ──────────────────────────────────────────
    // A deployment to the chosen environment may only touch hosts the
    // environment declares (by name, or by host label). Empty allowlist =
    // unrestricted. Static `host:` / `host_labels:` declarations are checked
    // Static host declarations are checked at trigger time. Dynamic host
    // references resolve only when a Runner claims the job, so restricted
    // environments reject them rather than allowing an unchecked target.
    if let Some(env_id) = environment_id.as_deref() {
        let allowed: Vec<(String, String)> =
            sqlx::query("SELECT kind, value FROM environment_hosts WHERE environment_id = ?")
                .bind(env_id)
                .fetch_all(&state.pool)
                .await?
                .into_iter()
                .map(|r| (r.get::<String, _>(0), r.get::<String, _>(1)))
                .collect();
        if !allowed.is_empty() {
            let allowed_names: std::collections::HashSet<&str> = allowed
                .iter()
                .filter(|(kind, _)| kind == "name")
                .map(|(_, v)| v.as_str())
                .collect();
            let allowed_labels: std::collections::HashSet<&str> = allowed
                .iter()
                .filter(|(kind, _)| kind == "label")
                .map(|(_, v)| v.as_str())
                .collect();
            // Preload host → labels once so label membership is checked in
            // memory (hosts.labels is a JSON string array).
            let host_label_sets: std::collections::HashMap<
                String,
                std::collections::HashSet<String>,
            > = sqlx::query("SELECT name, labels FROM hosts")
                .fetch_all(&state.pool)
                .await?
                .into_iter()
                .map(|r| {
                    let name: String = r.get(0);
                    let labels: Vec<String> =
                        serde_json::from_str(&r.get::<String, _>(1)).unwrap_or_default();
                    (name, labels.into_iter().collect())
                })
                .collect();
            let mut offenders: Vec<String> = Vec::new();
            for task in &tasks_snapshot {
                let step = task["name"].as_str().unwrap_or("unnamed");
                for name in task["host_names"]
                    .as_str()
                    .unwrap_or("")
                    .split(',')
                    .map(str::trim)
                    .filter(|n| !n.is_empty())
                {
                    if name.contains('$') {
                        offenders.push(format!("step '{step}' → dynamic host '{name}'"));
                        continue;
                    }
                    let ok = allowed_names.contains(name)
                        || host_label_sets.get(name).is_some_and(|host_labels| {
                            host_labels
                                .iter()
                                .any(|host_label| allowed_labels.contains(host_label.as_str()))
                        });
                    if !ok {
                        offenders.push(format!("step '{step}' → host '{name}'"));
                    }
                }
                for label in task["host_labels"]
                    .as_str()
                    .unwrap_or("")
                    .split(',')
                    .map(str::trim)
                    .filter(|l| !l.is_empty())
                {
                    if label.contains('$') {
                        offenders.push(format!("step '{step}' → dynamic host label '{label}'"));
                    } else if !allowed_labels.contains(label) {
                        offenders.push(format!("step '{step}' → host label '{label}'"));
                    }
                }
            }
            if !offenders.is_empty() {
                return Err(AppError::ValidationFailed(format!(
                    "environment '{}' host allowlist rejects: {}",
                    environment_slug.unwrap_or("default"),
                    offenders.join("; ")
                )));
            }
        }
    }

    let env_snapshot =
        sqlx::query("SELECT var_name, is_secret FROM variables WHERE pipeline_id = ?")
            .bind(pipeline_id)
            .fetch_all(&state.pool)
            .await?
            .into_iter()
            .map(|v| {
                serde_json::json!({
                    "name": v.get::<String, _>(0),
                    "is_secret": v.get::<i64, _>(1) != 0,
                })
            })
            .collect::<Vec<_>>();

    let pipeline_snapshot = sqlx::query(
        "SELECT trigger_type, timeout_seconds, source
         FROM pipelines WHERE id = ?",
    )
    .bind(pipeline_id)
    .fetch_optional(&state.pool)
    .await?
    .map(|p| {
        serde_json::json!({
            "trigger_type": p.get::<String, _>(0),
            "timeout_seconds": p.get::<i64, _>(1),
            "source": p.get::<String, _>(2),
        })
    })
    .unwrap_or(serde_json::json!({}));

    // Freeze notify config from on-disk YAML (keep `${secrets.…}`; resolve at send).
    let notify_snapshot = crate::services::notify::load_notify_snapshot(
        &state.pool,
        &state.config.pipeline.yaml_scan_dir,
        pipeline_id,
    )
    .await;

    let mut snapshot = serde_json::json!({
        "pipeline": pipeline_snapshot,
        "team_id": team_id,
        "runner_labels": serde_json::from_str::<Vec<String>>(&runner_labels).unwrap_or_default(),
        "concurrency_group": concurrency_group,
        "concurrency_mode": effective_concurrency_mode,
        "concurrency_max_queue": concurrency_group
            .as_ref()
            .filter(|g| !g.is_empty())
            .map(|_| concurrency_max_queue),
        "concurrency_queue_timeout_seconds": concurrency_group
            .as_ref()
            .filter(|g| !g.is_empty())
            .map(|_| concurrency_queue_timeout),
        "tasks": tasks_snapshot,
        "env": env_snapshot,
        "inputs": inputs_for_snapshot,
        "masking_input_keys": masking_keys,
        "triggered_by": triggered_by,
        "triggered_at": chrono::Utc::now().to_rfc3339(),
        "notify": notify_snapshot,
        "environment": environment_slug,
    });
    if let Some(protection) = env_protection {
        if let Some(map) = snapshot.as_object_mut() {
            map.insert("protection".into(), protection.to_snapshot_json());
        }
    }
    if job_status == "waiting_approval" {
        if let Some(map) = snapshot.as_object_mut() {
            map.insert(
                "approval".into(),
                serde_json::json!({ "required": true, "received": 0 }),
            );
        }
    }

    // Masking input values encrypted at rest (buzenc:v1:…); claim decrypts for Runner.
    let sealed = crypto::seal_trigger_params(inputs, &masking_keys, &state.config.auth.jwt_secret)
        .map_err(AppError::Internal)?;
    let trigger_params_json = serde_json::to_string(&sealed).unwrap_or_else(|_| "{}".into());

    // Allocate the GHA-style build number and create the job atomically, so
    // concurrent triggers of one pipeline get gapless, unique numbers.
    // BEGIN IMMEDIATE serializes against `disable`, whose transaction flips
    // is_active and cancels in-flight jobs — re-checking is_active here closes
    // the check-to-insert race with a concurrent disable.
    let mut tx = state
        .pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;
    match sqlx::query_scalar::<_, i64>("SELECT is_active FROM pipelines WHERE id = ?")
        .bind(pipeline_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?
    {
        None => return Err(AppError::NotFound("pipeline not found".into())),
        Some(0) => {
            return Err(AppError::Conflict(
                "pipeline is disabled — enable it before triggering".into(),
            ));
        }
        Some(_) => {}
    }
    let run_number = crate::db::allocate_run_number(&mut tx, pipeline_id)
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;
    sqlx::query(
        "INSERT INTO jobs (id, pipeline_id, status, trigger_by, trigger_params, snapshot,
                           debug_unredact, verbose_logs, environment_id, trigger_user_id,
                           run_number, run_attempt, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 1, datetime('now'), datetime('now'))",
    )
    .bind(&job_id)
    .bind(pipeline_id)
    .bind(job_status)
    .bind(triggered_by)
    .bind(&trigger_params_json)
    .bind(serde_json::to_string(&snapshot).unwrap_or_else(|_| "{}".into()))
    .bind(if debug_unredact { 1 } else { 0 })
    .bind(if verbose { 1 } else { 0 })
    .bind(environment_id)
    .bind(trigger_user_id)
    .bind(run_number)
    .execute(&mut *tx)
    .await?;
    tx.commit()
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;
    crate::services::poll_events::wake_all();

    tracing::info!("Pipeline triggered: {pipeline_name} → job {job_id} (by: {triggered_by})");

    // Git adapter records a richer TriggerEvent itself; skip duplicate here.
    if triggered_by != "git" {
        let source = match triggered_by {
            "webhook" => "webhook",
            "cron" => "cron",
            _ => "manual",
        };
        super::trigger_events::record_simple_trigger(
            &state.pool,
            pipeline_id,
            source,
            triggered_by,
            &job_id,
        )
        .await;
    }

    let mut resp = serde_json::json!({
        "job_id": job_id,
        "pipeline_id": pipeline_id,
        "pipeline_name": pipeline_name,
        "status": job_status,
    });
    if job_status == "waiting_approval" {
        if let Some(map) = resp.as_object_mut() {
            map.insert("approval_required".into(), serde_json::json!(true));
        }
    }

    Ok(Json(ApiResponse::success(resp)))
}

// --- helpers ---

async fn verify_pipeline_secret_bindings(
    pool: &sqlx::SqlitePool,
    auth: &AuthUser,
    team_id: &str,
    secret_ids: Option<&Vec<String>>,
) -> Result<(), AppError> {
    let Some(ids) = secret_ids.filter(|v| !v.is_empty()) else {
        return Ok(());
    };
    auth.check_team(team_id, Perm::SecretManage)?;
    for secret_id in ids {
        let secret_team = secret_team_id(pool, secret_id).await?;
        if secret_team != team_id {
            return Err(AppError::ValidationFailed(format!(
                "secret '{secret_id}' belongs to a different team"
            )));
        }
    }
    Ok(())
}

pub(crate) async fn insert_steps(
    conn: &mut sqlx::SqliteConnection,
    pipeline_id: &str,
    steps: &[StepDef],
) -> Result<Vec<serde_json::Value>, AppError> {
    let mut result = Vec::with_capacity(steps.len());

    for (i, step) in steps.iter().enumerate() {
        // Step definitions must already be expanded before they are persisted.
        if step.uses.is_some() {
            return Err(AppError::ValidationFailed(format!(
                "step '{}' uses marketplace reference '{}' — step resolution is not \
                 available in this build yet",
                step.name,
                step.uses.as_deref().unwrap_or_default()
            )));
        }
        if step.run.trim().is_empty() {
            return Err(AppError::ValidationFailed(format!(
                "step '{}' has no 'run' command",
                step.name
            )));
        }
        let task_id = Uuid::new_v4().to_string();
        let exec_type = step.engine.clone().unwrap_or_else(|| "shell".into());
        shared::plugin_spec::validate_plugin_capabilities(&step.capabilities, &exec_type)
            .map_err(AppError::ValidationFailed)?;
        let capabilities_stored = serde_json::to_string(&step.capabilities)
            .map_err(|error| AppError::Internal(error.to_string()))?;
        let fail_strategy = if step.continue_on_error.unwrap_or(false) {
            "continue"
        } else {
            "stop"
        };

        // ── Host names: store raw values, resolve static ones to IDs ──
        let raw_host_names: Vec<String> =
            step.host.as_ref().map(|h| h.to_vec()).unwrap_or_default();
        let host_names_stored = raw_host_names.join(",");

        // ── Host resolution (only for SSH steps) ──
        let host_ids = if exec_type == "ssh" {
            let mut ids: Vec<String> = Vec::new();

            // 1) Explicit host names — skip those containing '$' (dynamic references)
            for name in &raw_host_names {
                let name = name.trim();
                if name.is_empty() {
                    continue;
                }
                if name.contains('$') {
                    // Dynamic reference — defer resolution to poll time
                    tracing::debug!(
                        "Step '{}' has dynamic host reference '{}' — deferring resolution",
                        step.name,
                        name
                    );
                    continue;
                }
                if let Ok(Some(row)) = sqlx::query("SELECT id FROM hosts WHERE name = ?")
                    .bind(name)
                    .fetch_optional(&mut *conn)
                    .await
                {
                    ids.push(row.get::<String, _>(0));
                } else {
                    tracing::warn!(
                        "Step '{}' references unknown host '{}' — will fail at dispatch",
                        step.name,
                        name
                    );
                }
            }

            // 2) Host labels — skip those containing '$'
            if let Some(ref labels) = step.host_labels {
                for label in &labels.to_vec() {
                    let label = label.trim();
                    if label.is_empty() {
                        continue;
                    }
                    if label.contains('$') {
                        tracing::debug!(
                            "Step '{}' has dynamic host label '{}' — deferring resolution",
                            step.name,
                            label
                        );
                        continue;
                    }
                    match sqlx::query(
                        "SELECT id FROM hosts, json_each(hosts.labels) WHERE json_each.value = ?",
                    )
                    .bind(label)
                    .fetch_all(&mut *conn)
                    .await
                    {
                        Ok(rows) => {
                            if rows.is_empty() {
                                tracing::warn!(
                                    "Step '{}': label '{}' matched no hosts",
                                    step.name,
                                    label
                                );
                            }
                            for row in &rows {
                                ids.push(row.get::<String, _>(0));
                            }
                        }
                        Err(e) => {
                            tracing::error!(
                                "Step '{}': failed to query hosts by label '{}': {e}",
                                step.name,
                                label
                            );
                        }
                    }
                }
            }

            // Deduplicate while preserving order
            let mut seen = std::collections::HashSet::new();
            ids.retain(|id| seen.insert(id.clone()));
            Some(ids.join(","))
        } else {
            None
        };
        // Foreign key: bind NULL when no host is resolved (empty string violates REFERENCES).
        let host_id_bind: Option<String> = host_ids.filter(|s| !s.is_empty());
        // Normalize host_labels to a comma-separated string for storage
        let host_labels_stored = step
            .host_labels
            .as_ref()
            .map(|l| l.join(","))
            .unwrap_or_default();
        let hosts_strategy_stored =
            shared::pipeline_yaml::normalize_hosts_strategy(step.hosts_strategy.as_deref())
                .unwrap_or("chain")
                .to_string();
        // Step-level secret allowlist (comma-separated)
        let step_secrets_stored = step
            .secrets
            .as_ref()
            .map(|s| s.join(","))
            .unwrap_or_default();
        // Step-level env map JSON (values may contain ${secrets.X} / ${vars.X})
        let step_env_stored = step
            .env
            .as_ref()
            .filter(|m| !m.is_empty())
            .map(|m| serde_json::to_string(m).unwrap_or_default())
            .unwrap_or_default();
        let image_stored = step
            .image
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("")
            .to_string();

        let volumes_stored = step
            .volumes
            .as_ref()
            .map(|v| serde_json::to_string(v).unwrap_or_default())
            .unwrap_or_default();

        let privileged_val = if step.privileged.unwrap_or(false) {
            1i64
        } else {
            0i64
        };

        let registry_secret_stored = step
            .registry_secret
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("")
            .to_string();

        // Prefer nested `config:`; else use flattened API fields.
        let (flat_source, flat_key, flat_dest, flat_render) =
            shared::pipeline_yaml::flatten_config(step.config.as_ref());
        let config_source_stored = flat_source
            .or_else(|| {
                step.config_source
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string())
            })
            .unwrap_or_default();
        let config_key_path_stored = flat_key
            .or_else(|| {
                step.config_key_path
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string())
            })
            .unwrap_or_default();
        let config_dest_stored = flat_dest
            .or_else(|| {
                step.config_dest
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string())
            })
            .unwrap_or_default();
        let config_render_stored = flat_render
            .or_else(|| step.config_render.clone())
            .as_ref()
            .map(|e| serde_json::to_string(e).unwrap_or_default())
            .unwrap_or_default();

        sqlx::query(
            "INSERT INTO tasks (id, pipeline_id, step_order, name, exec_type, host_id, host_names, host_labels,
             command_template, image, work_dir, retry_count, timeout_seconds, fail_strategy, step_secrets,
             step_env, volumes, privileged, registry_secret, config_source, config_key_path, config_render, config_dest,
             hosts_strategy, capabilities)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&task_id)
        .bind(pipeline_id)
        .bind(i as i64)
        .bind(&step.name)
        .bind(&exec_type)
        .bind(&host_id_bind)
        .bind(&host_names_stored)
        .bind(&host_labels_stored)
        .bind(&step.run)
        .bind(&image_stored)
        .bind(step.dir.as_deref().unwrap_or(""))
        .bind(step.retry.unwrap_or(0))
        .bind(parse_timeout_seconds(step.timeout.as_deref()))
        .bind(fail_strategy)
        .bind(&step_secrets_stored)
        .bind(&step_env_stored)
        .bind(&volumes_stored)
        .bind(privileged_val)
        .bind(&registry_secret_stored)
        .bind(&config_source_stored)
        .bind(&config_key_path_stored)
        .bind(&config_render_stored)
        .bind(&config_dest_stored)
        .bind(&hosts_strategy_stored)
        .bind(&capabilities_stored)
        .execute(&mut *conn)
        .await?;

        result.push(serde_json::json!({
            "id": task_id,
            "step_order": i,
            "name": step.name,
            "exec_type": exec_type,
            "image": image_stored,
            "capabilities": step.capabilities,
        }));
    }

    Ok(result)
}

fn parse_timeout_seconds(raw: Option<&str>) -> i64 {
    match raw.map(str::trim).filter(|s| !s.is_empty()) {
        None => 3600,
        Some(s) => shared::pipeline_yaml::parse_duration_seconds(s).unwrap_or(3600),
    }
}

pub(crate) async fn insert_env_vars(
    conn: &mut sqlx::SqliteConnection,
    pipeline_id: &str,
    envs: &[shared::protocol::EnvPair],
) -> Result<(), AppError> {
    for env in envs {
        sqlx::query(
            "INSERT INTO variables (id, pipeline_id, var_name, source_type, is_secret, value)
             VALUES (?, ?, ?, 'inline', 0, ?)",
        )
        .bind(Uuid::new_v4().to_string())
        .bind(pipeline_id)
        .bind(&env.name)
        .bind(&env.value)
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
}

/// Explicitly bind variable IDs to a pipeline (many-to-many via pipeline_vars).
pub(crate) async fn insert_pipeline_var_bindings(
    conn: &mut sqlx::SqliteConnection,
    pipeline_id: &str,
    var_ids: &[String],
) -> Result<(), AppError> {
    for var_id in var_ids {
        sqlx::query(
            "INSERT OR IGNORE INTO pipeline_vars (pipeline_id, var_id)
             VALUES (?, ?)",
        )
        .bind(pipeline_id)
        .bind(var_id)
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
}

/// Explicitly bind secret IDs to a pipeline (many-to-many via pipeline_secrets).
pub(crate) async fn insert_pipeline_secret_bindings(
    conn: &mut sqlx::SqliteConnection,
    pipeline_id: &str,
    secret_ids: &[String],
) -> Result<(), AppError> {
    for secret_id in secret_ids {
        sqlx::query(
            "INSERT OR IGNORE INTO pipeline_secrets (pipeline_id, secret_id)
             VALUES (?, ?)",
        )
        .bind(pipeline_id)
        .bind(secret_id)
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
}

// ── YAML validation ──

#[derive(serde::Deserialize)]
pub struct ValidateYamlRequest {
    pub yaml: String,
}

pub async fn validate_yaml(
    auth: AuthUser,
    State(state): State<AppState>,
    Json(req): Json<ValidateYamlRequest>,
) -> Result<Json<ApiResponse<shared::pipeline_yaml::YamlValidationResult>>, AppError> {
    // Viewer-scoped: no TeamView on YAML `team:` → generic error (no secret oracle).
    let result = crate::services::yaml_validate::validate_pipeline_yaml_for_viewer(
        &state.pool,
        &req.yaml,
        &auth,
    )
    .await?;
    Ok(Json(ApiResponse::success(result)))
}

#[cfg(test)]
#[path = "tests/pipelines_list_tests.rs"]
mod tests;
