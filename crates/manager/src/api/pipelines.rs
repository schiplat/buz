use axum::{Json, extract::{State, Path}};
use shared::protocol::{CreatePipelineRequest, StepDef};
use shared::protocol::response::ApiResponse;
use shared::error::AppError;
use sqlx::Row;
use uuid::Uuid;

use crate::api::AppState;
use crate::middleware::AdminClaims;

pub async fn list(
    _auth: AdminClaims,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<Vec<serde_json::Value>>>, AppError> {
    let rows = sqlx::query(
        "SELECT p.id, p.name, p.description, p.trigger_type, p.runner_labels, 
                p.timeout_seconds, p.concurrency_group, p.input_defs, p.is_active, 
                p.source, p.created_at, p.updated_at,
                j.id as latest_job_id, j.status as latest_job_status,
                j.start_time as latest_job_start
         FROM pipelines p
         LEFT JOIN (
             SELECT pipeline_id, MAX(created_at) as max_created
             FROM jobs GROUP BY pipeline_id
         ) latest ON p.id = latest.pipeline_id
         LEFT JOIN jobs j ON j.pipeline_id = latest.pipeline_id AND j.created_at = latest.max_created
         ORDER BY p.created_at DESC",
    )
    .fetch_all(&state.pool)
    .await?;

    let pipelines: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|r| {
            let labels: String = r.get(4);
            let input_defs_raw: String = r.get(7);
            let input_defs = serde_json::from_str::<serde_json::Value>(&input_defs_raw).ok();
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
                "created_at": r.get::<Option<String>, _>(10),
                "updated_at": r.get::<Option<String>, _>(11),
                "latest_job_id": r.get::<Option<String>, _>(12),
                "latest_job_status": r.get::<Option<String>, _>(13),
                "latest_job_start": r.get::<Option<String>, _>(14),
            })
        })
        .collect();

    Ok(Json(ApiResponse::success(pipelines)))
}

pub async fn create(
    _auth: AdminClaims,
    State(state): State<AppState>,
    Json(req): Json<CreatePipelineRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let pipeline_id = Uuid::new_v4().to_string();
    let labels_json = serde_json::to_string(&req.runner_labels).unwrap_or_else(|_| "[]".into());
    let input_defs_json = serde_json::to_string(&req.input_defs.unwrap_or_default())
        .unwrap_or_else(|_| "[]".into());

    sqlx::query(
        "INSERT INTO pipelines (id, name, description, trigger_type, webhook_path, cron_expr,
         runner_labels, timeout_seconds, concurrency_group, input_defs, is_active)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 1)",
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
    .bind(&input_defs_json)
    .execute(&state.pool)
    .await?;

    // Insert step definitions
    let steps = insert_steps(&state.pool, &pipeline_id, &req.steps).await?;

    // Insert env variables
    if let Some(envs) = &req.env {
        insert_env_vars(&state.pool, &pipeline_id, envs).await?;
    }

    // Insert secret references
    if let Some(secrets) = &req.secrets {
        insert_secret_refs(&state.pool, &pipeline_id, secrets).await?;
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
        "steps": steps,
    }))))
}

pub async fn get_one(
    _auth: AdminClaims,
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let r = sqlx::query(
        "SELECT id, name, description, trigger_type, webhook_path, cron_expr,
                runner_labels, timeout_seconds, source, input_defs, concurrency_group, is_active,
                created_at, updated_at
         FROM pipelines WHERE id = ?",
    )
    .bind(&id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound("pipeline not found".into()))?;

    let labels: String = r.get(6);
    let input_defs_raw: String = r.get(9);
    let input_defs = serde_json::from_str::<serde_json::Value>(&input_defs_raw).ok();

    let tasks = sqlx::query(
        "SELECT id, step_order, name, exec_type, host_id, host_labels, command_template,
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
            "work_dir": t.get::<String, _>(7),
            "retry_count": t.get::<i64, _>(8),
            "timeout_seconds": t.get::<i64, _>(9),
            "fail_strategy": t.get::<String, _>(10),
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
        "created_at": r.get::<Option<String>, _>(12),
        "updated_at": r.get::<Option<String>, _>(13),
        "tasks": tasks,
    }))))
}

pub async fn update(
    _auth: AdminClaims,
    Path(id): Path<String>,
    State(state): State<AppState>,
    Json(req): Json<CreatePipelineRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let labels_json = serde_json::to_string(&req.runner_labels).unwrap_or_else(|_| "[]".into());
    let input_defs_json = serde_json::to_string(&req.input_defs.unwrap_or_default())
        .unwrap_or_else(|_| "[]".into());

    let result = sqlx::query(
        "UPDATE pipelines SET name = ?, description = ?, trigger_type = ?, webhook_path = ?,
         cron_expr = ?, runner_labels = ?, timeout_seconds = ?, concurrency_group = ?,
         input_defs = ?, updated_at = datetime('now')
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

    let steps = insert_steps(&state.pool, &id, &req.steps).await?;

    if let Some(envs) = &req.env {
        insert_env_vars(&state.pool, &id, envs).await?;
    }
    if let Some(secrets) = &req.secrets {
        insert_secret_refs(&state.pool, &id, secrets).await?;
    }

    tracing::info!("Pipeline updated: {id}");

    Ok(Json(ApiResponse::success(serde_json::json!({
        "id": id,
        "name": req.name,
        "steps": steps,
    }))))
}

pub async fn delete(
    _auth: AdminClaims,
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<()>>, AppError> {
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
    _auth: AdminClaims,
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
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

    let action = if new_active != 0 { "activated" } else { "deactivated" };
    let name: String = row.get(0);
    tracing::info!("Pipeline {name} ({id}) {action}");

    Ok(Json(ApiResponse::success(serde_json::json!({
        "is_active": new_active != 0
    }))))
}

pub async fn trigger(
    _auth: AdminClaims,
    Path(id): Path<String>,
    State(state): State<AppState>,
    Json(body): Json<shared::protocol::TriggerPipelineRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let inputs = body.inputs;
    // Verify pipeline exists
    let pipeline = sqlx::query(
        "SELECT id, name, runner_labels, concurrency_group FROM pipelines WHERE id = ? AND is_active = 1",
    )
    .bind(&id)
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
            .bind(&id)
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
        "SELECT step_order, name, exec_type, host_id, command_template,
                work_dir, retry_count, timeout_seconds, fail_strategy
         FROM tasks WHERE pipeline_id = ? ORDER BY step_order",
    )
    .bind(&id)
    .fetch_all(&state.pool)
    .await?
    .into_iter()
    .map(|t| {
        serde_json::json!({
            "step_order": t.get::<i64, _>(0),
            "name": t.get::<String, _>(1),
            "exec_type": t.get::<String, _>(2),
            "host_id": t.get::<Option<String>, _>(3),
            "command_template": t.get::<String, _>(4),
            "work_dir": t.get::<String, _>(5),
            "retry_count": t.get::<i64, _>(6),
            "timeout_seconds": t.get::<i64, _>(7),
            "fail_strategy": t.get::<String, _>(8),
        })
    })
    .collect::<Vec<_>>();

    let env_snapshot = sqlx::query(
        "SELECT var_name, is_secret FROM variables WHERE pipeline_id = ?",
    )
    .bind(&id)
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
    .bind(&id)
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
        "triggered_by": "admin",
        "triggered_at": chrono::Utc::now().to_rfc3339(),
    });

    let trigger_params_json = serde_json::to_string(&inputs).unwrap_or_else(|_| "{}".into());

    sqlx::query(
        "INSERT INTO jobs (id, pipeline_id, status, trigger_by, trigger_params, snapshot, created_at, updated_at)
         VALUES (?, ?, 'pending', 'manual', ?, ?, datetime('now'), datetime('now'))",
    )
    .bind(&job_id)
    .bind(&id)
    .bind(&trigger_params_json)
    .bind(serde_json::to_string(&snapshot).unwrap_or_else(|_| "{}".into()))
    .execute(&state.pool)
    .await?;

    tracing::info!("Pipeline triggered: {pipeline_name} → job {job_id}");

    Ok(Json(ApiResponse::success(serde_json::json!({
        "job_id": job_id,
        "pipeline_id": id,
        "pipeline_name": pipeline_name,
        "status": "pending",
    }))))
}

// --- helpers ---

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

        // Resolve host names AND host_labels → host IDs, merge, deduplicate, store comma-separated
        let host_ids = if exec_type == "ssh" {
            let mut ids: Vec<String> = Vec::new();

            // 1) Explicit host names
            if let Some(ref hosts) = step.host {
                for name in &hosts.to_vec() {
                    let name = name.trim();
                    if name.is_empty() { continue; }
                    if let Ok(Some(row)) = sqlx::query("SELECT id FROM hosts WHERE name = ?")
                        .bind(name)
                        .fetch_optional(pool)
                        .await
                    {
                        ids.push(row.get::<String, _>(0));
                    } else {
                        tracing::warn!("Step '{}' references unknown host '{}' — will fail at dispatch", step.name, name);
                    }
                }
            }

            // 2) Host labels — match hosts whose labels JSON array contains any of the given labels
            if let Some(ref labels) = step.host_labels {
                for label in &labels.to_vec() {
                    let label = label.trim();
                    if label.is_empty() { continue; }
                    // Use json_each to find hosts whose labels array includes this exact label
                    match sqlx::query(
                        "SELECT id FROM hosts, json_each(hosts.labels) WHERE json_each.value = ?",
                    )
                    .bind(label)
                    .fetch_all(pool)
                    .await
                    {
                        Ok(rows) => {
                            if rows.is_empty() {
                                tracing::warn!("Step '{}': label '{}' matched no hosts", step.name, label);
                            }
                            for row in &rows {
                                ids.push(row.get::<String, _>(0));
                            }
                        }
                        Err(e) => {
                            tracing::error!("Step '{}': failed to query hosts by label '{}': {e}", step.name, label);
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
        let host_labels_stored = step.host_labels.as_ref().map(|l| l.join(",")).unwrap_or_default();

        sqlx::query(
            "INSERT INTO tasks (id, pipeline_id, step_order, name, exec_type, host_id, host_labels,
             command_template, work_dir, retry_count, timeout_seconds, fail_strategy)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&task_id)
        .bind(pipeline_id)
        .bind(i as i64)
        .bind(&step.name)
        .bind(&exec_type)
        .bind(&host_id_bind)
        .bind(&host_labels_stored)
        .bind(&step.run)
        .bind(step.dir.as_deref().unwrap_or(""))
        .bind(step.retry.unwrap_or(0))
        .bind(parse_timeout_seconds(step.timeout.as_deref()))
        .bind(&fail_strategy)
        .execute(pool)
        .await?;

        result.push(serde_json::json!({
            "id": task_id,
            "step_order": i,
            "name": step.name,
            "exec_type": exec_type,
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
            "INSERT INTO variables (id, pipeline_id, var_name, source_type, is_secret)
             VALUES (?, ?, ?, 'inline', 0)",
        )
        .bind(Uuid::new_v4().to_string())
        .bind(pipeline_id)
        .bind(&env.name)
        .execute(pool)
        .await?;
    }
    Ok(())
}

pub(crate) async fn insert_secret_refs(
    pool: &sqlx::SqlitePool,
    pipeline_id: &str,
    secrets: &[String],
) -> Result<(), AppError> {
    for secret in secrets {
        sqlx::query(
            "INSERT INTO variables (id, pipeline_id, var_name, source_type, is_secret)
             VALUES (?, ?, ?, 'vault', 1)",
        )
        .bind(Uuid::new_v4().to_string())
        .bind(pipeline_id)
        .bind(secret)
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
    _auth: AdminClaims,
    Json(req): Json<ValidateYamlRequest>,
) -> Json<ApiResponse<shared::pipeline_yaml::YamlValidationResult>> {
    let result = shared::pipeline_yaml::parse_and_validate(&req.yaml);
    Json(ApiResponse::success(result))
}
