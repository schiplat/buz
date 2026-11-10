use axum::{
    extract::{Path, Query, State},
    Json,
};
use serde::Deserialize;
use shared::error::AppError;
use shared::protocol::redact_input_map;
use shared::protocol::response::ApiResponse;
use shared::rbac::Perm;
use sqlx::Row;
use std::collections::HashMap;

use crate::audit::{self, Action, AuditEvent, Resource};
use crate::middleware::{AuthUser, RunnerClaims};
use crate::rbac::{pipeline_team_id, team_in_clause};
use crate::state::AppState;
use crate::store::jobs::{cancel_job_in_tx, now_utc};

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
                p.team_id, j.verbose_logs, j.debug_unredact, j.trigger_user_id,
                j.run_number
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
                "trigger_user_id": r.get::<Option<String>, _>(16),
                "run_number": r.get::<i64, _>(17),
            })
        })
        .collect();

    Ok(Json(ApiResponse::success(jobs)))
}

// Dashboard stats endpoints ------------------------------------------------

#[derive(Debug, Deserialize, Default)]
pub struct TrendQuery {
    #[serde(default)]
    pub days: Option<i64>,
    #[serde(default)]
    pub team_id: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub struct RecentQuery {
    #[serde(default)]
    pub limit: Option<i64>,
    #[serde(default)]
    pub team_id: Option<String>,
}

/// Team scope for dashboard stats. An explicit `team_id` query param must be a
/// team the caller can view (403 otherwise); when absent, all the caller's
/// visible teams are used (platform admin = every team).
fn dashboard_team_scope(
    auth: &AuthUser,
    team_id: Option<&str>,
) -> Result<Option<(String, Vec<String>)>, AppError> {
    if let Some(tid) = team_id.map(str::trim).filter(|s| !s.is_empty()) {
        auth.check_team(tid, Perm::TeamView)?;
        return Ok(Some((
            "AND p.team_id = ?".to_string(),
            vec![tid.to_string()],
        )));
    }
    Ok(team_in_clause(auth, "p.team_id"))
}

/// Debug (unredacted) jobs are admin-only records — hidden from every stats
/// endpoint for non-admins, matching the job list/get handlers.
fn dashboard_debug_filter(auth: &AuthUser) -> &'static str {
    if auth.is_platform_admin {
        ""
    } else {
        "AND j.debug_unredact = 0"
    }
}

/// Assemble a zero-filled daily series covering the last `days` days (UTC),
/// overlaying the aggregated `counts` (day → status → count).
fn build_trend_points(
    counts: &HashMap<String, HashMap<String, i64>>,
    days: i64,
) -> Vec<serde_json::Value> {
    let today = chrono::Utc::now().date_naive();
    let mut points = Vec::with_capacity(days as usize);
    for offset in (0..days).rev() {
        let date = today - chrono::Duration::days(offset);
        let key = date.format("%Y-%m-%d").to_string();
        let day_counts = counts.get(&key).cloned().unwrap_or_default();
        let total: i64 = day_counts.values().sum();
        let success = day_counts.get("success").copied().unwrap_or(0);
        let failed = day_counts.get("failed").copied().unwrap_or(0);
        let cancelled = day_counts.get("cancelled").copied().unwrap_or(0);
        // Jobs created but not yet executed (pending / queued / waiting for
        // approval) are surfaced as their own series.
        let pending: i64 = ["pending", "queued", "waiting_approval"]
            .iter()
            .filter_map(|s| day_counts.get(*s))
            .sum();
        points.push(serde_json::json!({
            "date": key,
            "total": total,
            "success": success,
            "failed": failed,
            "cancelled": cancelled,
            "pending": pending,
            "other": total - success - failed - cancelled - pending,
        }));
    }
    points
}

/// GET /jobs/stats/trend?days=7&team_id=… — daily build counts (by status) for
/// the last 7/15/30 days, team- and role-scoped like every other job view.
pub async fn stats_trend(
    auth: AuthUser,
    State(state): State<AppState>,
    Query(q): Query<TrendQuery>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let days = q.days.unwrap_or(7).clamp(1, 90);
    let cutoff = chrono::Utc::now()
        .checked_sub_signed(chrono::Duration::days(days))
        .map(|d| d.format("%Y-%m-%d 00:00:00").to_string())
        .unwrap_or_else(|| "1970-01-01 00:00:00".to_string());
    let debug_filter = dashboard_debug_filter(&auth);
    let team_scope = dashboard_team_scope(&auth, q.team_id.as_deref())?;

    let empty = serde_json::json!({
        "days": days,
        "points": build_trend_points(&HashMap::new(), days),
    });

    let sql = match &team_scope {
        None => format!(
            "SELECT date(j.created_at) AS day, j.status, COUNT(*) AS cnt
             FROM jobs j JOIN pipelines p ON j.pipeline_id = p.id
             WHERE j.created_at >= ? {debug_filter}
             GROUP BY date(j.created_at), j.status"
        ),
        Some((clause, _)) if clause == "AND 1=0" => {
            return Ok(Json(ApiResponse::success(empty)));
        }
        Some((clause, _)) => format!(
            "SELECT date(j.created_at) AS day, j.status, COUNT(*) AS cnt
             FROM jobs j JOIN pipelines p ON j.pipeline_id = p.id
             WHERE j.created_at >= ? {clause} {debug_filter}
             GROUP BY date(j.created_at), j.status"
        ),
    };

    let mut q2 = sqlx::query(&sql).bind(&cutoff);
    if let Some((_, ids)) = &team_scope {
        for id in ids {
            q2 = q2.bind(id);
        }
    }
    let rows = q2.fetch_all(&state.pool).await?;

    let mut counts: HashMap<String, HashMap<String, i64>> = HashMap::new();
    for r in rows {
        let day: String = r.get(0);
        let status: String = r.get(1);
        let cnt: i64 = r.get(2);
        counts.entry(day).or_default().insert(status, cnt);
    }

    tracing::info!(
        user = %auth.username,
        days,
        team = ?q.team_id,
        "jobs: dashboard build trend computed",
    );

    Ok(Json(ApiResponse::success(serde_json::json!({
        "days": days,
        "points": build_trend_points(&counts, days),
    }))))
}

/// GET /jobs/recent?limit=10&team_id=… — lightweight build stream for the
/// Overview sidebar, team- and role-scoped.
pub async fn recent(
    auth: AuthUser,
    State(state): State<AppState>,
    Query(q): Query<RecentQuery>,
) -> Result<Json<ApiResponse<Vec<serde_json::Value>>>, AppError> {
    let limit = q.limit.unwrap_or(10).clamp(1, 50);
    let debug_filter = dashboard_debug_filter(&auth);
    let team_scope = dashboard_team_scope(&auth, q.team_id.as_deref())?;

    let sql = match &team_scope {
        None => format!(
            "SELECT j.id, j.pipeline_id, p.name AS pipeline_name, j.status,
                    j.trigger_by, p.team_id, j.created_at
             FROM jobs j JOIN pipelines p ON j.pipeline_id = p.id
             WHERE 1=1 {debug_filter}
             ORDER BY j.created_at DESC, j.id DESC
             LIMIT ?"
        ),
        Some((clause, _)) if clause == "AND 1=0" => {
            return Ok(Json(ApiResponse::success(vec![])));
        }
        Some((clause, _)) => format!(
            "SELECT j.id, j.pipeline_id, p.name AS pipeline_name, j.status,
                    j.trigger_by, p.team_id, j.created_at
             FROM jobs j JOIN pipelines p ON j.pipeline_id = p.id
             WHERE 1=1 {clause} {debug_filter}
             ORDER BY j.created_at DESC, j.id DESC
             LIMIT ?"
        ),
    };

    let mut q2 = sqlx::query(&sql);
    if let Some((_, ids)) = &team_scope {
        for id in ids {
            q2 = q2.bind(id);
        }
    }
    q2 = q2.bind(limit);
    let rows = q2.fetch_all(&state.pool).await?;

    let jobs: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|r| {
            serde_json::json!({
                "id": r.get::<String, _>(0),
                "pipeline_id": r.get::<String, _>(1),
                "pipeline_name": r.get::<Option<String>, _>(2),
                "status": r.get::<String, _>(3),
                "trigger_by": r.get::<String, _>(4),
                "team_id": r.get::<String, _>(5),
                "created_at": r.get::<Option<String>, _>(6),
            })
        })
        .collect();

    tracing::info!(
        user = %auth.username,
        count = jobs.len(),
        team = ?q.team_id,
        "jobs: dashboard build stream fetched",
    );

    Ok(Json(ApiResponse::success(jobs)))
}

/// Step orders whose streamed output hit the truncation marker
/// (`chunk_index % 10_000 == LOG_STEP_TRUNCATED`). Per-step 10_000-wide
/// windows make the modulo exact; negatives (runner banner) never match.
async fn truncated_steps(pool: &sqlx::SqlitePool, job_id: &str) -> Vec<i64> {
    // Written by the Runner's terminal report: job metadata, so it survives
    // job_log_retention deleting the marker chunk.
    let stored: Option<String> =
        sqlx::query_scalar("SELECT truncated_steps FROM jobs WHERE id = ?")
            .bind(job_id)
            .fetch_optional(pool)
            .await
            .ok()
            .flatten();
    if let Some(raw) = stored {
        if let Ok(mut steps) = serde_json::from_str::<Vec<i64>>(&raw) {
            if !steps.is_empty() {
                steps.sort_unstable();
                steps.dedup();
                return steps;
            }
        }
    }

    // Legacy fallback (jobs finished before the column existed): the marker
    // chunk in the step's 10_000-wide window. Correct only while logs remain.
    let mut steps: Vec<i64> = sqlx::query_scalar(
        "SELECT DISTINCT step_order FROM job_logs
         WHERE job_id = ? AND chunk_index >= 0
           AND chunk_index % 10000 = ?",
    )
    .bind(job_id)
    .bind(shared::protocol::LOG_STEP_TRUNCATED)
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    steps.sort_unstable();
    steps
}

pub async fn get_one(
    auth: AuthUser,
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let r = sqlx::query(
        "SELECT j.id, j.pipeline_id, p.name as pipeline_name, j.runner_id,
                j.status, j.trigger_by, j.trigger_params, j.snapshot, j.step_statuses, j.retry_times,
                j.start_time, j.end_time, j.created_at, j.updated_at, j.debug_unredact, j.verbose_logs,
                j.trigger_user_id, j.run_number, j.run_attempt,
                j.resource_profile, j.resource_slots, j.resource_spec, j.sandbox_instance_id,
                j.dispatch_reason
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

    // Debug (unredacted) jobs are admin-only records — non-admins must not
    // even see the metadata. 404 (not 403) so the existence of a debug job
    // is indistinguishable from a missing job, matching the `list` endpoint.
    let debug_unredact: i64 = r.get(14);
    if debug_unredact != 0 && !auth.is_platform_admin {
        return Err(AppError::NotFound("job not found".into()));
    }

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
    let approvals = list_job_approvals_json(&state.pool, &id).await?;

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
        "trigger_user_id": r.get::<Option<String>, _>(16),
        "run_number": r.get::<i64, _>(17),
        "run_attempt": r.get::<i64, _>(18),
        "team_id": team_id,
        "approvals": approvals,
        // Requested profile ('' = default) frozen at trigger; slots/spec and
        // the dispatching sandbox instance are filled at claim.
        "resource_profile": r.get::<String, _>(19),
        "resource_slots": r.get::<i64, _>(20),
        "resource_spec": serde_json::from_str::<serde_json::Value>(&r.get::<String, _>(21))
            .unwrap_or(serde_json::json!({})),
        "sandbox_instance_id": r.get::<String, _>(22),
        // Why the last poller skipped this job while pending ('' once claimed).
        "dispatch_reason": r.get::<String, _>(23),
        // Steps whose streamed output overflowed the per-step log window —
        // the UI badges them without downloading every chunk. The offset
        // (9991) is the shared LOG_STEP_TRUNCATED layout constant.
        "truncated_steps": truncated_steps(&state.pool, &id).await,
    }))))
}

pub async fn cancel(
    auth: AuthUser,
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    let ctx = sqlx::query("SELECT j.pipeline_id, p.name FROM jobs j LEFT JOIN pipelines p ON p.id = j.pipeline_id WHERE j.id = ?")
        .bind(&id)
        .fetch_optional(&state.pool)
        .await?;

    let (pipeline_id, pipeline_name) = match ctx {
        Some(row) => (
            row.get::<String, _>(0),
            row.try_get::<Option<String>, _>(1).ok().flatten(),
        ),
        None => return Err(AppError::NotFound("job not found".into())),
    };

    let team_id = pipeline_team_id(&state.pool, &pipeline_id).await?;
    auth.check_team(&team_id, Perm::PipelineWrite)?;

    let mut tx = state.pool.begin_with("BEGIN IMMEDIATE").await?;
    let Some(cancelled) = cancel_job_in_tx(&mut tx, &id, &auth.id, &auth.username).await? else {
        tx.rollback().await?;
        return Err(AppError::NotFound(
            "job not found or already finished".into(),
        ));
    };
    audit::record_in(
        &mut tx,
        AuditEvent::new(Action::JobCancel, &auth)
            .on(Resource::job(
                &id,
                pipeline_name.as_deref().unwrap_or(&pipeline_id),
            ))
            .team(Some(team_id.clone()))
            .changes(
                Some(serde_json::json!({ "status": cancelled.old_status })),
                Some(serde_json::json!({ "status": "cancelled" })),
            ),
    )
    .await?;
    tx.commit().await?;
    crate::services::poll_events::wake_all();

    tracing::info!(
        operator = %auth.username,
        job_id = %id,
        pipeline = ?pipeline_name,
        prev_status = %cancelled.old_status,
        "Job cancelled by operator"
    );

    crate::services::transition::announce_job_status(&state, &id, "cancelled", true).await;

    Ok(Json(ApiResponse::success(())))
}

#[derive(Debug, Deserialize, Default)]
pub struct ApprovalCommentRequest {
    #[serde(default)]
    pub comment: String,
}

async fn list_job_approvals_json(
    pool: &sqlx::SqlitePool,
    job_id: &str,
) -> Result<Vec<serde_json::Value>, AppError> {
    let rows = sqlx::query(
        "SELECT id, user_id, username, action, comment, created_at
         FROM job_approvals
         WHERE job_id = ?
         ORDER BY created_at ASC, id ASC",
    )
    .bind(job_id)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            serde_json::json!({
                "id": r.get::<String, _>(0),
                "user_id": r.get::<String, _>(1),
                "username": r.get::<String, _>(2),
                "action": r.get::<String, _>(3),
                "comment": r.get::<String, _>(4),
                "created_at": r.get::<Option<String>, _>(5),
            })
        })
        .collect())
}

fn broadcast_job_status(state: &AppState, job_id: &str, pipeline_id: &str, status: &str) {
    let payload = serde_json::json!({
        "id": job_id,
        "status": status,
        "end_time": if status == "cancelled" {
            Some(now_utc())
        } else {
            None
        },
    });
    state.log_broadcast.broadcast_status(
        job_id,
        pipeline_id,
        &serde_json::to_string(&payload).unwrap_or_default(),
    );
}

/// POST /jobs/{id}/approve — record an approval; promote when threshold met.
pub async fn approve(
    auth: AuthUser,
    Path(id): Path<String>,
    State(state): State<AppState>,
    body: Option<Json<ApprovalCommentRequest>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let comment = body
        .map(|Json(b)| b.comment.trim().to_string())
        .unwrap_or_default();

    tracing::info!(job_id = %id, actor = %auth.username, "environments: approve started");

    let outcome = crate::store::jobs::approve(&state.pool, &auth, &id, &comment).await?;
    broadcast_job_status(&state, &id, &outcome.pipeline_id, &outcome.new_status);

    Ok(Json(ApiResponse::success(serde_json::json!({
        "job_id": id,
        "status": outcome.new_status,
        "approvals_received": outcome.received,
        "approvals_required": outcome.required,
    }))))
}

/// POST /jobs/{id}/reject — reject a waiting_approval job → cancelled.
pub async fn reject(
    auth: AuthUser,
    Path(id): Path<String>,
    State(state): State<AppState>,
    body: Option<Json<ApprovalCommentRequest>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let comment = body
        .map(|Json(b)| b.comment.trim().to_string())
        .unwrap_or_default();

    tracing::info!(job_id = %id, actor = %auth.username, "environments: reject started");

    let pipeline_id = crate::store::jobs::job_pipeline_id(&state.pool, &id).await?;
    crate::store::jobs::reject(&state.pool, &auth, &id, &comment).await?;
    broadcast_job_status(&state, &id, &pipeline_id, "cancelled");

    Ok(Json(ApiResponse::success(serde_json::json!({
        "job_id": id,
        "status": "cancelled",
    }))))
}

/// POST /jobs/{id}/rerun — replay a terminal job from its frozen snapshot
/// (GHA re-run semantics): the same `run_number` with `run_attempt + 1`,
/// inputs and step definitions taken from the original snapshot, while the
/// CURRENT environment protection and pipeline concurrency still gate the
/// new run. Sealed `trigger_params` are copied verbatim — no decrypt /
/// re-encrypt round trip — so masking keys resolve at claim exactly as in
/// the original run. The re-run actor becomes `trigger_user_id`, so
/// `forbid_self_approve` applies to whoever clicked re-run.
pub async fn rerun(
    auth: AuthUser,
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let row = sqlx::query(
        "SELECT j.status, j.pipeline_id, j.trigger_params, j.snapshot,
                j.environment_id, j.debug_unredact, j.verbose_logs,
                j.run_number, j.run_attempt, j.resource_profile,
                p.team_id, p.name, p.is_active,
                p.concurrency_group, p.concurrency_mode, p.concurrency_max_queue
         FROM jobs j JOIN pipelines p ON p.id = j.pipeline_id
         WHERE j.id = ?",
    )
    .bind(&id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound("job not found".into()))?;

    let status: String = row.get(0);
    let pipeline_id: String = row.get(1);
    let trigger_params_raw: Option<String> = row.get(2);
    let snapshot_raw: String = row.get(3);
    let environment_id: Option<String> = row.get(4);
    let debug_unredact = row.get::<i64, _>(5) != 0;
    let verbose_logs = row.get::<i64, _>(6) != 0;
    let run_number: i64 = row.get(7);
    let run_attempt: i64 = row.get(8);
    let resource_profile: String = row.get(9);
    let team_id: String = row.get(10);
    let pipeline_name: String = row.get(11);
    let is_active = row.get::<i64, _>(12) != 0;
    let concurrency_group: Option<String> = row.get(13);
    let concurrency_mode: String = row.get(14);
    let concurrency_max_queue: i64 = row.get(15);

    // Debug (unredacted) records are admin-only end to end — a non-admin
    // must not even learn the job exists (404, matching get_one/list).
    if debug_unredact && !auth.is_platform_admin {
        return Err(AppError::NotFound("job not found".into()));
    }

    auth.check_team(&team_id, Perm::PipelineTrigger)?;

    if !matches!(
        status.as_str(),
        "success" | "failed" | "cancelled" | "timeout"
    ) {
        return Err(AppError::Conflict(format!(
            "only terminal jobs can be re-run (job is {status})"
        )));
    }
    if !is_active {
        return Err(AppError::Conflict(
            "pipeline is disabled — enable it before re-running".into(),
        ));
    }

    // Same authorization subject as trigger: the job's resolved team.
    crate::services::resource_grants::ensure_team_may_use(
        &state.pool,
        &team_id,
        resource_profile.trim(),
    )
    .await?;

    // Environment: resolve by the job's stored id so CURRENT protection
    // gates the re-run (protection belongs to the environment, not to the
    // historical snapshot). A deleted environment is a clear 404.
    let env =
        super::environments::resolve_environment(&state.pool, environment_id.as_deref()).await?;
    let job_status: &str = match env.as_ref().map(|e| &e.protection) {
        Some(p) if p.enabled => {
            auth.check_team_role(&team_id, p.min_trigger_role)?;
            "waiting_approval"
        }
        _ => {
            let mut tx = state.pool.begin_with("BEGIN IMMEDIATE").await?;
            let st = crate::store::pipelines::resolve_concurrency_status_in_tx(
                &mut tx,
                &pipeline_id,
                concurrency_group.as_deref(),
                &concurrency_mode,
                concurrency_max_queue,
            )
            .await?;
            tx.commit().await?;
            st
        }
    };

    // ── Rebuild the snapshot: fresh trigger identity and gate state;
    //    frozen tasks/inputs/labels stay untouched (reproducibility). ──
    let new_attempt = run_attempt + 1;
    let mut snapshot: serde_json::Value =
        serde_json::from_str(&snapshot_raw).unwrap_or(serde_json::json!({}));
    if let Some(map) = snapshot.as_object_mut() {
        map.insert("triggered_by".into(), serde_json::json!(&auth.username));
        map.insert(
            "triggered_at".into(),
            serde_json::json!(chrono::Utc::now().to_rfc3339()),
        );
        match &env {
            Some(e) => {
                map.insert("environment".into(), serde_json::json!(&e.slug));
                map.insert("protection".into(), e.protection.to_snapshot_json());
            }
            None => {
                map.remove("environment");
                map.remove("protection");
            }
        }
        if job_status == "waiting_approval" {
            map.insert(
                "approval".into(),
                serde_json::json!({ "required": true, "received": 0 }),
            );
        } else {
            map.remove("approval");
        }
        map.insert(
            "rerun".into(),
            serde_json::json!({
                "of": id,
                "attempt": new_attempt,
                "by": &auth.username,
            }),
        );
    }
    let snapshot_str = serde_json::to_string(&snapshot).unwrap_or_else(|_| "{}".into());

    let job_id = crate::store::jobs::insert_rerun(
        &state.pool,
        &crate::store::jobs::RerunInsert {
            source_job_id: &id,
            pipeline_id: &pipeline_id,
            pipeline_name: &pipeline_name,
            team_id: &team_id,
            job_status,
            trigger_by: &auth.username,
            trigger_user_id: &auth.id,
            trigger_params_raw: trigger_params_raw.as_deref(),
            snapshot_str: &snapshot_str,
            debug_unredact,
            verbose_logs,
            environment_id: environment_id.as_deref(),
            run_number,
            run_attempt: new_attempt,
            resource_profile: resource_profile.trim(),
        },
        &auth,
    )
    .await?;
    super::trigger_events::record_simple_trigger(
        &state.pool,
        &pipeline_id,
        "rerun",
        &auth.username,
        &job_id,
    )
    .await;

    tracing::info!(
        operator = %auth.username,
        job_id = %job_id,
        rerun_of = %id,
        run_number,
        run_attempt = new_attempt,
        "Job re-run created from snapshot"
    );

    let mut resp = serde_json::json!({
        "job_id": job_id,
        "pipeline_id": pipeline_id,
        "rerun_of": id,
        "run_number": run_number,
        "run_attempt": new_attempt,
        "status": job_status,
    });
    if job_status == "waiting_approval" {
        if let Some(map) = resp.as_object_mut() {
            map.insert("approval_required".into(), serde_json::json!(true));
        }
    }
    Ok(Json(ApiResponse::success(resp)))
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
            audit::record(
                &state.pool,
                AuditEvent::new(Action::JobDelete, &auth)
                    .on(Resource::job(
                        &id,
                        pipeline.as_deref().unwrap_or(&pipeline_id),
                    ))
                    .team(Some(team_id.clone()))
                    .before(Some(serde_json::json!({
                        "status": status,
                        "pipeline_id": pipeline_id,
                        "pipeline": pipeline,
                    }))),
            )
            .await;
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
    auth: RunnerClaims,
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let row = sqlx::query("SELECT status, runner_id FROM jobs WHERE id = ?")
        .bind(&id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(|| AppError::NotFound("job not found".into()))?;

    // Runner↔job binding: a runner may only poll jobs poll() dispatched to it.
    let owner: Option<String> = row.get(1);
    if owner.as_deref() != Some(auth.runner_id.as_str()) {
        return Err(AppError::Forbidden(
            "job is not assigned to this runner".into(),
        ));
    }

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
    auth: RunnerClaims,
    Path(job_id): Path<String>,
    State(state): State<AppState>,
    Json(req): Json<shared::protocol::UpdateJobStatusRequest>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    let outcome = crate::store::jobs::apply_runner_status(
        &state.pool,
        &auth.runner_id,
        &job_id,
        &crate::store::jobs::RunnerStatusReport {
            status: req.status.clone(),
            step_order: req.step_order,
            truncated_steps: req.truncated_steps.clone(),
        },
    )
    .await?;

    tracing::info!(
        "Job status updated: {job_id} → {} (applied={})",
        req.status,
        outcome.applied
    );

    // Per-step updates never notify (the job's own terminal transition will);
    // the broadcast payload is re-read from the row either way.
    crate::services::transition::announce_job_status(
        &state,
        &job_id,
        &req.status,
        req.step_order.is_none(),
    )
    .await;

    Ok(Json(ApiResponse::success(())))
}
