//! Post-commit fan-out for job status changes.
//!
//! Every path that flips a job's status outside the transaction ends with the
//! same two side effects: async notify channels (terminal statuses only) and
//! a WebSocket status broadcast. This module is the single home for that
//! fan-out so a new transition cannot forget one half or re-read the row with
//! a slightly different query.
//!
//! Audit rows and `poll_events::wake_all()` are deliberately NOT part of this:
//! audit belongs inside the caller's transaction (`record_in`) with
//! transition-specific before/after payloads, and the wake timing is a
//! scheduling decision the caller owns.

use sqlx::Row;

use crate::state::AppState;

/// A job status that notify channels treat as an end-of-build event.
fn is_notify_status(status: &str) -> bool {
    matches!(status, "success" | "failed" | "cancelled" | "timeout")
}

/// Fan out a committed status change: async notify (when `notify` and the
/// status is terminal) plus a WebSocket broadcast whose payload is re-read
/// from the jobs row — after the commit, the DB is the authoritative snapshot
/// (status, step_statuses, start/end times, debug flag), so every caller gets
/// the same payload shape without hand-building one.
///
/// Fire-and-forget: a missing row (deleted concurrently) skips the broadcast
/// silently; notify failures are logged inside the spawned task.
pub(crate) async fn announce_job_status(
    state: &AppState,
    job_id: &str,
    status: &str,
    notify: bool,
) {
    if notify && is_notify_status(status) {
        crate::services::notify::spawn_job_notify(
            state.pool.clone(),
            state.config.auth.jwt_secret.clone(),
            state.config.server.public_base_url.clone(),
            job_id.to_string(),
            status.to_string(),
        );
    }

    // Broadcast status to WebSocket subscribers.
    if let Ok(Some(row)) = sqlx::query(
        "SELECT pipeline_id, status, step_statuses, start_time, end_time, debug_unredact
         FROM jobs WHERE id = ?",
    )
    .bind(job_id)
    .fetch_optional(&state.pool)
    .await
    {
        let pipeline_id: String = row.get(0);
        let row_status: String = row.get(1);
        let payload = serde_json::json!({
            "id": job_id,
            "status": row_status,
            "step_statuses": serde_json::from_str::<serde_json::Value>(
                &row.get::<String, _>(2),
            )
            .unwrap_or_default(),
            "start_time": row.get::<Option<String>, _>(3),
            "end_time": row.get::<Option<String>, _>(4),
            "debug_unredact": row.get::<i64, _>(5) != 0,
        });
        state.log_broadcast.broadcast_status(
            job_id,
            &pipeline_id,
            &serde_json::to_string(&payload).unwrap_or_default(),
        );
    }
}
