//! Job state transitions.
//!
//! Transaction-internal cores: the caller opens `BEGIN IMMEDIATE`, these
//! functions do the guarded writes, and the caller handles post-commit
//! fan-out (notify, WebSocket, `wake_all`).

use sqlx::{Row, Sqlite, Transaction};
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
