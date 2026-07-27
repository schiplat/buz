use axum::{Json, extract::State, response::IntoResponse};
use shared::protocol::{RegisterRequest, RegisterResponse, HeartbeatRequest, CreateRunnerRequest};
use shared::protocol::response::ApiResponse;
use shared::auth::TokenManager;
use shared::error::AppError;
use axum::http::StatusCode;

use crate::api::AppState;
use crate::middleware::{AdminClaims, RunnerClaims};
use sqlx::Row;
use uuid::Uuid;

/// Insert a runner event (fire-and-forget; failures are silently logged).
async fn log_runner_event(pool: &sqlx::SqlitePool, runner_id: &str, event_type: &str, message: &str) {
    let id = Uuid::new_v4().to_string();
    if let Err(e) = sqlx::query(
        "INSERT INTO runner_events (id, runner_id, event_type, message) VALUES (?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(runner_id)
    .bind(event_type)
    .bind(message)
    .execute(pool)
    .await
    {
        tracing::warn!("Failed to log runner event for {runner_id}: {e}");
    }
}

pub async fn create_runner(
    _auth: AdminClaims,
    State(state): State<AppState>,
    Json(req): Json<CreateRunnerRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let id = Uuid::new_v4().to_string();
    let register_token = Uuid::new_v4().to_string();
    let labels_json = serde_json::to_string(
        &req.labels.unwrap_or_default()
    ).unwrap_or_else(|_| "[]".into());

    sqlx::query(
        "INSERT INTO runners (id, name, register_token, labels, status, version)
         VALUES (?, ?, ?, ?, 'offline', ?)",
    )
    .bind(&id)
    .bind(&req.name)
    .bind(&register_token)
    .bind(&labels_json)
    .bind(&req.version)
    .execute(&state.pool)
    .await?;

    tracing::info!("Runner created: {} ({id}) register_token={register_token}", req.name);

    log_runner_event(&state.pool, &id, "created", &format!("Runner created, awaiting registration")).await;

    Ok(Json(ApiResponse::success(serde_json::json!({
        "id": id,
        "name": req.name,
        "register_token": register_token,
    }))))
}

pub async fn register(
    State(state): State<AppState>,
    Json(req): Json<RegisterRequest>,
) -> Result<Json<ApiResponse<RegisterResponse>>, AppError> {
    let name = req.name.trim();
    let register_token = req.register_token.trim();

    // Look up the runner by name first.
    let existing = sqlx::query(
        "SELECT id, register_token, token FROM runners WHERE name = ?",
    )
    .bind(name)
    .fetch_optional(&state.pool)
    .await?;

    let runner_id: String;

    if let Some(ref row) = existing {
        let existing_token: Option<String> = row.get(2);

        if existing_token.as_ref().map_or(true, |t| t.is_empty()) {
            // First-time registration — must match the one-shot register_token.
            let stored_rt: String = row.get(1);
            if stored_rt != register_token {
                return Err(AppError::AuthFailed(
                    "invalid register token or runner name".into(),
                ));
            }
        }
        // else: re-registration — runner already has a JWT; allow without re-checking register_token.

        runner_id = row.get(0);
    } else {
        return Err(AppError::AuthFailed(
            "invalid register token or runner name".into(),
        ));
    }

    let token_mgr = TokenManager::new(&state.config.auth.jwt_secret);
    let runner_jwt = token_mgr
        .create_token(
            &runner_id,
            shared::auth::Role::Runner,
            state.config.auth.runner_register_token_ttl_hours,
        )
        .map_err(|e| AppError::Internal(format!("token generation failed: {e}")))?;

    let labels_json = serde_json::to_string(&req.labels).unwrap_or_else(|_| "[]".into());

    sqlx::query(
        "UPDATE runners SET token = ?, register_token = '', status = 'online', labels = ?, last_seen = datetime('now'), updated_at = datetime('now') WHERE id = ?",
    )
    .bind(&runner_jwt)
    .bind(&labels_json)
    .bind(&runner_id)
    .execute(&state.pool)
    .await?;

    tracing::info!("Runner registered: {} ({runner_id})", name);

    log_runner_event(&state.pool, &runner_id, "registered",
        &format!("Runner registered (version: {}, labels: {:?})", req.version.unwrap_or_default(), req.labels)).await;

    Ok(Json(ApiResponse::success(RegisterResponse {
        runner_id,
        token: runner_jwt,
    })))
}

pub async fn heartbeat(
    auth: RunnerClaims,
    State(state): State<AppState>,
    Json(req): Json<HeartbeatRequest>,
) -> Result<impl IntoResponse, AppError> {
    let system_info_json = req
        .system_info
        .as_ref()
        .and_then(|si| serde_json::to_string(si).ok())
        .unwrap_or_else(|| "{}".into());

    sqlx::query(
        "UPDATE runners SET status = ?, system_info = ?, last_seen = datetime('now'), updated_at = datetime('now') WHERE id = ?",
    )
    .bind(&req.status)
    .bind(&system_info_json)
    .bind(&auth.runner_id)
    .execute(&state.pool)
    .await?;

    Ok((StatusCode::OK, Json(ApiResponse::success(()))))
}

pub async fn poll(
    auth: RunnerClaims,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<Option<serde_json::Value>>>, AppError> {
    // Reject frozen runners — they should not be dispatched work
    let frozen: bool = sqlx::query_scalar(
        "SELECT is_frozen FROM runners WHERE id = ?",
    )
    .bind(&auth.runner_id)
    .fetch_one(&state.pool)
    .await
    .map(|v: i64| v != 0)
    .unwrap_or(false);

    if frozen {
        tracing::debug!("Runner {} is frozen — skipping poll", auth.runner_id);
        return Ok(Json(ApiResponse::success(None)));
    }

    // Find a pending job whose pipeline's runner_labels match this runner's labels.
    // For Phase 1 simplicity: dispatch any pending job and bind it to this runner.
    //
    // Strategy:
    // 1. Pick the oldest pending job
    // 2. Try to claim it atomically (UPDATE WHERE status='pending')
    // 3. Return the job with its tasks

    let claimed = sqlx::query(
        "UPDATE jobs SET status = 'running', runner_id = ?, start_time = datetime('now'),
         updated_at = datetime('now')
         WHERE id = (
             SELECT id FROM jobs WHERE status = 'pending'
             ORDER BY created_at ASC LIMIT 1
         )
         RETURNING id, pipeline_id, trigger_params",
    )
    .bind(&auth.runner_id)
    .fetch_optional(&state.pool)
    .await?;

    let Some(row) = claimed else {
        // No pending job — return empty
        return Ok(Json(ApiResponse::success(None)));
    };

    let job_id: String = row.get(0);
    let pipeline_id: String = row.get(1);
    let trigger_params_raw: Option<String> = row.get(2);
    let trigger_params: serde_json::Value = trigger_params_raw
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(serde_json::json!({}));

    // Fetch tasks for this pipeline
    let task_rows = sqlx::query(
        "SELECT id, step_order, name, exec_type, host_id, command_template,
                work_dir, retry_count, timeout_seconds, fail_strategy
         FROM tasks WHERE pipeline_id = ? ORDER BY step_order",
    )
    .bind(&pipeline_id)
    .fetch_all(&state.pool)
    .await?;

    let mut tasks: Vec<serde_json::Value> = Vec::new();
    for t in &task_rows {
        let exec_type: String = t.get(3);
        let host_id_str: String = t.get(4);

        let mut task_json = serde_json::json!({
            "id": t.get::<String, _>(0),
            "step_order": t.get::<i64, _>(1),
            "name": t.get::<String, _>(2),
            "exec_type": &exec_type,
            "command_template": t.get::<String, _>(5),
            "work_dir": t.get::<String, _>(6),
            "retry_count": t.get::<i64, _>(7),
            "timeout_seconds": t.get::<i64, _>(8),
            "fail_strategy": t.get::<String, _>(9),
        });

        // For SSH steps, resolve host IDs → connection details
        if exec_type == "ssh" {
            let host_ids: Vec<&str> = host_id_str.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()).collect();

            if host_ids.len() == 1 {
                // Single host — return as object (backward-compatible)
                if let Ok(Some(host_row)) = sqlx::query(
                    "SELECT host, port, username, credential_type, credential_ref
                     FROM hosts WHERE id = ?",
                )
                .bind(host_ids[0])
                .fetch_optional(&state.pool)
                .await
                {
                    task_json["host"] = serde_json::json!({
                        "host": host_row.get::<String, _>(0),
                        "port": host_row.get::<i64, _>(1) as u16,
                        "username": host_row.get::<String, _>(2),
                        "credential_type": host_row.get::<String, _>(3),
                        "credential_ref": host_row.get::<String, _>(4),
                    });
                }
            } else if host_ids.len() > 1 {
                // Multiple hosts — return as array
                let mut host_list: Vec<serde_json::Value> = Vec::new();
                for hid in &host_ids {
                    if let Ok(Some(host_row)) = sqlx::query(
                        "SELECT host, port, username, credential_type, credential_ref
                         FROM hosts WHERE id = ?",
                    )
                    .bind(*hid)
                    .fetch_optional(&state.pool)
                    .await
                    {
                        host_list.push(serde_json::json!({
                            "host": host_row.get::<String, _>(0),
                            "port": host_row.get::<i64, _>(1) as u16,
                            "username": host_row.get::<String, _>(2),
                            "credential_type": host_row.get::<String, _>(3),
                            "credential_ref": host_row.get::<String, _>(4),
                        }));
                    }
                }
                task_json["host"] = serde_json::Value::Array(host_list);
            } else {
                // No hosts resolved from the database
                let task_name = task_json["name"].as_str().unwrap_or("unnamed");
                tracing::warn!(
                    "SSH step '{task_name}': host_id '{}' resolved to zero hosts in the database",
                    host_id_str
                );
                task_json["host"] = serde_json::Value::Array(vec![]);
            }
        }

        tasks.push(task_json);
    }

    tracing::info!("Job dispatched: {job_id} → runner {}", auth.runner_id);

    log_runner_event(&state.pool, &auth.runner_id, "job_dispatched",
        &format!("Job {job_id} claimed, pipeline={pipeline_id}, {} steps", tasks.len())).await;

    Ok(Json(ApiResponse::success(Some(serde_json::json!({
        "job_id": job_id,
        "pipeline_id": pipeline_id,
        "tasks": tasks,
        "inputs": trigger_params,
    })))))
}

pub async fn list_runners(
    _auth: AdminClaims,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<Vec<serde_json::Value>>>, AppError> {
    let rows = sqlx::query(
        "SELECT id, name, labels, status, version, system_info, last_seen, is_frozen, created_at, updated_at FROM runners ORDER BY created_at DESC",
    )
    .fetch_all(&state.pool)
    .await?;

    let runners: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|r| {
            let labels: String = r.get(2);
            let system_info_raw: String = r.get(5);
            let system_info = serde_json::from_str::<serde_json::Value>(&system_info_raw).ok();
            serde_json::json!({
                "id": r.get::<String, _>(0),
                "name": r.get::<String, _>(1),
                "labels": serde_json::from_str::<Vec<String>>(&labels).unwrap_or_default(),
                "status": r.get::<String, _>(3),
                "version": r.get::<Option<String>, _>(4),
                "system_info": system_info,
                "last_seen": r.get::<Option<String>, _>(6),
                "is_frozen": r.get::<i64, _>(7) != 0,
                "created_at": r.get::<Option<String>, _>(8),
                "updated_at": r.get::<Option<String>, _>(9),
            })
        })
        .collect();

    Ok(Json(ApiResponse::success(runners)))
}

pub async fn get_runner(
    _auth: AdminClaims,
    axum::extract::Path(id): axum::extract::Path<String>,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let r = sqlx::query(
        "SELECT id, name, labels, status, version, system_info, last_seen, register_token, created_at, updated_at FROM runners WHERE id = ?",
    )
    .bind(&id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound("runner not found".into()))?;

    let labels: String = r.get(2);
    let system_info_raw: String = r.get(5);
    let system_info = serde_json::from_str::<serde_json::Value>(&system_info_raw).ok();
    Ok(Json(ApiResponse::success(serde_json::json!({
        "id": r.get::<String, _>(0),
        "name": r.get::<String, _>(1),
        "labels": serde_json::from_str::<Vec<String>>(&labels).unwrap_or_default(),
        "status": r.get::<String, _>(3),
        "version": r.get::<Option<String>, _>(4),
        "system_info": system_info,
        "last_seen": r.get::<Option<String>, _>(6),
        "register_token": r.get::<Option<String>, _>(7),
        "created_at": r.get::<Option<String>, _>(8),
        "updated_at": r.get::<Option<String>, _>(9),
    }))))
}

pub async fn delete_runner(
    _auth: AdminClaims,
    axum::extract::Path(id): axum::extract::Path<String>,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    let result = sqlx::query("DELETE FROM runners WHERE id = ?")
        .bind(&id)
        .execute(&state.pool)
        .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("runner not found".into()));
    }

    tracing::info!("Runner deleted: {id}");
    Ok(Json(ApiResponse::success(())))
}

pub async fn toggle_freeze(
    _auth: AdminClaims,
    axum::extract::Path(id): axum::extract::Path<String>,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let row = sqlx::query("SELECT name, is_frozen FROM runners WHERE id = ?")
        .bind(&id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(|| AppError::NotFound("runner not found".into()))?;

    let current_frozen: i64 = row.get(1);
    let new_frozen = if current_frozen != 0 { 0 } else { 1 };

    sqlx::query("UPDATE runners SET is_frozen = ?, updated_at = datetime('now') WHERE id = ?")
        .bind(new_frozen)
        .bind(&id)
        .execute(&state.pool)
        .await?;

    let action = if new_frozen != 0 { "frozen" } else { "unfrozen" };
    let name: String = row.get(0);
    tracing::info!("Runner {name} ({id}) {action}");

    log_runner_event(&state.pool, &id, action, &format!("Runner {action}")).await;

    Ok(Json(ApiResponse::success(serde_json::json!({
        "is_frozen": new_frozen != 0
    }))))
}

pub async fn get_events(
    _auth: AdminClaims,
    axum::extract::Path(id): axum::extract::Path<String>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    // Verify runner exists
    let _ = sqlx::query("SELECT id FROM runners WHERE id = ?")
        .bind(&id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(|| AppError::NotFound("runner not found".into()))?;

    let page: i64 = params.get("page").and_then(|p| p.parse().ok()).unwrap_or(1);
    let page_size: i64 = params.get("page_size").and_then(|s| s.parse().ok()).unwrap_or(20).clamp(1, 100);
    let offset = (page - 1) * page_size;

    // Get total count
    let total: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM runner_events WHERE runner_id = ?"
    )
    .bind(&id)
    .fetch_one(&state.pool)
    .await?;

    let rows = sqlx::query(
        "SELECT id, event_type, message, created_at
         FROM runner_events
         WHERE runner_id = ?
         ORDER BY created_at DESC
         LIMIT ? OFFSET ?",
    )
    .bind(&id)
    .bind(page_size)
    .bind(offset)
    .fetch_all(&state.pool)
    .await?;

    let events: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|r| {
            serde_json::json!({
                "id": r.get::<String, _>(0),
                "event_type": r.get::<String, _>(1),
                "message": r.get::<String, _>(2),
                "created_at": r.get::<Option<String>, _>(3),
            })
        })
        .collect();

    Ok(Json(ApiResponse::success(serde_json::json!({
        "items": events,
        "total": total,
        "page": page,
        "page_size": page_size,
        "total_pages": ((total as f64) / (page_size as f64)).ceil() as i64,
    }))))
}
