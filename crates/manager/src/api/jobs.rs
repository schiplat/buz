use axum::{
    extract::{Path, Query, State},
    Json,
};
use serde::Deserialize;
use shared::error::AppError;
use shared::protocol::redact_input_map;
use shared::protocol::response::ApiResponse;
use shared::rbac::{Perm, TeamRole};
use sqlx::Row;
use std::collections::HashMap;
use uuid::Uuid;

use crate::audit::{self, AuditEvent};
use crate::middleware::{AuthUser, RunnerClaims};
use crate::rbac::{pipeline_team_id, team_in_clause};
use crate::state::AppState;

/// Return current UTC timestamp in SQLite format ("2026-07-27 10:30:00")
pub(crate) fn now_utc() -> String {
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

pub async fn get_one(
    auth: AuthUser,
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let r = sqlx::query(
        "SELECT j.id, j.pipeline_id, p.name as pipeline_name, j.runner_id,
                j.status, j.trigger_by, j.trigger_params, j.snapshot, j.step_statuses, j.retry_times,
                j.start_time, j.end_time, j.created_at, j.updated_at, j.debug_unredact, j.verbose_logs,
                j.trigger_user_id, j.run_number, j.run_attempt
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
    tx.commit().await?;

    tracing::info!(
        operator = %auth.username,
        job_id = %id,
        pipeline = ?pipeline_name,
        prev_status = %cancelled.old_status,
        "Job cancelled by operator"
    );

    // Non-terminal → cancelled: async notify (only if channel `on` includes cancelled).
    crate::services::notify::spawn_job_notify(
        state.pool.clone(),
        state.config.auth.jwt_secret.clone(),
        state.config.server.public_base_url.clone(),
        id.clone(),
        "cancelled".to_string(),
    );

    // Broadcast cancelled status to WebSocket subscribers
    if let Ok(Some(pid_row)) =
        sqlx::query("SELECT pipeline_id, debug_unredact FROM jobs WHERE id = ?")
            .bind(&id)
            .fetch_optional(&state.pool)
            .await
    {
        let pid: String = pid_row.get(0);
        let debug_unredact: i64 = pid_row.get(1);
        let ss = serde_json::to_string(&cancelled.step_statuses).unwrap_or_default();
        let payload = serde_json::json!({
            "id": id,
            "status": "cancelled",
            "step_statuses": serde_json::from_str::<serde_json::Value>(&ss).unwrap_or_default(),
            "end_time": now_utc(),
            "debug_unredact": debug_unredact != 0,
        });
        state.log_broadcast.broadcast_status(
            &id,
            &pid,
            &serde_json::to_string(&payload).unwrap_or_default(),
        );
    }

    Ok(Json(ApiResponse::success(())))
}

/// A job flipped to `cancelled` inside the caller's transaction.
pub(crate) struct CancelledJob {
    pub old_status: String,
    pub step_statuses: Vec<serde_json::Value>,
}

/// Transaction-internal core of job cancellation: flips one non-terminal job
/// (`pending` / `running` / `queued` / `waiting_approval`) to `cancelled`,
/// marks the currently running step cancelled and records the approval
/// outcome for `waiting_approval` jobs. Returns `None` when the job is
/// missing or already terminal, so batch callers can skip instead of fail.
/// The caller owns the transaction (BEGIN IMMEDIATE) and handles post-commit
/// notify + WebSocket broadcast.
pub(crate) async fn cancel_job_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    job_id: &str,
    operator_id: &str,
    operator_username: &str,
) -> Result<Option<CancelledJob>, AppError> {
    let current = sqlx::query("SELECT status, step_statuses, snapshot FROM jobs WHERE id = ?")
        .bind(job_id)
        .fetch_optional(&mut **tx)
        .await?;
    let Some(current) = current else {
        return Ok(None);
    };
    let old_status: String = current.get(0);
    if !matches!(
        old_status.as_str(),
        "pending" | "running" | "queued" | "waiting_approval"
    ) {
        return Ok(None);
    }
    let step_statuses_str: String = current.get(1);
    let mut snapshot: serde_json::Value =
        serde_json::from_str(&current.get::<String, _>(2)).unwrap_or(serde_json::json!({}));

    // If a step is currently "running" in step_statuses, flip it to "cancelled".
    let mut step_statuses: Vec<serde_json::Value> =
        serde_json::from_str(&step_statuses_str).unwrap_or_default();
    for s in &mut step_statuses {
        if s["status"].as_str() == Some("running") {
            s["status"] = serde_json::json!("cancelled");
            s["finished_at"] = serde_json::json!(now_utc());
            break;
        }
    }
    if old_status == "waiting_approval" {
        let received = snapshot
            .get("approval")
            .and_then(|approval| approval.get("received"))
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0);
        set_snapshot_approval(&mut snapshot, received, Some("cancelled"));
    }

    let result = sqlx::query(
        "UPDATE jobs SET status = 'cancelled', step_statuses = ?, snapshot = ?,
         end_time = datetime('now'), updated_at = datetime('now')
         WHERE id = ? AND status IN ('pending', 'running', 'queued', 'waiting_approval')",
    )
    .bind(serde_json::to_string(&step_statuses).unwrap_or_else(|_| "[]".into()))
    .bind(serde_json::to_string(&snapshot).unwrap_or_else(|_| "{}".into()))
    .bind(job_id)
    .execute(&mut **tx)
    .await?;

    if result.rows_affected() == 0 {
        return Ok(None);
    }
    if old_status == "waiting_approval" {
        sqlx::query(
            "INSERT OR IGNORE INTO job_approvals (id, job_id, user_id, username, action, comment)
             VALUES (?, ?, ?, ?, 'cancel', '')",
        )
        .bind(Uuid::new_v4().to_string())
        .bind(job_id)
        .bind(operator_id)
        .bind(operator_username)
        .execute(&mut **tx)
        .await?;
    }
    Ok(Some(CancelledJob {
        old_status,
        step_statuses,
    }))
}

#[derive(Debug, Deserialize, Default)]
pub struct ApprovalCommentRequest {
    #[serde(default)]
    pub comment: String,
}

fn parse_min_approve_role(snapshot: &serde_json::Value) -> TeamRole {
    snapshot
        .get("protection")
        .and_then(|p| p.get("min_approve_role"))
        .and_then(|v| v.as_str())
        .and_then(TeamRole::parse)
        .unwrap_or(TeamRole::Maintainer)
}

fn protection_require_approvals(snapshot: &serde_json::Value) -> i64 {
    snapshot
        .get("protection")
        .and_then(|p| p.get("require_approvals"))
        .and_then(|v| v.as_i64())
        .unwrap_or(1)
        .max(1)
}

fn protection_forbid_self_approve(snapshot: &serde_json::Value) -> bool {
    snapshot
        .get("protection")
        .and_then(|p| p.get("forbid_self_approve"))
        .and_then(|v| v.as_bool())
        .unwrap_or(true)
}

fn set_snapshot_approval(snapshot: &mut serde_json::Value, received: i64, outcome: Option<&str>) {
    let mut approval = serde_json::json!({
        "required": true,
        "received": received,
    });
    if let Some(o) = outcome {
        if let Some(map) = approval.as_object_mut() {
            map.insert("outcome".into(), serde_json::json!(o));
        }
    }
    if let Some(map) = snapshot.as_object_mut() {
        map.insert("approval".into(), approval);
    }
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

    let row = sqlx::query(
        "SELECT j.status, j.pipeline_id, j.snapshot, j.trigger_user_id, p.team_id, p.name,
                p.concurrency_group, p.concurrency_mode, p.concurrency_max_queue
         FROM jobs j
         JOIN pipelines p ON p.id = j.pipeline_id
         WHERE j.id = ?",
    )
    .bind(&id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound("job not found".into()))?;

    let status: String = row.get(0);
    if status != "waiting_approval" {
        return Err(AppError::Conflict(format!(
            "job is not waiting for approval (status={status})"
        )));
    }

    let pipeline_id: String = row.get(1);
    let snapshot_raw: String = row.get(2);
    let trigger_user_id: Option<String> = row.get(3);
    let team_id: String = row.get(4);
    let pipeline_name: String = row.get(5);
    let concurrency_group: Option<String> = row.get(6);
    let concurrency_mode: String = row.get(7);
    let concurrency_max_queue: i64 = row.get(8);

    let mut snapshot: serde_json::Value =
        serde_json::from_str(&snapshot_raw).unwrap_or(serde_json::json!({}));
    let min_approve = parse_min_approve_role(&snapshot);
    auth.check_team_role(&team_id, min_approve)?;

    if !auth.is_platform_admin
        && protection_forbid_self_approve(&snapshot)
        && trigger_user_id.as_deref() == Some(auth.id.as_str())
    {
        return Err(AppError::Unauthorized(
            "self-approval is forbidden for this environment".into(),
        ));
    }

    let approval_id = Uuid::new_v4().to_string();
    let mut approval_tx = state.pool.begin_with("BEGIN IMMEDIATE").await?;
    let current_status: Option<String> = sqlx::query_scalar("SELECT status FROM jobs WHERE id = ?")
        .bind(&id)
        .fetch_optional(&mut *approval_tx)
        .await?;
    if current_status.as_deref() != Some("waiting_approval") {
        approval_tx.rollback().await?;
        return Err(AppError::Conflict(
            "job is no longer waiting for approval".into(),
        ));
    }
    let insert = sqlx::query(
        "INSERT INTO job_approvals (id, job_id, user_id, username, action, comment)
         VALUES (?, ?, ?, ?, 'approve', ?)",
    )
    .bind(&approval_id)
    .bind(&id)
    .bind(&auth.id)
    .bind(&auth.username)
    .bind(&comment)
    .execute(&mut *approval_tx)
    .await;

    if let Err(e) = insert {
        if let sqlx::Error::Database(db) = &e {
            if db.message().contains("UNIQUE") {
                return Err(AppError::Conflict(
                    "you have already approved this job".into(),
                ));
            }
        }
        return Err(e.into());
    }

    let received: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM job_approvals WHERE job_id = ? AND action = 'approve'",
    )
    .bind(&id)
    .fetch_one(&mut *approval_tx)
    .await?;

    let required = protection_require_approvals(&snapshot);
    let mut new_status = status.clone();

    if received >= required {
        match super::pipelines::resolve_concurrency_status_in_tx(
            &mut approval_tx,
            &pipeline_id,
            concurrency_group.as_deref(),
            &concurrency_mode,
            concurrency_max_queue,
        )
        .await
        {
            Ok(next) => {
                set_snapshot_approval(&mut snapshot, received, Some("approved"));
                let snapshot_str = serde_json::to_string(&snapshot).unwrap_or_default();
                let result = sqlx::query(
                    "UPDATE jobs SET status = ?, snapshot = ?, updated_at = datetime('now')
                     WHERE id = ? AND status = 'waiting_approval'",
                )
                .bind(next)
                .bind(&snapshot_str)
                .bind(&id)
                .execute(&mut *approval_tx)
                .await?;
                if result.rows_affected() == 0 {
                    approval_tx.rollback().await?;
                    return Err(AppError::Conflict(
                        "job is no longer waiting for approval".into(),
                    ));
                }
                approval_tx.commit().await?;
                new_status = next.to_string();
                broadcast_job_status(&state, &id, &pipeline_id, next);
            }
            Err(AppError::Conflict(msg)) => {
                // Keep waiting_approval; approval is recorded but concurrency blocked.
                set_snapshot_approval(&mut snapshot, received, None);
                let snapshot_str = serde_json::to_string(&snapshot).unwrap_or_default();
                let result = sqlx::query(
                    "UPDATE jobs SET snapshot = ?, updated_at = datetime('now') \
                     WHERE id = ? AND status = 'waiting_approval'",
                )
                .bind(&snapshot_str)
                .bind(&id)
                .execute(&mut *approval_tx)
                .await?;
                if result.rows_affected() == 0 {
                    approval_tx.rollback().await?;
                    return Err(AppError::Conflict(
                        "job is no longer waiting for approval".into(),
                    ));
                }
                approval_tx.commit().await?;
                tracing::warn!(
                    job_id = %id,
                    pipeline = %pipeline_name,
                    error = %msg,
                    "environments: approve recorded but concurrency blocked"
                );
                return Err(AppError::Conflict(msg));
            }
            Err(e) => return Err(e),
        }
    } else {
        set_snapshot_approval(&mut snapshot, received, None);
        let snapshot_str = serde_json::to_string(&snapshot).unwrap_or_default();
        let result = sqlx::query(
            "UPDATE jobs SET snapshot = ?, updated_at = datetime('now') \
             WHERE id = ? AND status = 'waiting_approval'",
        )
        .bind(&snapshot_str)
        .bind(&id)
        .execute(&mut *approval_tx)
        .await?;
        if result.rows_affected() == 0 {
            approval_tx.rollback().await?;
            return Err(AppError::Conflict(
                "job is no longer waiting for approval".into(),
            ));
        }
        approval_tx.commit().await?;
    }

    audit::record(
        &state.pool,
        AuditEvent::from_auth(&auth, "job.approve")
            .resource("job", &id, &pipeline_name)
            .team(Some(team_id))
            .after(Some(serde_json::json!({
                "received": received,
                "required": required,
                "status": new_status,
            }))),
    )
    .await;

    tracing::info!(
        job_id = %id,
        actor = %auth.username,
        received,
        required,
        status = %new_status,
        "environments: approve complete"
    );

    Ok(Json(ApiResponse::success(serde_json::json!({
        "job_id": id,
        "status": new_status,
        "approvals_received": received,
        "approvals_required": required,
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

    let row = sqlx::query(
        "SELECT j.status, j.pipeline_id, j.snapshot, j.trigger_user_id, p.team_id, p.name
         FROM jobs j
         JOIN pipelines p ON p.id = j.pipeline_id
         WHERE j.id = ?",
    )
    .bind(&id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound("job not found".into()))?;

    let status: String = row.get(0);
    if status != "waiting_approval" {
        return Err(AppError::Conflict(format!(
            "job is not waiting for approval (status={status})"
        )));
    }

    let pipeline_id: String = row.get(1);
    let snapshot_raw: String = row.get(2);
    let trigger_user_id: Option<String> = row.get(3);
    let team_id: String = row.get(4);
    let pipeline_name: String = row.get(5);

    let snapshot: serde_json::Value =
        serde_json::from_str(&snapshot_raw).unwrap_or(serde_json::json!({}));
    let min_approve = parse_min_approve_role(&snapshot);
    auth.check_team_role(&team_id, min_approve)?;

    if !auth.is_platform_admin
        && protection_forbid_self_approve(&snapshot)
        && trigger_user_id.as_deref() == Some(auth.id.as_str())
    {
        return Err(AppError::Unauthorized(
            "self-approval is forbidden for this environment".into(),
        ));
    }

    let mut reject_tx = state.pool.begin_with("BEGIN IMMEDIATE").await?;
    let current = sqlx::query("SELECT status, snapshot FROM jobs WHERE id = ?")
        .bind(&id)
        .fetch_optional(&mut *reject_tx)
        .await?
        .ok_or_else(|| AppError::NotFound("job not found".into()))?;
    let current_status: Option<String> = current.get(0);
    if current_status.as_deref() != Some("waiting_approval") {
        reject_tx.rollback().await?;
        return Err(AppError::Conflict(
            "job is no longer waiting for approval".into(),
        ));
    }
    // Re-read the snapshot inside the transaction: an approval recorded after
    // our first read must not be clobbered by the reject.
    let mut snapshot: serde_json::Value =
        serde_json::from_str(&current.get::<String, _>(1)).unwrap_or(serde_json::json!({}));
    let received: i64 = snapshot
        .get("approval")
        .and_then(|approval| approval.get("received"))
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0);
    set_snapshot_approval(&mut snapshot, received, Some("rejected"));
    let snapshot_str = serde_json::to_string(&snapshot).unwrap_or_default();
    let result = sqlx::query(
        "UPDATE jobs SET status = 'cancelled', snapshot = ?,
                end_time = datetime('now'), updated_at = datetime('now')
         WHERE id = ? AND status = 'waiting_approval'",
    )
    .bind(&snapshot_str)
    .bind(&id)
    .execute(&mut *reject_tx)
    .await?;
    if result.rows_affected() == 0 {
        reject_tx.rollback().await?;
        return Err(AppError::Conflict(
            "job is no longer waiting for approval".into(),
        ));
    }

    let approval_id = Uuid::new_v4().to_string();
    let insert = sqlx::query(
        "INSERT INTO job_approvals (id, job_id, user_id, username, action, comment)
         VALUES (?, ?, ?, ?, 'reject', ?)",
    )
    .bind(&approval_id)
    .bind(&id)
    .bind(&auth.id)
    .bind(&auth.username)
    .bind(&comment)
    .execute(&mut *reject_tx)
    .await;
    if let Err(e) = insert {
        if let sqlx::Error::Database(db) = &e {
            if db.message().contains("UNIQUE") {
                reject_tx.rollback().await?;
                return Err(AppError::Conflict(
                    "you have already acted on this job".into(),
                ));
            }
        }
        return Err(e.into());
    }
    reject_tx.commit().await?;

    broadcast_job_status(&state, &id, &pipeline_id, "cancelled");

    audit::record(
        &state.pool,
        AuditEvent::from_auth(&auth, "job.reject")
            .resource("job", &id, &pipeline_name)
            .team(Some(team_id))
            .after(Some(
                serde_json::json!({ "status": "cancelled", "outcome": "rejected" }),
            )),
    )
    .await;

    tracing::info!(
        job_id = %id,
        actor = %auth.username,
        "environments: reject complete"
    );

    Ok(Json(ApiResponse::success(serde_json::json!({
        "job_id": id,
        "status": "cancelled",
    }))))
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
    let mut tx = state.pool.begin_with("BEGIN IMMEDIATE").await?;
    let row = sqlx::query("SELECT status FROM jobs WHERE id = ?")
        .bind(&job_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| AppError::NotFound("job not found".into()))?;

    let current_status: String = row.get(0);

    // Terminal states win — don't let the Runner overwrite a cancellation or completion.
    if matches!(
        current_status.as_str(),
        "cancelled" | "success" | "failed" | "timeout"
    ) {
        tx.rollback().await?;
        return Ok(Json(ApiResponse::success(())));
    }

    if let Some(step_order) = req.step_order {
        // Per-step status update
        let row2 = sqlx::query("SELECT step_statuses FROM jobs WHERE id = ?")
            .bind(&job_id)
            .fetch_optional(&mut *tx)
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
            .execute(&mut *tx)
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
            "UPDATE jobs SET status = ?, {end_expr} updated_at = {now}, retry_times = retry_times + 1 \
             WHERE id = ? AND status NOT IN ('cancelled', 'success', 'failed', 'timeout')"
        );

        let result = sqlx::query(&query)
            .bind(&req.status)
            .bind(&job_id)
            .execute(&mut *tx)
            .await?;

        if result.rows_affected() == 0 {
            tx.rollback().await?;
            return Ok(Json(ApiResponse::success(())));
        }

        // Safety net: if the job transitions to a terminal state, mark any
        // step still reported as "running" as "failed" so the UI never hangs.
        if matches!(req.status.as_str(), "failed") {
            let row3 = sqlx::query("SELECT step_statuses FROM jobs WHERE id = ?")
                .bind(&job_id)
                .fetch_optional(&mut *tx)
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
                        .execute(&mut *tx)
                        .await?;
                }
            }
        }

        // Non-terminal → terminal: async notify (failures log-only).
        if matches!(
            req.status.as_str(),
            "success" | "failed" | "cancelled" | "timeout"
        ) {
            crate::services::notify::spawn_job_notify(
                state.pool.clone(),
                state.config.auth.jwt_secret.clone(),
                state.config.server.public_base_url.clone(),
                job_id.clone(),
                req.status.clone(),
            );
        }
    }
    tx.commit().await?;

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
            "SELECT j.status, j.step_statuses, j.start_time, j.end_time, j.debug_unredact
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
            let debug_unredact: i64 = job_row.get(4);
            let payload = serde_json::json!({
                "id": job_id,
                "status": status,
                "step_statuses": serde_json::from_str::<serde_json::Value>(&step_statuses_raw).unwrap_or_default(),
                "start_time": start_time,
                "end_time": end_time,
                "debug_unredact": debug_unredact != 0,
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
