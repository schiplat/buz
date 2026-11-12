//! Job state transitions.
//!
//! Transaction-internal cores: the caller opens `BEGIN IMMEDIATE`, these
//! functions do the guarded writes, and the caller handles post-commit
//! fan-out (notify, WebSocket, `wake_all`).
//!
//! On the dual-backend dispatcher (`store::exec`): pool-level reads and plain
//! writes travel as SQLite-dialect strings with backend-neutral binds, rows
//! decode by column name. The `BEGIN IMMEDIATE` write paths (`approve`,
//! `reject`, `apply_runner_status`, `insert_rerun`, `cancel`, and the `*_in_tx`
//! cores) stay on `sqlx::SqlitePool` / `sqlx::Transaction<Sqlite>` — they
//! share the transaction scope with `crate::audit::record` / `record_in` and
//! migrate with the audit tx-scope slice.

use crate::store::exec::{self, DbPool, SqlValue};
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
/// Stays on the caller's `sqlx::Transaction` — tx-scope slice.
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
/// reject/cancel wins. Stays on `&sqlx::SqlitePool` — its own `BEGIN
/// IMMEDIATE` scope and the post-commit `crate::audit::record` call; tx-scope
/// slice.
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
/// inside the transaction so a concurrent approval is not clobbered. Stays on
/// `&sqlx::SqlitePool` — its own `BEGIN IMMEDIATE` scope and the post-commit
/// `crate::audit::record` call; tx-scope slice.
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
pub async fn job_pipeline_id(pool: &DbPool, job_id: &str) -> Result<String, AppError> {
    exec::fetch_optional(
        pool,
        "SELECT pipeline_id FROM jobs WHERE id = ?",
        &[job_id.into()],
    )
    .await?
    .map(|r| r.text_of("pipeline_id"))
    .ok_or_else(|| AppError::NotFound("job not found".into()))
}

/// Runner status report: one job, one status, optional per-step order.
pub struct RunnerStatusReport {
    pub status: String,
    pub step_order: Option<i32>,
    pub truncated_steps: Vec<i32>,
}

/// Outcome of a runner status report.
pub struct StatusReportOutcome {
    /// False when the report was silently ignored — a terminal state already
    /// won (manual cancel/completion), or the row flipped underneath us.
    pub applied: bool,
    /// True when the reported status itself is terminal.
    pub terminal: bool,
}

/// Transaction-internal core of the Runner status report (`update_status`).
///
/// Invariants, all under one `BEGIN IMMEDIATE`:
/// - only the runner the job was dispatched to may write it;
/// - terminal states win — a manual cancel or a finished job is never
///   overwritten (an ignored report is `Ok(applied=false)`, not an error);
/// - per-step updates upsert into `step_statuses` with started/finished stamps;
/// - a job-level `failed` sweeps any still-running step to `failed` so the UI
///   never hangs;
/// - `truncated_steps` is a union, never an assignment (DAG steps report
///   concurrently; a late per-step report must not erase earlier ones).
///
/// Post-commit fan-out (poller wake, announce) stays with the caller. Stays
/// on `&sqlx::SqlitePool` — its own `BEGIN IMMEDIATE` scope; tx-scope slice.
pub async fn apply_runner_status(
    pool: &SqlitePool,
    runner_id: &str,
    job_id: &str,
    report: &RunnerStatusReport,
) -> Result<StatusReportOutcome, AppError> {
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    let row = sqlx::query("SELECT status, runner_id FROM jobs WHERE id = ?")
        .bind(job_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| AppError::NotFound("job not found".into()))?;

    // Runner↔job binding: only the runner poll() dispatched the job to may
    // update it — a leaked runner JWT must not be able to touch other jobs.
    let owner: Option<String> = row.get(1);
    if owner.as_deref() != Some(runner_id) {
        tx.rollback().await?;
        return Err(AppError::Forbidden(
            "job is not assigned to this runner".into(),
        ));
    }

    let current_status: String = row.get(0);

    // Terminal states win — don't let the Runner overwrite a cancellation or completion.
    if matches!(
        current_status.as_str(),
        "cancelled" | "success" | "failed" | "timeout"
    ) {
        tx.rollback().await?;
        return Ok(StatusReportOutcome {
            applied: false,
            terminal: true,
        });
    }

    if let Some(step_order) = report.step_order {
        // Per-step status update
        let row2 = sqlx::query("SELECT step_statuses FROM jobs WHERE id = ?")
            .bind(job_id)
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
            existing["status"] = serde_json::json!(&report.status);
            // Record started_at when a step transitions to "running"
            if report.status == "running" {
                existing["started_at"] = serde_json::json!(now);
            }
            // Record finished_at when a step reaches a terminal state
            if matches!(report.status.as_str(), "success" | "failed" | "cancelled") {
                existing["finished_at"] = serde_json::json!(now);
            }
        } else {
            let mut entry = serde_json::json!({
                "step_order": step_order,
                "status": &report.status,
            });
            if report.status == "running" {
                entry["started_at"] = serde_json::json!(now);
            }
            if matches!(report.status.as_str(), "success" | "failed" | "cancelled") {
                entry["finished_at"] = serde_json::json!(now);
            }
            statuses.push(entry);
        }

        let updated = serde_json::to_string(&statuses).unwrap_or_else(|_| "[]".into());
        sqlx::query("UPDATE jobs SET step_statuses = ?, updated_at = datetime('now') WHERE id = ?")
            .bind(&updated)
            .bind(job_id)
            .execute(&mut *tx)
            .await?;
    } else {
        // Overall status update
        let now = "datetime('now')";
        let end_expr = if matches!(
            report.status.as_str(),
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
            .bind(&report.status)
            .bind(job_id)
            .execute(&mut *tx)
            .await?;

        if result.rows_affected() == 0 {
            tx.rollback().await?;
            return Ok(StatusReportOutcome {
                applied: false,
                terminal: true,
            });
        }

        // Safety net: if the job transitions to a terminal state, mark any
        // step still reported as "running" as "failed" so the UI never hangs.
        if report.status == "failed" {
            let row3 = sqlx::query("SELECT step_statuses FROM jobs WHERE id = ?")
                .bind(job_id)
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
                        .bind(job_id)
                        .execute(&mut *tx)
                        .await?;
                }
            }
        }
    }

    // Truncation is a fact about the job, not about its log rows: persist it so
    // it survives job_log_retention sweeping the marker chunk.
    // Union, never assignment. Read-merge-write is safe here — the surrounding
    // BEGIN IMMEDIATE holds the single SQLite writer.
    if !report.truncated_steps.is_empty() {
        let existing: String = sqlx::query_scalar("SELECT truncated_steps FROM jobs WHERE id = ?")
            .bind(job_id)
            .fetch_one(&mut *tx)
            .await?;
        let mut merged: Vec<i32> = serde_json::from_str(&existing).unwrap_or_default();
        merged.extend(report.truncated_steps.iter().copied());
        merged.sort_unstable();
        merged.dedup();
        let encoded = serde_json::to_string(&merged).unwrap_or_else(|_| "[]".into());
        sqlx::query("UPDATE jobs SET truncated_steps = ? WHERE id = ?")
            .bind(&encoded)
            .bind(job_id)
            .execute(&mut *tx)
            .await?;
    }

    tx.commit().await?;
    let terminal = matches!(
        report.status.as_str(),
        "success" | "failed" | "cancelled" | "timeout"
    );
    if terminal {
        crate::services::poll_events::wake_all();
    }
    Ok(StatusReportOutcome {
        applied: true,
        terminal,
    })
}

/// Everything needed to insert one re-run attempt.
pub struct RerunInsert<'a> {
    pub source_job_id: &'a str,
    pub pipeline_id: &'a str,
    pub pipeline_name: &'a str,
    pub team_id: &'a str,
    /// `pending` or `waiting_approval` — gates decided by the caller.
    pub job_status: &'a str,
    pub trigger_by: &'a str,
    pub trigger_user_id: &'a str,
    /// Sealed `trigger_params` copied verbatim from the source job.
    pub trigger_params_raw: Option<&'a str>,
    pub snapshot_str: &'a str,
    pub debug_unredact: bool,
    pub verbose_logs: bool,
    pub environment_id: Option<&'a str>,
    pub run_number: i64,
    pub run_attempt: i64,
    pub resource_profile: &'a str,
}

/// Insert a re-run attempt atomically: `is_active` is re-checked inside the
/// transaction (closing the check-to-insert race with a concurrent disable,
/// same as `reserve_job`), the row and its audit record land together.
/// Returns the new job id. Poller wake happens after commit. Stays on
/// `&sqlx::SqlitePool` — its own `BEGIN IMMEDIATE` around
/// `crate::audit::record_in`; audit tx-scope slice.
pub async fn insert_rerun(
    pool: &SqlitePool,
    insert: &RerunInsert<'_>,
    actor: &crate::middleware::AuthUser,
) -> Result<String, AppError> {
    let job_id = Uuid::new_v4().to_string();
    let mut tx = pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;
    match sqlx::query_scalar::<_, i64>("SELECT is_active FROM pipelines WHERE id = ?")
        .bind(insert.pipeline_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?
    {
        None => return Err(AppError::NotFound("pipeline not found".into())),
        Some(0) => {
            return Err(AppError::Conflict(
                "pipeline is disabled — enable it before re-running".into(),
            ));
        }
        Some(_) => {}
    }
    sqlx::query(
        "INSERT INTO jobs (id, pipeline_id, status, trigger_by, trigger_params, snapshot,
                           debug_unredact, verbose_logs, environment_id, trigger_user_id,
                           run_number, run_attempt, resource_profile, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, datetime('now'), datetime('now'))",
    )
    .bind(&job_id)
    .bind(insert.pipeline_id)
    .bind(insert.job_status)
    .bind(insert.trigger_by)
    .bind(insert.trigger_params_raw)
    .bind(insert.snapshot_str)
    .bind(i64::from(insert.debug_unredact))
    .bind(i64::from(insert.verbose_logs))
    .bind(insert.environment_id)
    .bind(insert.trigger_user_id)
    .bind(insert.run_number)
    .bind(insert.run_attempt)
    .bind(insert.resource_profile)
    .execute(&mut *tx)
    .await
    .map_err(|e| AppError::Internal(e.to_string()))?;

    crate::audit::record_in(
        &mut tx,
        crate::audit::AuditEvent::from_auth(actor, crate::audit::Action::JobRerun)
            .resource("job", &job_id, insert.pipeline_name)
            .team(Some(insert.team_id.to_string()))
            .after(Some(serde_json::json!({
                "rerun_of": insert.source_job_id,
                "run_number": insert.run_number,
                "run_attempt": insert.run_attempt,
                "status": insert.job_status,
            }))),
    )
    .await?;
    tx.commit()
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;

    crate::services::poll_events::wake_all();
    Ok(job_id)
}

// ── Read paths ──────────────────────────────────────────────────────────────
//
// Team scope / debug filtering arrive pre-built from the handler (that is
// authorization); these functions assemble and run the queries and shape rows.
// They run on the dual-backend dispatcher (`store::exec`).

pub type TeamScope = Option<(String, Vec<String>)>;

/// `ORDER BY`-stable listing of the latest 200 jobs (admin list page).
pub async fn list_jobs(
    pool: &DbPool,
    team_scope: &TeamScope,
    hide_debug: bool,
) -> Result<Vec<serde_json::Value>, AppError> {
    let base = "SELECT j.id, j.pipeline_id, p.name as pipeline_name, j.runner_id,
                j.status, j.trigger_by, j.retry_times,
                j.step_statuses, j.snapshot,
                j.start_time, j.end_time, j.created_at, j.updated_at,
                p.team_id, j.verbose_logs, j.debug_unredact, j.trigger_user_id,
                j.run_number
         FROM jobs j
         JOIN pipelines p ON j.pipeline_id = p.id
         WHERE 1=1";
    let debug_filter = if hide_debug {
        "AND j.debug_unredact = 0"
    } else {
        ""
    };
    let sql = match team_scope {
        None => format!("{base} {debug_filter} ORDER BY j.created_at DESC LIMIT 200"),
        Some((clause, _)) => {
            format!("{base} {clause} {debug_filter} ORDER BY j.created_at DESC LIMIT 200")
        }
    };

    let mut binds: Vec<SqlValue> = Vec::new();
    if let Some((_, ids)) = team_scope {
        for id in ids {
            binds.push(id.as_str().into());
        }
    }
    let rows = exec::fetch_all(pool, &sql, &binds).await?;

    Ok(rows
        .into_iter()
        .map(|r| {
            let step_statuses: serde_json::Value =
                serde_json::from_str(&r.text_of("step_statuses")).unwrap_or_default();
            let snapshot: serde_json::Value =
                serde_json::from_str(&r.text_of("snapshot")).unwrap_or_default();
            serde_json::json!({
                "id": r.text_of("id"),
                "pipeline_id": r.text_of("pipeline_id"),
                "pipeline_name": r.opt_text_of("pipeline_name"),
                "runner_id": r.opt_text_of("runner_id"),
                "status": r.text_of("status"),
                "trigger_by": r.text_of("trigger_by"),
                "retry_times": r.i64_of("retry_times"),
                "step_statuses": step_statuses,
                "snapshot": snapshot,
                "start_time": r.opt_text_of("start_time"),
                "end_time": r.opt_text_of("end_time"),
                "created_at": r.opt_text_of("created_at"),
                "updated_at": r.opt_text_of("updated_at"),
                "team_id": r.text_of("team_id"),
                "verbose_logs": r.i64_of("verbose_logs") != 0,
                "debug_unredact": r.i64_of("debug_unredact") != 0,
                "trigger_user_id": r.opt_text_of("trigger_user_id"),
                "run_number": r.i64_of("run_number"),
            })
        })
        .collect())
}

/// Dashboard build trend: `(day, status, count)` rows since `cutoff`.
pub async fn trend_rows(
    pool: &DbPool,
    cutoff: &str,
    team_scope: &TeamScope,
    hide_debug: bool,
) -> Result<Vec<(String, String, i64)>, AppError> {
    let debug_filter = if hide_debug {
        "AND j.debug_unredact = 0"
    } else {
        ""
    };
    let sql = match team_scope {
        None => format!(
            "SELECT date(j.created_at) AS day, j.status, COUNT(*) AS cnt
             FROM jobs j JOIN pipelines p ON j.pipeline_id = p.id
             WHERE j.created_at >= ? {debug_filter}
             GROUP BY date(j.created_at), j.status"
        ),
        Some((clause, _)) => format!(
            "SELECT date(j.created_at) AS day, j.status, COUNT(*) AS cnt
             FROM jobs j JOIN pipelines p ON j.pipeline_id = p.id
             WHERE j.created_at >= ? {clause} {debug_filter}
             GROUP BY date(j.created_at), j.status"
        ),
    };

    let mut binds: Vec<SqlValue> = vec![cutoff.into()];
    if let Some((_, ids)) = team_scope {
        for id in ids {
            binds.push(id.as_str().into());
        }
    }
    let rows = exec::fetch_all(pool, &sql, &binds).await?;
    Ok(rows
        .into_iter()
        .map(|r| (r.text(0), r.text(1), r.i64(2)))
        .collect())
}

/// Overview build stream: latest `limit` jobs, team- and role-scoped.
pub async fn recent_rows(
    pool: &DbPool,
    limit: i64,
    team_scope: &TeamScope,
    hide_debug: bool,
) -> Result<Vec<serde_json::Value>, AppError> {
    let debug_filter = if hide_debug {
        "AND j.debug_unredact = 0"
    } else {
        ""
    };
    let sql = match team_scope {
        None => format!(
            "SELECT j.id, j.pipeline_id, p.name AS pipeline_name, j.status,
                    j.trigger_by, p.team_id, j.created_at
             FROM jobs j JOIN pipelines p ON j.pipeline_id = p.id
             WHERE 1=1 {debug_filter}
             ORDER BY j.created_at DESC, j.id DESC
             LIMIT ?"
        ),
        Some((clause, _)) => format!(
            "SELECT j.id, j.pipeline_id, p.name AS pipeline_name, j.status,
                    j.trigger_by, p.team_id, j.created_at
             FROM jobs j JOIN pipelines p ON j.pipeline_id = p.id
             WHERE 1=1 {clause} {debug_filter}
             ORDER BY j.created_at DESC, j.id DESC
             LIMIT ?"
        ),
    };

    let mut binds: Vec<SqlValue> = Vec::new();
    if let Some((_, ids)) = team_scope {
        for id in ids {
            binds.push(id.as_str().into());
        }
    }
    binds.push(SqlValue::Int(limit));
    let rows = exec::fetch_all(pool, &sql, &binds).await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            serde_json::json!({
                "id": r.text_of("id"),
                "pipeline_id": r.text_of("pipeline_id"),
                "pipeline_name": r.opt_text_of("pipeline_name"),
                "status": r.text_of("status"),
                "trigger_by": r.text_of("trigger_by"),
                "team_id": r.text_of("team_id"),
                "created_at": r.opt_text_of("created_at"),
            })
        })
        .collect())
}

/// Approval history rows for one job.
pub async fn list_job_approvals(
    pool: &DbPool,
    job_id: &str,
) -> Result<Vec<serde_json::Value>, AppError> {
    let rows = exec::fetch_all(
        pool,
        "SELECT id, user_id, username, action, comment, created_at
         FROM job_approvals
         WHERE job_id = ?
         ORDER BY created_at ASC, id ASC",
        &[job_id.into()],
    )
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            serde_json::json!({
                "id": r.text_of("id"),
                "user_id": r.text_of("user_id"),
                "username": r.text_of("username"),
                "action": r.text_of("action"),
                "comment": r.text_of("comment"),
                "created_at": r.opt_text_of("created_at"),
            })
        })
        .collect())
}

/// A job row joined with its pipeline name — the detail-page payload source.
pub struct JobDetailRow {
    pub id: String,
    pub pipeline_id: String,
    pub pipeline_name: Option<String>,
    pub runner_id: Option<String>,
    pub status: String,
    pub trigger_by: String,
    pub trigger_params: Option<String>,
    pub snapshot_raw: Option<String>,
    pub step_statuses_raw: Option<String>,
    pub retry_times: i64,
    pub start_time: Option<String>,
    pub end_time: Option<String>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    pub debug_unredact: bool,
    pub verbose_logs: bool,
    pub trigger_user_id: Option<String>,
    pub run_number: i64,
    pub run_attempt: i64,
    pub resource_profile: String,
    pub resource_slots: i64,
    pub resource_spec_raw: String,
    pub sandbox_instance_id: String,
    pub dispatch_reason: String,
}

/// Fetch one job with pipeline identity (`None` = no such job).
pub async fn job_detail_row(pool: &DbPool, id: &str) -> Result<Option<JobDetailRow>, AppError> {
    let r = exec::fetch_optional(
        pool,
        "SELECT j.id, j.pipeline_id, p.name as pipeline_name, j.runner_id,
                j.status, j.trigger_by, j.trigger_params, j.snapshot, j.step_statuses, j.retry_times,
                j.start_time, j.end_time, j.created_at, j.updated_at, j.debug_unredact, j.verbose_logs,
                j.trigger_user_id, j.run_number, j.run_attempt,
                j.resource_profile, j.resource_slots, j.resource_spec, j.sandbox_instance_id,
                j.dispatch_reason
         FROM jobs j
         LEFT JOIN pipelines p ON j.pipeline_id = p.id
         WHERE j.id = ?",
        &[id.into()],
    )
    .await?;
    Ok(r.map(|r| JobDetailRow {
        id: r.text_of("id"),
        pipeline_id: r.text_of("pipeline_id"),
        pipeline_name: r.opt_text_of("pipeline_name"),
        runner_id: r.opt_text_of("runner_id"),
        status: r.text_of("status"),
        trigger_by: r.text_of("trigger_by"),
        trigger_params: r.opt_text_of("trigger_params"),
        snapshot_raw: r.opt_text_of("snapshot"),
        step_statuses_raw: r.opt_text_of("step_statuses"),
        retry_times: r.i64_of("retry_times"),
        start_time: r.opt_text_of("start_time"),
        end_time: r.opt_text_of("end_time"),
        created_at: r.opt_text_of("created_at"),
        updated_at: r.opt_text_of("updated_at"),
        debug_unredact: r.i64_of("debug_unredact") != 0,
        verbose_logs: r.i64_of("verbose_logs") != 0,
        trigger_user_id: r.opt_text_of("trigger_user_id"),
        run_number: r.i64_of("run_number"),
        run_attempt: r.i64_of("run_attempt"),
        resource_profile: r.text_of("resource_profile"),
        resource_slots: r.i64_of("resource_slots"),
        resource_spec_raw: r.text_of("resource_spec"),
        sandbox_instance_id: r.text_of("sandbox_instance_id"),
        dispatch_reason: r.text_of("dispatch_reason"),
    }))
}

/// `(status, pipeline_id, pipeline_name?)` — the audit context for cancel/delete.
pub async fn job_context(
    pool: &DbPool,
    id: &str,
) -> Result<Option<(String, String, Option<String>)>, AppError> {
    let row = exec::fetch_optional(
        pool,
        "SELECT j.status, j.pipeline_id, p.name
         FROM jobs j
         LEFT JOIN pipelines p ON p.id = j.pipeline_id
         WHERE j.id = ?",
        &[id.into()],
    )
    .await?;
    Ok(row.map(|r| (r.text(0), r.text(1), r.opt_text(2))))
}

/// Owner-scoped status poll for the Runner.
pub async fn runner_job_status(
    pool: &DbPool,
    runner_id: &str,
    job_id: &str,
) -> Result<Option<String>, AppError> {
    let row = exec::fetch_optional(
        pool,
        "SELECT status, runner_id FROM jobs WHERE id = ?",
        &[job_id.into()],
    )
    .await?
    .ok_or_else(|| AppError::NotFound("job not found".into()))?;
    let owner = row.opt_text_of("runner_id");
    if owner.as_deref() != Some(runner_id) {
        return Err(AppError::Forbidden(
            "job is not assigned to this runner".into(),
        ));
    }
    Ok(Some(row.text_of("status")))
}

/// Delete one job row.
pub async fn delete_job(pool: &DbPool, id: &str) -> Result<(), AppError> {
    exec::execute(pool, "DELETE FROM jobs WHERE id = ?", &[id.into()]).await?;
    Ok(())
}

/// Step orders whose streamed output hit the truncation marker
/// (`chunk_index % 10_000 == LOG_STEP_TRUNCATED`). Per-step 10_000-wide
/// windows make the modulo exact; negatives (runner banner) never match.
///
/// Prefers the durable job column (survives job_log_retention sweeping the
/// marker chunk); legacy fallback reads the marker chunk while logs remain.
pub async fn truncated_steps(pool: &DbPool, job_id: &str) -> Vec<i64> {
    let stored = exec::fetch_optional(
        pool,
        "SELECT truncated_steps FROM jobs WHERE id = ?",
        &[job_id.into()],
    )
    .await
    .ok()
    .flatten()
    .and_then(|r| r.opt_text_of("truncated_steps"));
    if let Some(raw) = stored {
        if let Ok(mut steps) = serde_json::from_str::<Vec<i64>>(&raw) {
            if !steps.is_empty() {
                steps.sort_unstable();
                steps.dedup();
                return steps;
            }
        }
    }

    let mut steps: Vec<i64> = match exec::fetch_all(
        pool,
        "SELECT DISTINCT step_order FROM job_logs
         WHERE job_id = ? AND chunk_index >= 0
           AND chunk_index % 10000 = ?",
        &[job_id.into(), shared::protocol::LOG_STEP_TRUNCATED.into()],
    )
    .await
    {
        Ok(rows) => rows.into_iter().map(|r| r.i64(0)).collect(),
        Err(_) => Vec::new(),
    };
    steps.sort_unstable();
    steps
}

/// `(pipeline_id, pipeline name?)` for cancel-path audit context.
pub async fn job_pipeline_name(
    pool: &DbPool,
    job_id: &str,
) -> Result<Option<(String, Option<String>)>, AppError> {
    let row = exec::fetch_optional(
        pool,
        "SELECT j.pipeline_id, p.name FROM jobs j
         LEFT JOIN pipelines p ON p.id = j.pipeline_id
         WHERE j.id = ?",
        &[job_id.into()],
    )
    .await?;
    Ok(row.map(|r| (r.text(0), r.opt_text(1))))
}

/// Source-job facts for a re-run — gates and snapshot rebuild read these.
pub struct RerunSource {
    pub status: String,
    pub pipeline_id: String,
    pub trigger_params_raw: Option<String>,
    pub snapshot_raw: String,
    pub environment_id: Option<String>,
    pub debug_unredact: bool,
    pub verbose_logs: bool,
    pub run_number: i64,
    pub run_attempt: i64,
    pub resource_profile: String,
    pub team_id: String,
    pub pipeline_name: String,
    pub pipeline_active: bool,
    pub concurrency_group: Option<String>,
    pub concurrency_mode: String,
    pub concurrency_max_queue: i64,
}

/// Fetch the terminal job being re-run, joined with its pipeline's gate state.
pub async fn rerun_source(pool: &DbPool, id: &str) -> Result<RerunSource, AppError> {
    let row = exec::fetch_optional(
        pool,
        "SELECT j.status, j.pipeline_id, j.trigger_params, j.snapshot,
                j.environment_id, j.debug_unredact, j.verbose_logs,
                j.run_number, j.run_attempt, j.resource_profile,
                p.team_id, p.name, p.is_active,
                p.concurrency_group, p.concurrency_mode, p.concurrency_max_queue
         FROM jobs j JOIN pipelines p ON p.id = j.pipeline_id
         WHERE j.id = ?",
        &[id.into()],
    )
    .await?
    .ok_or_else(|| AppError::NotFound("job not found".into()))?;
    Ok(RerunSource {
        status: row.text_of("status"),
        pipeline_id: row.text_of("pipeline_id"),
        trigger_params_raw: row.opt_text_of("trigger_params"),
        snapshot_raw: row.text_of("snapshot"),
        environment_id: row.opt_text_of("environment_id"),
        debug_unredact: row.i64_of("debug_unredact") != 0,
        verbose_logs: row.i64_of("verbose_logs") != 0,
        run_number: row.i64_of("run_number"),
        run_attempt: row.i64_of("run_attempt"),
        resource_profile: row.text_of("resource_profile"),
        team_id: row.text_of("team_id"),
        pipeline_name: row.text_of("name"),
        pipeline_active: row.i64_of("is_active") != 0,
        concurrency_group: row.opt_text_of("concurrency_group"),
        concurrency_mode: row.text_of("concurrency_mode"),
        concurrency_max_queue: row.i64_of("concurrency_max_queue"),
    })
}

/// Operator cancellation: the guard update and its audit record land in one
/// `BEGIN IMMEDIATE` (`Err(NotFound)` when the job is missing or already
/// terminal — the transaction is rolled back). Post-commit announce stays
/// with the caller. Stays on `&sqlx::SqlitePool` — its own `BEGIN IMMEDIATE`
/// around `crate::audit::record_in`; audit tx-scope slice.
pub async fn cancel(
    pool: &SqlitePool,
    actor: &crate::middleware::AuthUser,
    job_id: &str,
    pipeline_id: &str,
    pipeline_name: Option<&str>,
    team_id: &str,
) -> Result<String, AppError> {
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    let Some(cancelled) = cancel_job_in_tx(&mut tx, job_id, &actor.id, &actor.username).await?
    else {
        tx.rollback().await?;
        return Err(AppError::NotFound(
            "job not found or already finished".into(),
        ));
    };
    crate::audit::record_in(
        &mut tx,
        crate::audit::AuditEvent::new(crate::audit::Action::JobCancel, actor)
            .on(crate::audit::Resource::job(
                job_id,
                pipeline_name.unwrap_or(pipeline_id),
            ))
            .team(Some(team_id.to_string()))
            .changes(
                Some(serde_json::json!({ "status": cancelled.old_status })),
                Some(serde_json::json!({ "status": "cancelled" })),
            ),
    )
    .await?;
    tx.commit().await?;
    crate::services::poll_events::wake_all();
    Ok(cancelled.old_status)
}
