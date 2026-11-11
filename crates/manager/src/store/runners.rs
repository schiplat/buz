//! Runner rows: admin management operations and the bookkeeping writes the
//! poller performs on its own runner.
//!
//! The claim decision itself (candidate selection, slot gate, environment
//! gates, cancel_in_progress supersede) stays in `api/runners.rs`: it is
//! interleaved with a loop's control flow and moves in a separate step.

use serde_json::{json, Value};
use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use shared::error::AppError;

/// Why the last poll round skipped a pending job. Display-only, cleared on
/// claim. Logged only when the reason changes, so a job parked on one reason
/// does not repeat itself every poll round.
pub async fn record_dispatch_reason(pool: &SqlitePool, job_id: &str, reason: &str) {
    match sqlx::query(
        "UPDATE jobs SET dispatch_reason = ? WHERE id = ? AND status = 'pending'
         AND COALESCE(dispatch_reason, '') != ?",
    )
    .bind(reason)
    .bind(job_id)
    .bind(reason)
    .execute(pool)
    .await
    {
        Ok(done) if done.rows_affected() > 0 => {
            tracing::info!(job = %job_id, reason, "job not dispatched");
        }
        Ok(_) => {}
        Err(e) => {
            tracing::warn!(job = %job_id, error = %e, "failed to record dispatch reason");
        }
    }
}

/// Append one runner event row. Fire-and-forget: a failure is logged and
/// not propagated, because event history must never block an operator action.
pub async fn log_runner_event(pool: &SqlitePool, runner_id: &str, event_type: &str, message: &str) {
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

/// `(name, team_id, status)` snapshot taken before a row is deleted: the audit
/// record outlives the row and is the last description of it.
pub async fn runner_snapshot(
    pool: &SqlitePool,
    id: &str,
) -> Result<Option<(String, Option<String>, String)>, AppError> {
    let row = sqlx::query("SELECT name, team_id, status FROM runners WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await?;
    Ok(row.map(|r| (r.get(0), r.try_get(1).ok().flatten(), r.get(2))))
}

/// Delete one runner. False when no row matched.
///
/// `jobs.runner_id` carries a plain FK to `runners(id)` with no `ON DELETE`
/// action, so the referencing rows must be cleared or the delete fails.
pub async fn delete_runner(pool: &SqlitePool, id: &str) -> Result<bool, AppError> {
    sqlx::query("UPDATE jobs SET runner_id = NULL WHERE runner_id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    let result = sqlx::query("DELETE FROM runners WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() != 0)
}

/// Flip the freeze flag: `(name, was_frozen, now_frozen)`.
///
/// Read-modify-write in one `BEGIN IMMEDIATE`, so two concurrent toggles cannot
/// both read the same value and write the same result.
pub async fn toggle_freeze(pool: &SqlitePool, id: &str) -> Result<(String, bool, bool), AppError> {
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    let row = sqlx::query("SELECT name, is_frozen FROM runners WHERE id = ?")
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| AppError::NotFound("runner not found".into()))?;
    let name: String = row.get(0);
    let was_frozen = row.get::<i64, _>(1) != 0;
    let now_frozen = !was_frozen;
    sqlx::query("UPDATE runners SET is_frozen = ?, updated_at = datetime('now') WHERE id = ?")
        .bind(i64::from(now_frozen))
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    crate::services::poll_events::wake_all();
    Ok((name, was_frozen, now_frozen))
}

/// What a restart request needs to know about the runner.
pub struct RunnerRestartState {
    pub name: String,
    /// Ids of jobs still running on it (`ORDER BY created_at`).
    pub running_job_ids: Vec<String>,
}

pub async fn runner_restart_state(
    pool: &SqlitePool,
    id: &str,
) -> Result<RunnerRestartState, AppError> {
    let name: Option<String> = sqlx::query_scalar("SELECT name FROM runners WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await?;
    let running_job_ids: Vec<String> = sqlx::query_scalar(
        "SELECT id FROM jobs WHERE runner_id = ? AND status = 'running' ORDER BY created_at",
    )
    .bind(id)
    .fetch_all(pool)
    .await?;
    Ok(RunnerRestartState {
        name: name.ok_or_else(|| AppError::NotFound("runner not found".into()))?,
        running_job_ids,
    })
}

/// Set the restart flag served on the runner's next heartbeat.
pub async fn request_restart(pool: &SqlitePool, id: &str) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE runners SET restart_requested = 1, updated_at = datetime('now') WHERE id = ?",
    )
    .bind(id)
    .execute(pool)
    .await?;
    crate::services::poll_events::wake_all();
    Ok(())
}

/// Set the one-shot capability re-probe flag.
pub async fn request_capability_reprobe(pool: &SqlitePool, id: &str) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE runners SET capability_reprobe_requested = 1,
            capability_reprobe_requested_at = datetime('now'),
            capability_reprobe_served_at = NULL,
            updated_at = datetime('now') WHERE id = ?",
    )
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Replace the one-shot register token. Returns the runner name, or `None`
/// when the row does not exist received.
///
/// The caller passes an already-hashed token: plaintext is never stored.
pub async fn reissue_register_token(
    pool: &SqlitePool,
    id: &str,
    token_hash: &str,
) -> Result<Option<String>, AppError> {
    Ok(sqlx::query_scalar(
        "UPDATE runners SET register_token = ?, updated_at = datetime('now') WHERE id = ? RETURNING name",
    )
    .bind(token_hash)
    .bind(id)
    .fetch_optional(pool)
    .await?)
}

/// Runner name by id (`None` when absent).
pub async fn runner_name(pool: &SqlitePool, id: &str) -> Result<Option<String>, AppError> {
    Ok(sqlx::query_scalar("SELECT name FROM runners WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await?)
}

/// True when the runner row exists.
pub async fn exists(pool: &SqlitePool, id: &str) -> Result<bool, AppError> {
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runners WHERE id = ?")
        .bind(id)
        .fetch_one(pool)
        .await?;
    Ok(count != 0)
}

/// One page of a runner's event history plus the total count.
pub struct RunnerEventPage {
    pub events: Vec<Value>,
    pub total: i64,
}

pub async fn event_page(
    pool: &SqlitePool,
    runner_id: &str,
    limit: i64,
    offset: i64,
) -> Result<RunnerEventPage, AppError> {
    let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runner_events WHERE runner_id = ?")
        .bind(runner_id)
        .fetch_one(pool)
        .await?;
    let rows = sqlx::query(
        "SELECT id, event_type, message, created_at
         FROM runner_events
         WHERE runner_id = ?
         ORDER BY created_at DESC
         LIMIT ? OFFSET ?",
    )
    .bind(runner_id)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await?;
    let events = rows
        .into_iter()
        .map(|r| {
            json!({
                "id": r.get::<String, _>(0),
                "event_type": r.get::<String, _>(1),
                "message": r.get::<String, _>(2),
                "created_at": r.get::<Option<String>, _>(3),
            })
        })
        .collect();
    Ok(RunnerEventPage { events, total })
}

/// Outcome of the environment deployment-concurrency gate for one candidate.
pub enum EnvironmentGate {
    /// The candidate may be claimed. `superseded` lists jobs cancelled by a
    /// `cancel_in_progress` environment (empty otherwise).
    Proceed { superseded: Vec<String> },
    /// The candidate cannot be claimed in this round; leave it pending.
    Blocked,
}

/// Evaluate an environment's deployment-concurrency gate for `job_id` and, for
/// `cancel_in_progress`, cancel the in-flight siblings.
///
/// Must run inside the caller's `BEGIN IMMEDIATE` transaction that also
/// performs the claim: the write lock is what serialises two pollers
/// evaluating the same environment. `Blocked` is not an error and the
/// function has already left the transaction usable.
pub async fn environment_gate_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    job_id: &str,
    pipeline_id: &str,
    environment_id: &str,
) -> Result<EnvironmentGate, AppError> {
    use sqlx::Row;

    let deployment_concurrency: String = sqlx::query_scalar(
        "SELECT COALESCE(deployment_concurrency, 'off') FROM environments WHERE id = ?",
    )
    .bind(environment_id)
    .fetch_optional(&mut **tx)
    .await?
    .unwrap_or_else(|| "off".to_string());

    if deployment_concurrency == "cancel_in_progress" {
        // The newest pending job for the environment is the only claimant.
        let newest_pending: Option<String> = sqlx::query_scalar(
            "SELECT id FROM jobs WHERE environment_id = ? AND status = 'pending' \
             ORDER BY seq DESC LIMIT 1",
        )
        .bind(environment_id)
        .fetch_optional(&mut **tx)
        .await?;
        if newest_pending.as_deref() != Some(job_id) {
            return Ok(EnvironmentGate::Blocked);
        }

        let superseded_rows = sqlx::query(
            "SELECT id, status, step_statuses, COALESCE(snapshot, '{}') \
             FROM jobs WHERE environment_id = ? AND id != ? \
             AND status IN ('pending', 'queued', 'waiting_approval', 'running')",
        )
        .bind(environment_id)
        .bind(job_id)
        .fetch_all(&mut **tx)
        .await?;

        let mut superseded = Vec::new();
        for row in superseded_rows {
            let superseded_id: String = row.get(0);
            let old_status: String = row.get(1);
            let step_statuses_raw: String = row.get(2);
            let snapshot_raw: String = row.get(3);
            let mut step_statuses: Vec<serde_json::Value> =
                serde_json::from_str(&step_statuses_raw).unwrap_or_default();
            for step in &mut step_statuses {
                if step.get("status").and_then(serde_json::Value::as_str) == Some("running") {
                    step["status"] = serde_json::json!("cancelled");
                    step["finished_at"] = serde_json::json!(chrono::Utc::now().to_rfc3339());
                }
            }
            let mut snapshot: serde_json::Value =
                serde_json::from_str(&snapshot_raw).unwrap_or(serde_json::json!({}));
            if old_status == "waiting_approval" {
                let received = snapshot
                    .get("approval")
                    .and_then(|approval| approval.get("received"))
                    .and_then(serde_json::Value::as_i64)
                    .unwrap_or(0);
                if let Some(object) = snapshot.as_object_mut() {
                    object.insert(
                        "approval".into(),
                        serde_json::json!({
                            "required": true,
                            "received": received,
                            "outcome": "cancelled",
                        }),
                    );
                }
            }
            let updated = sqlx::query(
                "UPDATE jobs SET status = 'cancelled', step_statuses = ?, snapshot = ?, \
                 cancel_ack_pending = ?, \
                 end_time = datetime('now'), updated_at = datetime('now') \
                 WHERE id = ? AND status IN \
                 ('pending', 'queued', 'waiting_approval', 'running')",
            )
            .bind(serde_json::to_string(&step_statuses).unwrap_or_else(|_| "[]".into()))
            .bind(serde_json::to_string(&snapshot).unwrap_or_else(|_| "{}".into()))
            .bind(i64::from(old_status == "running"))
            .bind(&superseded_id)
            .execute(&mut **tx)
            .await?;
            if updated.rows_affected() > 0 {
                if old_status == "waiting_approval" {
                    sqlx::query(
                        "INSERT OR IGNORE INTO job_approvals \
                         (id, job_id, user_id, username, action, comment) \
                         VALUES (?, ?, 'system:environment', 'environment concurrency', \
                                 'cancel', 'superseded by cancel_in_progress')",
                    )
                    .bind(Uuid::new_v4().to_string())
                    .bind(&superseded_id)
                    .execute(&mut **tx)
                    .await?;
                }
                superseded.push(superseded_id);
            }
        }
        return Ok(EnvironmentGate::Proceed { superseded });
    }

    // One deployment per (pipeline, environment) at a time.
    let pair_running: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM jobs \
         WHERE pipeline_id = ? AND environment_id = ? AND status = 'running' AND id != ?",
    )
    .bind(pipeline_id)
    .bind(environment_id)
    .bind(job_id)
    .fetch_one(&mut **tx)
    .await?;
    if pair_running > 0 {
        return Ok(EnvironmentGate::Blocked);
    }

    // `queue` serialises across pipelines sharing the environment.
    if deployment_concurrency == "queue" {
        let env_running: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM jobs WHERE environment_id = ? AND status = 'running'",
        )
        .bind(environment_id)
        .fetch_one(&mut **tx)
        .await?;
        if env_running > 0 {
            return Ok(EnvironmentGate::Blocked);
        }
    }

    Ok(EnvironmentGate::Proceed {
        superseded: Vec::new(),
    })
}

// ───────────────────────────── registration plane ─────────────────────────────
//
// The register / heartbeat paths speak these statements; policy shaping and
// token verification stay in the handler (they are auth, not storage).

/// `(id, register_token, token)` of the row with this name — the registration
/// lookup (`None` = unknown name).
pub async fn register_row_by_name(
    pool: &SqlitePool,
    name: &str,
) -> Result<Option<(String, String, Option<String>)>, AppError> {
    let row = sqlx::query("SELECT id, register_token, token FROM runners WHERE name = ?")
        .bind(name)
        .fetch_optional(pool)
        .await?;
    Ok(row.map(|r| (r.get(0), r.get(1), r.get(2))))
}

/// Persist a fresh runner JWT (hash only, never plaintext), burn the one-shot
/// register token, flip online and record the reported deployment identities.
#[allow(clippy::too_many_arguments)]
pub async fn mark_registered(
    pool: &SqlitePool,
    runner_id: &str,
    token_hash: &str,
    version: Option<&str>,
    host_id: &str,
    runner_instance_id: &str,
) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE runners SET token = ?, register_token = '', status = 'online', \
         version = COALESCE(?, version), \
         host_id = COALESCE(NULLIF(?, ''), host_id), \
         runner_instance_id = COALESCE(NULLIF(?, ''), runner_instance_id), \
         last_seen = datetime('now'), updated_at = datetime('now'), \
         restart_requested = 0 WHERE id = ?",
    )
    .bind(token_hash)
    .bind(version)
    .bind(host_id)
    .bind(runner_instance_id)
    .bind(runner_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Labels JSON as stored (`"[]"` when absent).
pub async fn labels_of(pool: &SqlitePool, runner_id: &str) -> Result<String, AppError> {
    Ok(
        sqlx::query_scalar::<_, String>("SELECT labels FROM runners WHERE id = ?")
            .bind(runner_id)
            .fetch_optional(pool)
            .await?
            .unwrap_or_else(|| "[]".into()),
    )
}

/// Heartbeat write: status, system_info, version + deployment identities, and
/// the liveness stamps. Empty identities keep the stored ones (COALESCE guard).
#[allow(clippy::too_many_arguments)]
pub async fn mark_heartbeat(
    pool: &SqlitePool,
    runner_id: &str,
    status: &str,
    system_info_json: &str,
    version: Option<&str>,
    host_id: &str,
    runner_instance_id: &str,
    sandbox_instance_id: &str,
) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE runners SET status = ?, system_info = ?, version = COALESCE(?, version), \
         host_id = COALESCE(NULLIF(?, ''), host_id), \
         runner_instance_id = COALESCE(NULLIF(?, ''), runner_instance_id), \
         sandbox_instance_id = COALESCE(NULLIF(?, ''), sandbox_instance_id), \
         last_seen = datetime('now'), updated_at = datetime('now') WHERE id = ?",
    )
    .bind(status)
    .bind(system_info_json)
    .bind(version)
    .bind(host_id)
    .bind(runner_instance_id)
    .bind(sandbox_instance_id)
    .bind(runner_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// `(restart_requested, capability_reprobe_requested)` — the two one-shot
/// command flags served (and then cleared) by a heartbeat.
pub async fn pending_commands(
    pool: &SqlitePool,
    runner_id: &str,
) -> Result<(bool, bool), AppError> {
    let row: Option<(i64, i64)> = sqlx::query_as(
        "SELECT restart_requested, capability_reprobe_requested FROM runners WHERE id = ?",
    )
    .bind(runner_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|(r, c)| (r != 0, c != 0)).unwrap_or((false, false)))
}

/// Clear the consumed restart flag.
pub async fn clear_restart_requested(pool: &SqlitePool, runner_id: &str) -> Result<(), AppError> {
    sqlx::query("UPDATE runners SET restart_requested = 0 WHERE id = ?")
        .bind(runner_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Clear the consumed re-probe flag and stamp when it was served.
pub async fn clear_capability_reprobe_requested(
    pool: &SqlitePool,
    runner_id: &str,
) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE runners SET capability_reprobe_requested = 0, \
         capability_reprobe_served_at = datetime('now') WHERE id = ?",
    )
    .bind(runner_id)
    .execute(pool)
    .await?;
    Ok(())
}

// ───────────────────────────── admin plane ─────────────────────────────
//
// The UI manages one policy column at a time; the column names below are a
// closed set (`column` is never caller-supplied text), so the formatted SQL
// stays within the validator's declared dynamic shapes.

/// Replace one boolean policy column (`clean_workspace_after_job`,
/// `shell_enabled`, `ssh_enabled`, `docker_enabled`, `wasm_enabled`,
/// `netns_isolation`).
pub async fn update_flag(
    pool: &SqlitePool,
    id: &str,
    column: &str,
    value: bool,
) -> Result<(), AppError> {
    sqlx::query(&format!(
        "UPDATE runners SET {column} = ?, updated_at = datetime('now') WHERE id = ?"
    ))
    .bind(if value { 1i64 } else { 0 })
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Replace the labels JSON.
pub async fn update_labels(pool: &SqlitePool, id: &str, labels_json: &str) -> Result<(), AppError> {
    sqlx::query("UPDATE runners SET labels = ?, updated_at = datetime('now') WHERE id = ?")
        .bind(labels_json)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Replace the concurrency budget.
pub async fn update_max_concurrent(pool: &SqlitePool, id: &str, max: i64) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE runners SET max_concurrent_jobs = ?, updated_at = datetime('now') WHERE id = ?",
    )
    .bind(max)
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Replace the SSH host-key policy string (`known_hosts` | `insecure`).
pub async fn update_ssh_host_key_policy(
    pool: &SqlitePool,
    id: &str,
    policy: &str,
) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE runners SET ssh_host_key_policy = ?, updated_at = datetime('now') WHERE id = ?",
    )
    .bind(policy)
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Replace the claim strategy string (`backfill` | `fifo`).
pub async fn update_claim_strategy(
    pool: &SqlitePool,
    id: &str,
    strategy: &str,
) -> Result<(), AppError> {
    sqlx::query("UPDATE runners SET claim_strategy = ?, updated_at = datetime('now') WHERE id = ?")
        .bind(strategy)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Concurrency budget of one runner (`max_concurrent_jobs`, at least 1).
pub async fn max_concurrent_of(pool: &SqlitePool, id: &str) -> Result<i64, AppError> {
    Ok(
        sqlx::query_scalar::<_, i64>("SELECT max_concurrent_jobs FROM runners WHERE id = ?")
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap_or(1)
            .max(1),
    )
}

/// Jobs currently running on this runner — the capacity gate's occupancy.
pub async fn running_job_count(pool: &SqlitePool, id: &str) -> Result<i64, AppError> {
    Ok(sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM jobs WHERE runner_id = ? AND status = 'running'",
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .unwrap_or(0))
}

/// `(is_frozen, restart_requested)` — the poll dispatch gate.
pub async fn dispatch_gate(pool: &SqlitePool, id: &str) -> Result<(bool, bool), AppError> {
    let row: Option<(i64, i64)> =
        sqlx::query_as("SELECT is_frozen, restart_requested FROM runners WHERE id = ?")
            .bind(id)
            .fetch_optional(pool)
            .await?;
    Ok(row
        .map(|(frozen, restart)| (frozen != 0, restart != 0))
        .unwrap_or((false, false)))
}

/// Delivery gate for the long-poll loop (same two flags).
pub async fn long_poll_gate(pool: &SqlitePool, id: &str) -> Result<(i64, i64), AppError> {
    Ok(
        sqlx::query_as("SELECT is_frozen, restart_requested FROM runners WHERE id = ?")
            .bind(id)
            .fetch_optional(pool)
            .await?
            .unwrap_or((0, 0)),
    )
}

/// `(team_id, labels-json)` — the claim path's dispatch boundary and label set.
pub async fn team_and_labels(
    pool: &SqlitePool,
    id: &str,
) -> Result<(Option<String>, String), AppError> {
    let team_id: Option<String> = sqlx::query_scalar("SELECT team_id FROM runners WHERE id = ?")
        .bind(id)
        .fetch_one(pool)
        .await?;
    let labels_raw: Option<String> =
        sqlx::query_scalar::<_, String>("SELECT labels FROM runners WHERE id = ?")
            .bind(id)
            .fetch_optional(pool)
            .await?;
    Ok((team_id, labels_raw.unwrap_or_else(|| "[]".into())))
}

/// `(execution_mode, host_id, sandbox_instance_id, claim_strategy)` — the
/// slot scheduler's dispatch context.
pub async fn dispatch_context_row(
    pool: &SqlitePool,
    id: &str,
) -> Result<(String, String, String, String), AppError> {
    Ok(sqlx::query_as(
        "SELECT execution_mode, host_id, sandbox_instance_id, claim_strategy
         FROM runners WHERE id = ?",
    )
    .bind(id)
    .fetch_one(pool)
    .await?)
}

/// The registration queue: pending jobs this runner may claim, queue-fair
/// order, with the pipeline labels and the age used by the aging rule.
///
/// A job is excluded while the environment it targets has a sibling running
/// (chain rule), and `cancel_in_progress` environments offer only their oldest
/// pending job — the sibling cancellation happens inside the claim
/// transaction.
pub async fn claim_candidates(
    pool: &SqlitePool,
    runner_team_id: Option<&str>,
) -> Result<Vec<sqlx::sqlite::SqliteRow>, AppError> {
    Ok(sqlx::query(

        "SELECT j.id, j.pipeline_id, j.trigger_params, COALESCE(j.snapshot, '{}'), p.runner_labels,
                j.debug_unredact, j.verbose_logs, j.trigger_by, p.name,
                COALESCE(json_extract(j.snapshot, '$.team_id'), p.team_id), p.trigger_type,
                j.environment_id, j.run_number, j.run_attempt,
                CAST(strftime('%s', 'now') - strftime('%s', j.created_at) AS INTEGER) AS waited_seconds
         FROM jobs j
         JOIN pipelines p ON p.id = j.pipeline_id
         WHERE j.status = 'pending'
           AND (? IS NULL OR json_extract(j.snapshot, '$.team_id') = ?)
           AND (
             j.environment_id IS NULL
             OR COALESCE((SELECT deployment_concurrency FROM environments e WHERE e.id = j.environment_id), 'off') != 'cancel_in_progress'
             OR j.seq = (
                 SELECT MAX(latest.seq) FROM jobs latest
                 WHERE latest.environment_id = j.environment_id AND latest.status = 'pending'
             )
           )
           AND NOT (
             COALESCE((SELECT deployment_concurrency FROM environments e WHERE e.id = j.environment_id), 'off') = 'queue'
             AND EXISTS (SELECT 1 FROM jobs running WHERE running.environment_id = j.environment_id AND running.status = 'running')
           )
           AND NOT (
             COALESCE((SELECT deployment_concurrency FROM environments e WHERE e.id = j.environment_id), 'off') != 'cancel_in_progress'
             AND EXISTS (SELECT 1 FROM jobs running WHERE running.pipeline_id = j.pipeline_id AND running.environment_id = j.environment_id AND running.status = 'running')
           )
         ORDER BY j.created_at ASC, j.seq ASC
         LIMIT 50",
    )
    .bind(runner_team_id)
    .bind(runner_team_id)
    .fetch_all(pool)
    .await?)
}

/// True when the team row exists — the runner team binding check.
pub async fn team_exists(pool: &SqlitePool, team_id: &str) -> Result<bool, AppError> {
    Ok(
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM teams WHERE id = ?)")
            .bind(team_id)
            .fetch_one(pool)
            .await?,
    )
}

/// `team_id` of one runner (`None` = shared/unassigned or absent row).
pub async fn team_of(pool: &SqlitePool, runner_id: &str) -> Result<Option<String>, AppError> {
    Ok(
        sqlx::query_scalar("SELECT team_id FROM runners WHERE id = ?")
            .bind(runner_id)
            .fetch_one(pool)
            .await?,
    )
}

/// How many runner rows carry this exact name (standalone idempotence check).
pub async fn count_by_name(pool: &SqlitePool, name: &str) -> Result<i64, AppError> {
    Ok(
        sqlx::query_scalar("SELECT COUNT(1) FROM runners WHERE name = ?")
            .bind(name)
            .fetch_one(pool)
            .await?,
    )
}

/// Mark a pending job failed because its resource-profile grant was revoked
/// while it was queued (the claim path's terminal write).
pub async fn fail_pending_job(pool: &SqlitePool, job_id: &str) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE jobs SET status = 'failed', end_time = datetime('now'),
         updated_at = datetime('now') WHERE id = ? AND status = 'pending'",
    )
    .bind(job_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// The policy columns of one runner, raw (`None` = no such row). Column order:
/// max_concurrent_jobs, clean_workspace_after_job, shell_enabled,
/// ssh_enabled, docker_enabled, wasm_enabled, ssh_host_key_policy,
/// execution_mode, claim_strategy, netns_isolation.
pub async fn policy_row(
    pool: &SqlitePool,
    runner_id: &str,
) -> Result<Option<(i64, i64, i64, i64, i64, i64, String, String, String, i64)>, AppError> {
    Ok(sqlx::query_as(
        "SELECT max_concurrent_jobs, clean_workspace_after_job,
                shell_enabled, ssh_enabled, docker_enabled, wasm_enabled, ssh_host_key_policy,
                execution_mode, claim_strategy, netns_isolation
         FROM runners WHERE id = ?",
    )
    .bind(runner_id)
    .fetch_optional(pool)
    .await?)
}

/// The claim's write: flip the job to running and stamp the dispatch facts.
/// Guarded by `status = 'pending'`, so a job another poller just took comes back
/// `None` instead of being claimed twice. Returns the frozen run identity:
/// `(id, pipeline_id, trigger_params, environment_id, run_number, run_attempt)`.
pub async fn claim_job_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    runner_id: &str,
    job_id: &str,
    slots: i64,
    resource_spec: &str,
    sandbox_key: &str,
) -> Result<Option<(String, String, Option<String>, Option<String>, i64, i64)>, AppError> {
    Ok(sqlx::query_as(
        "UPDATE jobs SET status = 'running', runner_id = ?, start_time = datetime('now'),
         resource_slots = ?, resource_spec = ?, sandbox_instance_id = ?, dispatch_reason = '',
         updated_at = datetime('now')
         WHERE id = ? AND status = 'pending'
         RETURNING id, pipeline_id, trigger_params, environment_id, run_number, run_attempt",
    )
    .bind(runner_id)
    .bind(slots)
    .bind(resource_spec)
    .bind(sandbox_key)
    .bind(job_id)
    .fetch_optional(&mut **tx)
    .await?)
}

/// The claim decision's re-read, inside the caller's `BEGIN IMMEDIATE` scope:
/// `(pipeline_id, environment_id, runner_team_id, job_team_id)`. `None` when
/// the job is gone or no longer pending (the caller rolls back and moves on).
pub async fn claim_candidate_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    runner_id: &str,
    job_id: &str,
) -> Result<Option<(String, Option<String>, Option<String>, Option<String>)>, AppError> {
    let row = sqlx::query(
        "SELECT j.pipeline_id, j.environment_id, r.team_id, \
                json_extract(j.snapshot, '$.team_id') \
         FROM jobs j JOIN runners r ON r.id = ? \
         WHERE j.id = ? AND j.status = 'pending'",
    )
    .bind(runner_id)
    .bind(job_id)
    .fetch_optional(&mut **tx)
    .await?;
    Ok(row.map(|r| (r.get(0), r.get(1), r.get(2), r.get(3))))
}
