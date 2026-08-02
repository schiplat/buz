use axum::{
    extract::{Path, State},
    Json,
};
use shared::error::AppError;
use shared::protocol::redact_input_map;
use shared::protocol::response::ApiResponse;
use shared::rbac::Perm;
use sqlx::Row;
use std::collections::HashMap;

use crate::middleware::{AuthUser, RunnerClaims};
use crate::rbac::{pipeline_team_id, team_in_clause};
use crate::state::AppState;

/// Return current UTC timestamp in SQLite format ("2026-07-27 10:30:00")
fn now_utc() -> String {
    chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

/// Redact masking input values in API-facing trigger_params using snapshot.masking_input_keys.
/// Does not decrypt — sealed (`buzenc:v1:…`) or legacy plaintext values are replaced with `***`.
fn redact_trigger_params_for_api(
    trigger_params_raw: Option<&str>,
    snapshot: &serde_json::Value,
) -> Option<String> {
    let raw = trigger_params_raw?;
    let Ok(map) = serde_json::from_str::<HashMap<String, String>>(raw) else {
        return Some(raw.to_string());
    };
    let masking_keys: Vec<String> = snapshot
        .get("masking_input_keys")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    if masking_keys.is_empty() {
        return Some(raw.to_string());
    }
    let redacted = redact_input_map(&map, &masking_keys);
    Some(serde_json::to_string(&redacted).unwrap_or_else(|_| raw.to_string()))
}

// Admin-protected management handlers

pub async fn list(
    auth: AuthUser,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<Vec<serde_json::Value>>>, AppError> {
    let base = "SELECT j.id, j.pipeline_id, p.name as pipeline_name, j.runner_id,
                j.status, j.trigger_by, j.retry_times,
                j.step_statuses, j.snapshot,
                j.start_time, j.end_time, j.created_at, j.updated_at,
                p.team_id, j.verbose_logs, j.debug_unredact
         FROM jobs j
         JOIN pipelines p ON j.pipeline_id = p.id
         WHERE 1=1";

    let team_filter = team_in_clause(&auth, "p.team_id");
    // Debug (unredacted) jobs must never surface to non-admins in any job
    // listing — their logs are admin-only, so the record itself is hidden too.
    let debug_filter = if auth.is_platform_admin {
        String::new()
    } else {
        "AND j.debug_unredact = 0".to_string()
    };
    let sql = match &team_filter {
        None => format!("{base} {debug_filter} ORDER BY j.created_at DESC LIMIT 200"),
        Some((clause, _)) if clause == "AND 1=0" => {
            return Ok(Json(ApiResponse::success(vec![])));
        }
        Some((clause, _)) => {
            format!("{base} {clause} {debug_filter} ORDER BY j.created_at DESC LIMIT 200")
        }
    };

    let mut q = sqlx::query(&sql);
    if let Some((_, ids)) = &team_filter {
        for id in ids {
            q = q.bind(id);
        }
    }

    let rows = q.fetch_all(&state.pool).await?;

    let jobs: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|r| {
            let step_statuses_raw: String = r.get(7);
            let step_statuses: serde_json::Value =
                serde_json::from_str(&step_statuses_raw).unwrap_or_default();
            let snapshot_raw: String = r.get(8);
            let snapshot: serde_json::Value =
                serde_json::from_str(&snapshot_raw).unwrap_or_default();
            serde_json::json!({
                "id": r.get::<String, _>(0),
                "pipeline_id": r.get::<String, _>(1),
                "pipeline_name": r.get::<Option<String>, _>(2),
                "runner_id": r.get::<Option<String>, _>(3),
                "status": r.get::<String, _>(4),
                "trigger_by": r.get::<String, _>(5),
                "retry_times": r.get::<i64, _>(6),
                "step_statuses": step_statuses,
                "snapshot": snapshot,
                "start_time": r.get::<Option<String>, _>(9),
                "end_time": r.get::<Option<String>, _>(10),
                "created_at": r.get::<Option<String>, _>(11),
                "updated_at": r.get::<Option<String>, _>(12),
                "team_id": r.get::<String, _>(13),
                "verbose_logs": r.get::<i64, _>(14) != 0,
                "debug_unredact": r.get::<i64, _>(15) != 0,
            })
        })
        .collect();

    Ok(Json(ApiResponse::success(jobs)))
}

pub async fn get_one(
    auth: AuthUser,
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let r = sqlx::query(
        "SELECT j.id, j.pipeline_id, p.name as pipeline_name, j.runner_id,
                j.status, j.trigger_by, j.trigger_params, j.snapshot, j.step_statuses, j.retry_times,
                j.start_time, j.end_time, j.created_at, j.updated_at, j.debug_unredact, j.verbose_logs
         FROM jobs j
         LEFT JOIN pipelines p ON j.pipeline_id = p.id
         WHERE j.id = ?",
    )
    .bind(&id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound("job not found".into()))?;

    let pipeline_id: String = r.get(1);
    let team_id = pipeline_team_id(&state.pool, &pipeline_id).await?;
    auth.check_team(&team_id, Perm::TeamView)?;

    let trigger_params: Option<String> = r.get(6);
    let snapshot_raw: Option<String> = r.get(7);
    let snapshot: serde_json::Value = snapshot_raw
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(serde_json::json!({}));
    let step_statuses_raw: Option<String> = r.get(8);
    let step_statuses: serde_json::Value = step_statuses_raw
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();

    let trigger_params_safe = redact_trigger_params_for_api(trigger_params.as_deref(), &snapshot);

    Ok(Json(ApiResponse::success(serde_json::json!({
        "id": r.get::<String, _>(0),
        "pipeline_id": r.get::<String, _>(1),
        "pipeline_name": r.get::<Option<String>, _>(2),
        "runner_id": r.get::<Option<String>, _>(3),
        "status": r.get::<String, _>(4),
        "trigger_by": r.get::<String, _>(5),
        "trigger_params": trigger_params_safe,
        "snapshot": snapshot,
        "step_statuses": step_statuses,
        "retry_times": r.get::<i64, _>(9),
        "start_time": r.get::<Option<String>, _>(10),
        "end_time": r.get::<Option<String>, _>(11),
        "created_at": r.get::<Option<String>, _>(12),
        "updated_at": r.get::<Option<String>, _>(13),
        "debug_unredact": r.get::<i64, _>(14) != 0,
        "verbose_logs": r.get::<i64, _>(15) != 0,
    }))))
}

pub async fn cancel(
    auth: AuthUser,
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    // Fetch job + pipeline context before the update so we can log rich details
    let ctx = sqlx::query(
        "SELECT j.status, j.step_statuses, j.pipeline_id, p.name
         FROM jobs j
         LEFT JOIN pipelines p ON p.id = j.pipeline_id
         WHERE j.id = ?",
    )
    .bind(&id)
    .fetch_optional(&state.pool)
    .await?;

    let (old_status, step_statuses_str, pipeline_id, pipeline_name) = match ctx {
        Some(row) => {
            let s: String = row.get(0);
            let ss: String = row.get(1);
            let pid: String = row.get(2);
            let p: Option<String> = row.try_get(3).ok();
            (s, ss, pid, p)
        }
        None => return Err(AppError::NotFound("job not found".into())),
    };

    let team_id = pipeline_team_id(&state.pool, &pipeline_id).await?;
    auth.check_team(&team_id, Perm::PipelineWrite)?;

    // If a step is currently "running" in step_statuses, flip it to "cancelled"
    let mut step_statuses: Vec<serde_json::Value> =
        serde_json::from_str(&step_statuses_str).unwrap_or_default();
    let mut changed = false;
    for s in &mut step_statuses {
        if s["status"].as_str() == Some("running") {
            s["status"] = serde_json::json!("cancelled");
            s["finished_at"] = serde_json::json!(now_utc());
            changed = true;
            break; // only one step can be running at a time
        }
    }

    let result = sqlx::query(
        "UPDATE jobs SET status = 'cancelled', end_time = datetime('now'), updated_at = datetime('now')
         WHERE id = ? AND status IN ('pending', 'running')",
    )
    .bind(&id)
    .execute(&state.pool)
    .await?;

    if result.rows_affected() == 0 {
        tracing::warn!(
            operator = %auth.username,
            job_id = %id,
            pipeline = ?pipeline_name,
            prev_status = %old_status,
            "Cancel request ignored — job already in terminal state"
        );
        return Err(AppError::NotFound(
            "job not found or already finished".into(),
        ));
    }

    // Update step_statuses if we flipped a running step
    if changed {
        let updated = serde_json::to_string(&step_statuses).unwrap_or_default();
        sqlx::query("UPDATE jobs SET step_statuses = ? WHERE id = ?")
            .bind(&updated)
            .bind(&id)
            .execute(&state.pool)
            .await?;
    }

    tracing::info!(
        operator = %auth.username,
        job_id = %id,
        pipeline = ?pipeline_name,
        prev_status = %old_status,
        "Job cancelled by operator"
    );

    // Broadcast cancelled status to WebSocket subscribers
    if let Ok(Some(pid_row)) = sqlx::query("SELECT pipeline_id FROM jobs WHERE id = ?")
        .bind(&id)
        .fetch_optional(&state.pool)
        .await
    {
        let pid: String = pid_row.get(0);
        let ss = serde_json::to_string(&step_statuses).unwrap_or_default();
        let payload = serde_json::json!({
            "id": id,
            "status": "cancelled",
            "step_statuses": serde_json::from_str::<serde_json::Value>(&ss).unwrap_or_default(),
            "end_time": now_utc(),
        });
        state.log_broadcast.broadcast_status(
            &id,
            &pid,
            &serde_json::to_string(&payload).unwrap_or_default(),
        );
    }

    Ok(Json(ApiResponse::success(())))
}

pub async fn delete(
    auth: AuthUser,
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    // Fetch job context before deletion for logging
    let ctx = sqlx::query(
        "SELECT j.status, j.pipeline_id, p.name
         FROM jobs j
         LEFT JOIN pipelines p ON p.id = j.pipeline_id
         WHERE j.id = ?",
    )
    .bind(&id)
    .fetch_optional(&state.pool)
    .await?;

    match ctx {
        Some(row) => {
            let status: String = row.get(0);
            let pipeline_id: String = row.get(1);
            let pipeline: Option<String> = row.try_get(2).ok();
            let team_id = pipeline_team_id(&state.pool, &pipeline_id).await?;
            auth.check_team(&team_id, Perm::PipelineWrite)?;
            sqlx::query("DELETE FROM jobs WHERE id = ?")
                .bind(&id)
                .execute(&state.pool)
                .await?;
            tracing::info!(
                operator = %auth.username,
                job_id = %id,
                pipeline = ?pipeline,
                prev_status = %status,
                "Job deleted by operator"
            );
        }
        None => {
            tracing::warn!(
                operator = %auth.username,
                job_id = %id,
                "Delete request ignored — job not found"
            );
            return Err(AppError::NotFound("job not found".into()));
        }
    };

    Ok(Json(ApiResponse::success(())))
}

/// Runner calls this between steps to check if the job has been cancelled.
/// Returns the current status — if "cancelled" the runner should bail immediately.
pub async fn status_check(
    _auth: RunnerClaims,
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let row = sqlx::query("SELECT status FROM jobs WHERE id = ?")
        .bind(&id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(|| AppError::NotFound("job not found".into()))?;

    let status: String = row.get(0);
    Ok(Json(ApiResponse::success(serde_json::json!({
        "status": status,
    }))))
}

// Runner-protected execution handlers

pub async fn claim(
    _auth: RunnerClaims,
    State(_state): State<AppState>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    // Claim is now handled in poll() — this endpoint kept for compatibility.
    // The poll endpoint atomically updates job status from pending → running
    // and binds it to the runner, which is more efficient.
    Ok(Json(ApiResponse::success(serde_json::json!({
        "message": "Claim is handled automatically via poll endpoint"
    }))))
}

pub async fn update_status(
    _auth: RunnerClaims,
    Path(job_id): Path<String>,
    State(state): State<AppState>,
    Json(req): Json<shared::protocol::UpdateJobStatusRequest>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    // If the job has been manually cancelled via the UI, silently ignore
    // Runner status updates so the cancellation sticks.
    let row = sqlx::query("SELECT status FROM jobs WHERE id = ?")
        .bind(&job_id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(|| AppError::NotFound("job not found".into()))?;

    let current_status: String = row.get(0);

    // Terminal states win — don't let the Runner overwrite a cancellation
    if current_status == "cancelled" || current_status == "success" || current_status == "failed" {
        return Ok(Json(ApiResponse::success(())));
    }

    if let Some(step_order) = req.step_order {
        // Per-step status update
        let row2 = sqlx::query("SELECT step_statuses FROM jobs WHERE id = ?")
            .bind(&job_id)
            .fetch_optional(&state.pool)
            .await?
            .ok_or_else(|| AppError::NotFound("job not found".into()))?;

        let raw: String = row2.get(0);
        let mut statuses: Vec<serde_json::Value> = serde_json::from_str(&raw).unwrap_or_default();
        let now = now_utc();

        // Upsert
        if let Some(existing) = statuses
            .iter_mut()
            .find(|s| s["step_order"].as_i64() == Some(step_order as i64))
        {
            existing["status"] = serde_json::json!(&req.status);
            // Record started_at when a step transitions to "running"
            if req.status == "running" {
                existing["started_at"] = serde_json::json!(now);
            }
            // Record finished_at when a step reaches a terminal state
            if matches!(req.status.as_str(), "success" | "failed" | "cancelled") {
                existing["finished_at"] = serde_json::json!(now);
            }
        } else {
            let mut entry = serde_json::json!({
                "step_order": step_order,
                "status": &req.status,
            });
            if req.status == "running" {
                entry["started_at"] = serde_json::json!(now);
            }
            if matches!(req.status.as_str(), "success" | "failed" | "cancelled") {
                entry["finished_at"] = serde_json::json!(now);
            }
            statuses.push(entry);
        }

        let updated = serde_json::to_string(&statuses).unwrap_or_else(|_| "[]".into());
        sqlx::query("UPDATE jobs SET step_statuses = ?, updated_at = datetime('now') WHERE id = ?")
            .bind(&updated)
            .bind(&job_id)
            .execute(&state.pool)
            .await?;
    } else {
        // Overall status update
        let now = "datetime('now')";
        let end_expr = if matches!(
            req.status.as_str(),
            "success" | "failed" | "cancelled" | "timeout"
        ) {
            format!("end_time = {now},")
        } else {
            String::new()
        };

        let query = format!(
            "UPDATE jobs SET status = ?, {end_expr} updated_at = {now}, retry_times = retry_times + 1 WHERE id = ?"
        );

        let result = sqlx::query(&query)
            .bind(&req.status)
            .bind(&job_id)
            .execute(&state.pool)
            .await?;

        if result.rows_affected() == 0 {
            return Err(AppError::NotFound("job not found".into()));
        }

        // Safety net: if the job transitions to a terminal state, mark any
        // step still reported as "running" as "failed" so the UI never hangs.
        if matches!(req.status.as_str(), "failed") {
            let row3 = sqlx::query("SELECT step_statuses FROM jobs WHERE id = ?")
                .bind(&job_id)
                .fetch_optional(&state.pool)
                .await?;
            if let Some(row3) = row3 {
                let raw: String = row3.get(0);
                let mut step_statuses: Vec<serde_json::Value> =
                    serde_json::from_str(&raw).unwrap_or_default();
                let mut changed = false;
                for s in &mut step_statuses {
                    if s.get("status").and_then(|v| v.as_str()) == Some("running") {
                        s["status"] = serde_json::json!("failed");
                        s["finished_at"] = serde_json::json!(now_utc());
                        changed = true;
                    }
                }
                if changed {
                    let updated = serde_json::to_string(&step_statuses).unwrap_or_default();
                    sqlx::query("UPDATE jobs SET step_statuses = ? WHERE id = ?")
                        .bind(&updated)
                        .bind(&job_id)
                        .execute(&state.pool)
                        .await?;
                }
            }
        }
    }

    tracing::info!("Job status updated: {job_id} → {}", req.status);

    // Broadcast status to WebSocket subscribers
    let pipeline_id: Option<String> = sqlx::query("SELECT pipeline_id FROM jobs WHERE id = ?")
        .bind(&job_id)
        .fetch_optional(&state.pool)
        .await
        .ok()
        .flatten()
        .map(|r: sqlx::sqlite::SqliteRow| r.get(0));

    if let Some(pid) = &pipeline_id {
        // Build a minimal status snapshot for the broadcast
        if let Ok(Some(job_row)) = sqlx::query(
            "SELECT j.status, j.step_statuses, j.start_time, j.end_time
             FROM jobs j WHERE j.id = ?",
        )
        .bind(&job_id)
        .fetch_optional(&state.pool)
        .await
        {
            let status: String = job_row.get(0);
            let step_statuses_raw: String = job_row.get(1);
            let start_time: Option<String> = job_row.get(2);
            let end_time: Option<String> = job_row.get(3);
            let payload = serde_json::json!({
                "id": job_id,
                "status": status,
                "step_statuses": serde_json::from_str::<serde_json::Value>(&step_statuses_raw).unwrap_or_default(),
                "start_time": start_time,
                "end_time": end_time,
            });
            state.log_broadcast.broadcast_status(
                &job_id,
                pid,
                &serde_json::to_string(&payload).unwrap_or_default(),
            );
        }
    }

    Ok(Json(ApiResponse::success(())))
}
