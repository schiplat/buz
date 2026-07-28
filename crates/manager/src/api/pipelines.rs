use axum::{
    extract::{Path, State},
    Json,
};
use shared::error::AppError;
use shared::protocol::response::ApiResponse;
use shared::protocol::{CreatePipelineRequest, StepDef};
use shared::rbac::Perm;
use sqlx::Row;
use uuid::Uuid;

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
    let base = "SELECT p.id, p.name, p.description, p.trigger_type, p.runner_labels,
                p.timeout_seconds, p.concurrency_group, p.input_defs, p.is_active,
                p.source, p.team_id, p.owner_group, p.created_at, p.updated_at,
                j.id as latest_job_id, j.status as latest_job_status,
                j.start_time as latest_job_start,
                COALESCE(js.job_total, 0) as job_total,
                COALESCE(js.job_success, 0) as job_success
         FROM pipelines p
         LEFT JOIN (
             SELECT pipeline_id, MAX(created_at) as max_created
             FROM jobs GROUP BY pipeline_id
         ) latest ON p.id = latest.pipeline_id
         LEFT JOIN jobs j ON j.pipeline_id = latest.pipeline_id AND j.created_at = latest.max_created
         LEFT JOIN (
             SELECT pipeline_id,
                    COUNT(*) as job_total,
                    SUM(CASE WHEN status = 'success' THEN 1 ELSE 0 END) as job_success
             FROM jobs
             GROUP BY pipeline_id
         ) js ON p.id = js.pipeline_id
         WHERE 1=1";

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

    let pipeline_id = Uuid::new_v4().to_string();
    let labels_json = serde_json::to_string(&req.runner_labels).unwrap_or_else(|_| "[]".into());
    let input_defs_json =
        serde_json::to_string(&req.input_defs.unwrap_or_default()).unwrap_or_else(|_| "[]".into());

    sqlx::query(
        "INSERT INTO pipelines (id, name, description, trigger_type, webhook_path, cron_expr,
         runner_labels, timeout_seconds, concurrency_group, owner_group, team_id, input_defs, is_active)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 1)",
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
    .bind(&owner_group)
    .bind(&team_id)
    .bind(&input_defs_json)
    .execute(&state.pool)
    .await?;

    // Insert step definitions
    let steps = insert_steps(&state.pool, &pipeline_id, &req.steps).await?;

    // Insert env variables
    if let Some(envs) = &req.env {
        insert_env_vars(&state.pool, &pipeline_id, envs).await?;
    }

    // Insert explicit secret bindings
    if let Some(secret_ids) = &req.secret_ids {
        insert_pipeline_secret_bindings(&state.pool, &pipeline_id, secret_ids).await?;
    }

    // Insert explicit variable bindings
    if let Some(var_ids) = &req.var_ids {
        insert_pipeline_var_bindings(&state.pool, &pipeline_id, var_ids).await?;
    }

    tracing::info!("Pipeline created: {} ({pipeline_id})", req.name);

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
                owner_group, team_id, created_at, updated_at
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

    let tasks = sqlx::query(
        "SELECT id, step_order, name, exec_type, host_id, host_labels, command_template, image,
                work_dir, retry_count, timeout_seconds, fail_strategy
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
            "host_labels": t.get::<String, _>(5),
            "command_template": t.get::<String, _>(6),
            "image": t.get::<String, _>(7),
            "work_dir": t.get::<String, _>(8),
            "retry_count": t.get::<i64, _>(9),
            "timeout_seconds": t.get::<i64, _>(10),
            "fail_strategy": t.get::<String, _>(11),
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
        "tasks": tasks,
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

    let labels_json = serde_json::to_string(&req.runner_labels).unwrap_or_else(|_| "[]".into());
    let input_defs_json =
        serde_json::to_string(&req.input_defs.unwrap_or_default()).unwrap_or_else(|_| "[]".into());

    let result = sqlx::query(
        "UPDATE pipelines SET name = ?, description = ?, trigger_type = ?, webhook_path = ?,
         cron_expr = ?, runner_labels = ?, timeout_seconds = ?, concurrency_group = ?,
         owner_group = ?, team_id = ?, input_defs = ?, updated_at = datetime('now')
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
    .bind(&owner_group)
    .bind(&team_id)
    .bind(&input_defs_json)
    .bind(&id)
    .execute(&state.pool)
    .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("pipeline not found".into()));
    }

    // Replace steps: delete old, insert new
    sqlx::query("DELETE FROM tasks WHERE pipeline_id = ?")
        .bind(&id)
        .execute(&state.pool)
        .await?;

    // Delete old variables
    sqlx::query("DELETE FROM variables WHERE pipeline_id = ?")
        .bind(&id)
        .execute(&state.pool)
        .await?;

    // Delete old explicit secret bindings
    sqlx::query("DELETE FROM pipeline_secrets WHERE pipeline_id = ?")
        .bind(&id)
        .execute(&state.pool)
        .await?;

    // Delete old explicit variable bindings
    sqlx::query("DELETE FROM pipeline_vars WHERE pipeline_id = ?")
        .bind(&id)
        .execute(&state.pool)
        .await?;

    let steps = insert_steps(&state.pool, &id, &req.steps).await?;

    if let Some(envs) = &req.env {
        insert_env_vars(&state.pool, &id, envs).await?;
    }
    if let Some(secret_ids) = &req.secret_ids {
        insert_pipeline_secret_bindings(&state.pool, &id, secret_ids).await?;
    }
    if let Some(var_ids) = &req.var_ids {
        insert_pipeline_var_bindings(&state.pool, &id, var_ids).await?;
    }

    tracing::info!("Pipeline updated: {id}");

    Ok(Json(ApiResponse::success(serde_json::json!({
        "id": id,
        "name": req.name,
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

    let result = sqlx::query("DELETE FROM pipelines WHERE id = ?")
        .bind(&id)
        .execute(&state.pool)
        .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("pipeline not found".into()));
    }

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

    Ok(Json(ApiResponse::success(serde_json::json!({
        "is_active": new_active != 0
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
    do_trigger(&state, &id, &auth.username, &body.inputs).await
}

/// Shared trigger logic — used by admin trigger, webhooks, and cron scheduler.
pub(crate) async fn do_trigger(
    state: &AppState,
    pipeline_id: &str,
    triggered_by: &str,
    inputs: &std::collections::HashMap<String, String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let pipeline = sqlx::query(
        "SELECT id, name, runner_labels, concurrency_group FROM pipelines WHERE id = ? AND is_active = 1",
    )
    .bind(pipeline_id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound("pipeline not found or inactive".into()))?;

    // Concurrency check: if any job is pending/running for this pipeline with a concurrency group set
    let concurrency_group: Option<String> = pipeline.get(3);
    if let Some(ref group) = concurrency_group {
        if !group.is_empty() {
            let running = sqlx::query(
                "SELECT COUNT(*) as cnt FROM jobs WHERE pipeline_id = ? AND status IN ('pending', 'running')",
            )
            .bind(pipeline_id)
            .fetch_one(&state.pool)
            .await?;

            let count: i64 = running.get(0);
            if count > 0 {
                return Err(AppError::Conflict(format!(
                    "concurrency group '{group}' has {count} active job(s)"
                )));
            }
        }
    }

    let job_id = Uuid::new_v4().to_string();
    let pipeline_name: String = pipeline.get(1);
    let runner_labels: String = pipeline.get(2);

    // ── Freeze snapshot ──
    // Capture the full pipeline definition so historical runs are
    // reproducible even if the pipeline is later modified or deleted.
    let tasks_snapshot = sqlx::query(
        "SELECT step_order, name, exec_type, host_id, host_names, host_labels, command_template,
                image, work_dir, retry_count, timeout_seconds, fail_strategy, step_secrets
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
        })
    })
    .collect::<Vec<_>>();

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

    let snapshot = serde_json::json!({
        "pipeline": pipeline_snapshot,
        "runner_labels": serde_json::from_str::<Vec<String>>(&runner_labels).unwrap_or_default(),
        "concurrency_group": concurrency_group,
        "tasks": tasks_snapshot,
        "env": env_snapshot,
        "inputs": inputs,
        "triggered_by": triggered_by,
        "triggered_at": chrono::Utc::now().to_rfc3339(),
    });

    let trigger_params_json = serde_json::to_string(&inputs).unwrap_or_else(|_| "{}".into());

    sqlx::query(
        "INSERT INTO jobs (id, pipeline_id, status, trigger_by, trigger_params, snapshot, created_at, updated_at)
         VALUES (?, ?, 'pending', ?, ?, ?, datetime('now'), datetime('now'))",
    )
    .bind(&job_id)
    .bind(pipeline_id)
    .bind(triggered_by)
    .bind(&trigger_params_json)
    .bind(serde_json::to_string(&snapshot).unwrap_or_else(|_| "{}".into()))
    .execute(&state.pool)
    .await?;

    tracing::info!("Pipeline triggered: {pipeline_name} → job {job_id} (by: {triggered_by})");

    Ok(Json(ApiResponse::success(serde_json::json!({
        "job_id": job_id,
        "pipeline_id": pipeline_id,
        "pipeline_name": pipeline_name,
        "status": "pending",
    }))))
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
    pool: &sqlx::SqlitePool,
    pipeline_id: &str,
    steps: &[StepDef],
) -> Result<Vec<serde_json::Value>, AppError> {
    let mut result = Vec::with_capacity(steps.len());

    for (i, step) in steps.iter().enumerate() {
        let task_id = Uuid::new_v4().to_string();
        let exec_type = step.uses.clone().unwrap_or_else(|| "local".into());
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
                    .fetch_optional(pool)
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
                    .fetch_all(pool)
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
        // Step-level secrets (comma-separated)
        let step_secrets_stored = step
            .secrets
            .as_ref()
            .map(|s| s.join(","))
            .unwrap_or_default();
        let image_stored = step
            .image
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("")
            .to_string();

        sqlx::query(
            "INSERT INTO tasks (id, pipeline_id, step_order, name, exec_type, host_id, host_names, host_labels,
             command_template, image, work_dir, retry_count, timeout_seconds, fail_strategy, step_secrets)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
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
        .bind(&fail_strategy)
        .bind(&step_secrets_stored)
        .execute(pool)
        .await?;

        result.push(serde_json::json!({
            "id": task_id,
            "step_order": i,
            "name": step.name,
            "exec_type": exec_type,
            "image": image_stored,
        }));
    }

    Ok(result)
}

fn parse_timeout_seconds(raw: Option<&str>) -> i64 {
    raw.and_then(|s| {
        let s = s.trim();
        if s.ends_with('m') {
            s[..s.len() - 1].parse::<i64>().ok().map(|v| v * 60)
        } else if s.ends_with('h') {
            s[..s.len() - 1].parse::<i64>().ok().map(|v| v * 3600)
        } else {
            s.parse::<i64>().ok()
        }
    })
    .unwrap_or(3600)
}

pub(crate) async fn insert_env_vars(
    pool: &sqlx::SqlitePool,
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
        .execute(pool)
        .await?;
    }
    Ok(())
}

/// Explicitly bind variable IDs to a pipeline (many-to-many via pipeline_vars).
pub(crate) async fn insert_pipeline_var_bindings(
    pool: &sqlx::SqlitePool,
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
        .execute(pool)
        .await?;
    }
    Ok(())
}

/// Explicitly bind secret IDs to a pipeline (many-to-many via pipeline_secrets).
pub(crate) async fn insert_pipeline_secret_bindings(
    pool: &sqlx::SqlitePool,
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
        .execute(pool)
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
    _auth: AuthUser,
    Json(req): Json<ValidateYamlRequest>,
) -> Json<ApiResponse<shared::pipeline_yaml::YamlValidationResult>> {
    let result = shared::pipeline_yaml::parse_and_validate(&req.yaml);
    Json(ApiResponse::success(result))
}

#[cfg(test)]
#[path = "tests/pipelines_list_tests.rs"]
mod tests;
