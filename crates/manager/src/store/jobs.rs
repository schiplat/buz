//! Job state transitions.
//!
//! Transaction-internal cores: the caller opens `BEGIN IMMEDIATE`, these
//! functions do the guarded writes, and the caller handles post-commit
//! fan-out (notify, WebSocket, `wake_all`).

use sqlx::{Row, Sqlite, SqlitePool, Transaction};
use uuid::Uuid;

use shared::error::AppError;

/// Current UTC timestamp in SQLite format ("2026-07-27 10:30:00").
pub(crate) fn now_utc() -> String {
    chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

/// A job flipped to `cancelled` inside the caller's transaction.
#[derive(Debug)]
pub struct CancelledJob {
    pub old_status: String,
}

/// Transaction-internal core of job cancellation: flips one non-terminal job
/// (`pending` / `running` / `queued` / `waiting_approval`) to `cancelled`,
/// marks the currently running step cancelled and records the approval
/// outcome for `waiting_approval` jobs. Returns `None` when the job is
/// missing or already terminal, so batch callers can skip instead of fail.
pub(crate) async fn cancel_job_in_tx(
    tx: &mut Transaction<'_, Sqlite>,
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
         cancel_ack_pending = ?,
         end_time = datetime('now'), updated_at = datetime('now')
         WHERE id = ? AND status IN ('pending', 'running', 'queued', 'waiting_approval')",
    )
    .bind(serde_json::to_string(&step_statuses).unwrap_or_else(|_| "[]".into()))
    .bind(serde_json::to_string(&snapshot).unwrap_or_else(|_| "{}".into()))
    .bind(i64::from(old_status == "running"))
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
    Ok(Some(CancelledJob { old_status }))
}

/// Shape the job snapshot's `approval` block for the given outcome.
///
/// Shared by the cancel / approve / reject transaction cores, which all
/// rewrite the same snapshot field.
pub(crate) fn set_snapshot_approval(
    snapshot: &mut serde_json::Value,
    received: i64,
    outcome: Option<&str>,
) {
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

/// Approval-gate shaping helpers + the approve/reject transaction cores.
///
/// The caller (api handler) does the HTTP surface; these functions own the
/// `BEGIN IMMEDIATE` scope and the audit row. Post-commit fan-out: the store
/// wakes pollers when a promotion releases a slot; WebSocket status broadcast
/// stays with the caller (needs `AppState`).
use shared::rbac::TeamRole;

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

/// Result of a successful approval record.
pub struct ApproveOutcome {
    pub new_status: String,
    pub received: i64,
    pub required: i64,
    pub pipeline_id: String,
    pub pipeline_name: String,
    pub team_id: String,
    /// True when the threshold was met and the job left `waiting_approval`.
    pub promoted: bool,
}

/// Record an approval; promote when threshold met.
///
/// Authorization (min approve role, self-approve ban) runs before any write;
/// the status re-check happens inside the transaction so a concurrent
/// reject/cancel wins.
pub async fn approve(
    pool: &SqlitePool,
    auth: &crate::middleware::AuthUser,
    job_id: &str,
    comment: &str,
) -> Result<ApproveOutcome, AppError> {
    let row = sqlx::query(
        "SELECT j.status, j.pipeline_id, j.snapshot, j.trigger_user_id, p.team_id, p.name,
                p.concurrency_group, p.concurrency_mode, p.concurrency_max_queue
         FROM jobs j
         JOIN pipelines p ON p.id = j.pipeline_id
         WHERE j.id = ?",
    )
    .bind(job_id)
    .fetch_optional(pool)
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
        return Err(AppError::Forbidden(
            "self-approval is forbidden for this environment".into(),
        ));
    }

    let approval_id = Uuid::new_v4().to_string();
    let mut approval_tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    let current_status: Option<String> = sqlx::query_scalar("SELECT status FROM jobs WHERE id = ?")
        .bind(job_id)
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
    .bind(job_id)
    .bind(&auth.id)
    .bind(&auth.username)
    .bind(comment)
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
    .bind(job_id)
    .fetch_one(&mut *approval_tx)
    .await?;

    let required = protection_require_approvals(&snapshot);
    let mut new_status = status.clone();
    let mut promoted = false;

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
                .bind(job_id)
                .execute(&mut *approval_tx)
                .await?;
                if result.rows_affected() == 0 {
                    approval_tx.rollback().await?;
                    return Err(AppError::Conflict(
                        "job is no longer waiting for approval".into(),
                    ));
                }
                approval_tx.commit().await?;
                promoted = next != "waiting_approval";
                if next == "pending" {
                    crate::services::poll_events::wake_all();
                }
                new_status = next.to_string();
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
                .bind(job_id)
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
                    job_id = %job_id,
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
        .bind(job_id)
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

    crate::audit::record(
        pool,
        crate::audit::AuditEvent::from_auth(auth, crate::audit::Action::JobApprove)
            .resource("job", job_id, &pipeline_name)
            .team(Some(team_id.clone()))
            .after(Some(serde_json::json!({
                "received": received,
                "required": required,
                "status": new_status,
            }))),
    )
    .await;

    tracing::info!(
        job_id = %job_id,
        actor = %auth.username,
        received,
        required,
        status = %new_status,
        "environments: approve complete"
    );

    Ok(ApproveOutcome {
        new_status,
        received,
        required,
        pipeline_id,
        pipeline_name,
        team_id,
        promoted,
    })
}

/// Record a rejection: the job goes to `cancelled`, re-reading the snapshot
/// inside the transaction so a concurrent approval is not clobbered.
pub async fn reject(
    pool: &SqlitePool,
    auth: &crate::middleware::AuthUser,
    job_id: &str,
    comment: &str,
) -> Result<(), AppError> {
    let row = sqlx::query(
        "SELECT j.status, j.pipeline_id, j.snapshot, j.trigger_user_id, p.team_id, p.name
         FROM jobs j
         JOIN pipelines p ON p.id = j.pipeline_id
         WHERE j.id = ?",
    )
    .bind(job_id)
    .fetch_optional(pool)
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
        return Err(AppError::Forbidden(
            "self-approval is forbidden for this environment".into(),
        ));
    }

    let mut reject_tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    let current = sqlx::query("SELECT status, snapshot FROM jobs WHERE id = ?")
        .bind(job_id)
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
    .bind(job_id)
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
    .bind(job_id)
    .bind(&auth.id)
    .bind(&auth.username)
    .bind(comment)
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
    crate::services::poll_events::wake_all();

    crate::audit::record(
        pool,
        crate::audit::AuditEvent::from_auth(auth, crate::audit::Action::JobReject)
            .resource("job", job_id, &pipeline_name)
            .team(Some(team_id))
            .after(Some(
                serde_json::json!({ "status": "cancelled", "outcome": "rejected" }),
            )),
    )
    .await;

    tracing::info!(
        job_id = %job_id,
        actor = %auth.username,
        "environments: reject complete"
    );

    let _ = pipeline_id;
    Ok(())
}

/// Pipeline id for a job — enough to scope a post-commit status broadcast.
pub async fn job_pipeline_id(pool: &SqlitePool, job_id: &str) -> Result<String, AppError> {
    sqlx::query_scalar("SELECT pipeline_id FROM jobs WHERE id = ?")
        .bind(job_id)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| AppError::NotFound("job not found".into()))
}
