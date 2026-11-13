//! Runner rows: admin management operations and the bookkeeping writes the
//! poller performs on its own runner.
//!
//! The claim decision itself (candidate selection, slot gate, environment
//! gates, cancel_in_progress supersede) stays in `api/runners.rs`: it is
//! interleaved with a loop's control flow and moves in a separate step.

use serde_json::{json, Value};
use sqlx::Row;
use uuid::Uuid;

use crate::store::exec::{self, DbPool, DbTx, SqlValue};

use shared::error::AppError;
use shared::protocol::{ExecutionBackendStatus, RunnerExecutionMode, SystemInfo};

/// Why the last poll round skipped a pending job. Display-only, cleared on
/// claim. Logged only when the reason changes, so a job parked on one reason
/// does not repeat itself every poll round.
pub async fn record_dispatch_reason(pool: &DbPool, job_id: &str, reason: &str) {
    match exec::execute(
        pool,
        "UPDATE jobs SET dispatch_reason = ? WHERE id = ? AND status = 'pending'
         AND COALESCE(dispatch_reason, '') != ?",
        &[reason.into(), job_id.into(), reason.into()],
    )
    .await
    {
        Ok(affected) if affected > 0 => {
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
pub async fn log_runner_event(pool: &DbPool, runner_id: &str, event_type: &str, message: &str) {
    let id = Uuid::new_v4().to_string();
    if let Err(e) = exec::execute(
        pool,
        "INSERT INTO runner_events (id, runner_id, event_type, message) VALUES (?, ?, ?, ?)",
        &[
            id.as_str().into(),
            runner_id.into(),
            event_type.into(),
            message.into(),
        ],
    )
    .await
    {
        tracing::warn!("Failed to log runner event for {runner_id}: {e}");
    }
}

/// `(id, name)` of online, unfrozen runners whose last heartbeat is older
/// than `timeout_secs` — the monitor's stale set.
pub async fn stale_online_runners(
    pool: &DbPool,
    timeout_secs: i64,
) -> Result<Vec<(String, String)>, AppError> {
    let cutoff = format!("-{timeout_secs} seconds");
    Ok(exec::fetch_all(
        pool,
        "SELECT id, name FROM runners
         WHERE status = 'online'
           AND is_frozen = 0
           AND last_seen < datetime('now', ?)",
        &[cutoff.into()],
    )
    .await?
    .into_iter()
    .map(|r| (r.text(0), r.text(1)))
    .collect())
}

/// Flip the stale set to `offline`; returns the rows affected.
pub async fn mark_stale_runners_offline(pool: &DbPool, timeout_secs: i64) -> Result<u64, AppError> {
    let cutoff = format!("-{timeout_secs} seconds");
    Ok(exec::execute(
        pool,
        "UPDATE runners
         SET status = 'offline',
             updated_at = datetime('now')
         WHERE status = 'online'
           AND is_frozen = 0
           AND last_seen < datetime('now', ?)",
        &[cutoff.into()],
    )
    .await?)
}

/// Ids of jobs still running/pending on a runner that just went offline.
pub async fn orphaned_job_ids(pool: &DbPool, runner_id: &str) -> Result<Vec<String>, AppError> {
    Ok(exec::fetch_all(
        pool,
        "SELECT id FROM jobs WHERE runner_id = ? AND status IN ('running', 'pending')",
        &[runner_id.into()],
    )
    .await?
    .into_iter()
    .map(|r| r.text(0))
    .collect())
}

/// `(name, team_id, status)` snapshot taken before a row is deleted: the audit
/// record outlives the row and is the last description of it.
pub async fn runner_snapshot(
    pool: &DbPool,
    id: &str,
) -> Result<Option<(String, Option<String>, String)>, AppError> {
    Ok(exec::fetch_optional(
        pool,
        "SELECT name, team_id, status FROM runners WHERE id = ?",
        &[id.into()],
    )
    .await?
    .map(|r| (r.text(0), r.opt_text(1), r.text(2))))
}

/// Delete one runner. False when no row matched.
///
/// `jobs.runner_id` carries a plain FK to `runners(id)` with no `ON DELETE`
/// action, so the referencing rows must be cleared or the delete fails.
pub async fn delete_runner(pool: &DbPool, id: &str) -> Result<bool, AppError> {
    exec::execute(
        pool,
        "UPDATE jobs SET runner_id = NULL WHERE runner_id = ?",
        &[id.into()],
    )
    .await?;
    let affected = exec::execute(pool, "DELETE FROM runners WHERE id = ?", &[id.into()]).await?;
    Ok(affected != 0)
}

/// Flip the freeze flag: `(name, was_frozen, now_frozen)`.
///
/// Read-modify-write in one `BEGIN IMMEDIATE`, so two concurrent toggles cannot
/// both read the same value and write the same result.
pub async fn toggle_freeze(pool: &DbPool, id: &str) -> Result<(String, bool, bool), AppError> {
    let mut tx = pool.begin_immediate().await?;
    let row = tx
        .fetch_optional(
            "SELECT name, is_frozen FROM runners WHERE id = ?",
            &[id.into()],
        )
        .await?
        .ok_or_else(|| AppError::NotFound("runner not found".into()))?;
    let name = row.text(0);
    let was_frozen = row.i64(1) != 0;
    let now_frozen = !was_frozen;
    tx.execute(
        "UPDATE runners SET is_frozen = ?, updated_at = datetime('now') WHERE id = ?",
        &[SqlValue::Int(i64::from(now_frozen)), id.into()],
    )
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

pub async fn runner_restart_state(pool: &DbPool, id: &str) -> Result<RunnerRestartState, AppError> {
    let name = exec::fetch_optional(pool, "SELECT name FROM runners WHERE id = ?", &[id.into()])
        .await?
        .map(|r| r.text(0));
    let running_job_ids = exec::fetch_all(
        pool,
        "SELECT id FROM jobs WHERE runner_id = ? AND status = 'running' ORDER BY created_at",
        &[id.into()],
    )
    .await?
    .into_iter()
    .map(|r| r.text(0))
    .collect();
    Ok(RunnerRestartState {
        name: name.ok_or_else(|| AppError::NotFound("runner not found".into()))?,
        running_job_ids,
    })
}

/// Set the restart flag served on the runner's next heartbeat.
pub async fn request_restart(pool: &DbPool, id: &str) -> Result<(), AppError> {
    exec::execute(
        pool,
        "UPDATE runners SET restart_requested = 1, updated_at = datetime('now') WHERE id = ?",
        &[id.into()],
    )
    .await?;
    crate::services::poll_events::wake_all();
    Ok(())
}

/// Set the one-shot capability re-probe flag.
pub async fn request_capability_reprobe(pool: &DbPool, id: &str) -> Result<(), AppError> {
    exec::execute(
        pool,
        "UPDATE runners SET capability_reprobe_requested = 1,
            capability_reprobe_requested_at = datetime('now'),
            capability_reprobe_served_at = NULL,
            updated_at = datetime('now') WHERE id = ?",
        &[id.into()],
    )
    .await?;
    Ok(())
}

/// Replace the one-shot register token. Returns the runner name, or `None`
/// when the row does not exist received.
///
/// The caller passes an already-hashed token: plaintext is never stored.
pub async fn reissue_register_token(
    pool: &DbPool,
    id: &str,
    token_hash: &str,
) -> Result<Option<String>, AppError> {
    Ok(exec::fetch_optional(
        pool,
        "UPDATE runners SET register_token = ?, updated_at = datetime('now') WHERE id = ? RETURNING name",
        &[token_hash.into(), id.into()],
    )
    .await?
    .map(|r| r.text(0)))
}

/// Runner name by id (`None` when absent).
pub async fn runner_name(pool: &DbPool, id: &str) -> Result<Option<String>, AppError> {
    Ok(
        exec::fetch_optional(pool, "SELECT name FROM runners WHERE id = ?", &[id.into()])
            .await?
            .map(|r| r.text(0)),
    )
}

/// True when the runner row exists.
pub async fn exists(pool: &DbPool, id: &str) -> Result<bool, AppError> {
    let count = exec::fetch_scalar_i64(
        pool,
        "SELECT COUNT(*) FROM runners WHERE id = ?",
        &[id.into()],
    )
    .await?;
    Ok(count != 0)
}

/// One page of a runner's event history plus the total count.
pub struct RunnerEventPage {
    pub events: Vec<Value>,
    pub total: i64,
}

pub async fn event_page(
    pool: &DbPool,
    runner_id: &str,
    limit: i64,
    offset: i64,
) -> Result<RunnerEventPage, AppError> {
    let total = exec::fetch_scalar_i64(
        pool,
        "SELECT COUNT(*) FROM runner_events WHERE runner_id = ?",
        &[runner_id.into()],
    )
    .await?;
    let rows = exec::fetch_all(
        pool,
        "SELECT id, event_type, message, created_at
         FROM runner_events
         WHERE runner_id = ?
         ORDER BY created_at DESC
         LIMIT ? OFFSET ?",
        &[
            runner_id.into(),
            SqlValue::Int(limit),
            SqlValue::Int(offset),
        ],
    )
    .await?;
    let events = rows
        .into_iter()
        .map(|r| {
            json!({
                "id": r.text(0),
                "event_type": r.text(1),
                "message": r.text(2),
                "created_at": r.opt_text(3),
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
    tx: &mut DbTx<'_>,
    job_id: &str,
    pipeline_id: &str,
    environment_id: &str,
) -> Result<EnvironmentGate, AppError> {
    let deployment_concurrency = tx
        .fetch_optional(
            "SELECT COALESCE(deployment_concurrency, 'off') FROM environments WHERE id = ?",
            &[environment_id.into()],
        )
        .await?
        .map(|r| r.text(0))
        .unwrap_or_else(|| "off".to_string());

    if deployment_concurrency == "cancel_in_progress" {
        // The newest pending job for the environment is the only claimant.
        let newest_pending = tx
            .fetch_optional(
                "SELECT id FROM jobs WHERE environment_id = ? AND status = 'pending' \
             ORDER BY seq DESC LIMIT 1",
                &[environment_id.into()],
            )
            .await?
            .map(|r| r.text(0));
        if newest_pending.as_deref() != Some(job_id) {
            return Ok(EnvironmentGate::Blocked);
        }

        let superseded_rows = tx
            .fetch_all(
                "SELECT id, status, step_statuses, COALESCE(snapshot, '{}') \
             FROM jobs WHERE environment_id = ? AND id != ? \
             AND status IN ('pending', 'queued', 'waiting_approval', 'running')",
                &[environment_id.into(), job_id.into()],
            )
            .await?;

        let mut superseded = Vec::new();
        for row in superseded_rows {
            let superseded_id: String = row.text(0);
            let old_status: String = row.text(1);
            let step_statuses_raw: String = row.text(2);
            let snapshot_raw: String = row.text(3);
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
            let updated = tx
                .execute(
                    "UPDATE jobs SET status = 'cancelled', step_statuses = ?, snapshot = ?, \
                 cancel_ack_pending = ?, \
                 end_time = datetime('now'), updated_at = datetime('now') \
                 WHERE id = ? AND status IN \
                 ('pending', 'queued', 'waiting_approval', 'running')",
                    &[
                        serde_json::to_string(&step_statuses)
                            .unwrap_or_else(|_| "[]".into())
                            .into(),
                        serde_json::to_string(&snapshot)
                            .unwrap_or_else(|_| "{}".into())
                            .into(),
                        SqlValue::Int(i64::from(old_status == "running")),
                        superseded_id.as_str().into(),
                    ],
                )
                .await?;
            if updated > 0 {
                if old_status == "waiting_approval" {
                    tx.execute(
                        "INSERT OR IGNORE INTO job_approvals \
                         (id, job_id, user_id, username, action, comment) \
                         VALUES (?, ?, 'system:environment', 'environment concurrency', \
                                 'cancel', 'superseded by cancel_in_progress')",
                        &[
                            Uuid::new_v4().to_string().into(),
                            superseded_id.as_str().into(),
                        ],
                    )
                    .await?;
                }
                superseded.push(superseded_id);
            }
        }
        return Ok(EnvironmentGate::Proceed { superseded });
    }

    // One deployment per (pipeline, environment) at a time.
    let pair_running = tx
        .fetch_optional(
            "SELECT COUNT(*) FROM jobs \
         WHERE pipeline_id = ? AND environment_id = ? AND status = 'running' AND id != ?",
            &[pipeline_id.into(), environment_id.into(), job_id.into()],
        )
        .await?
        .map(|r| r.i64(0))
        .unwrap_or(0);
    if pair_running > 0 {
        return Ok(EnvironmentGate::Blocked);
    }

    // `queue` serialises across pipelines sharing the environment.
    if deployment_concurrency == "queue" {
        let env_running = tx
            .fetch_optional(
                "SELECT COUNT(*) FROM jobs WHERE environment_id = ? AND status = 'running'",
                &[environment_id.into()],
            )
            .await?
            .map(|r| r.i64(0))
            .unwrap_or(0);
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
    pool: &DbPool,
    name: &str,
) -> Result<Option<(String, String, Option<String>)>, AppError> {
    Ok(exec::fetch_optional(
        pool,
        "SELECT id, register_token, token FROM runners WHERE name = ?",
        &[name.into()],
    )
    .await?
    .map(|r| (r.text(0), r.text(1), r.opt_text(2))))
}

/// Persist a fresh runner JWT (hash only, never plaintext), burn the one-shot
/// register token, flip online and record the reported deployment identities.
#[allow(clippy::too_many_arguments)]
pub async fn mark_registered(
    pool: &DbPool,
    runner_id: &str,
    token_hash: &str,
    version: Option<&str>,
    host_id: &str,
    runner_instance_id: &str,
) -> Result<(), AppError> {
    exec::execute(
        pool,
        "UPDATE runners SET token = ?, register_token = '', status = 'online', \
         version = COALESCE(?, version), \
         host_id = COALESCE(NULLIF(?, ''), host_id), \
         runner_instance_id = COALESCE(NULLIF(?, ''), runner_instance_id), \
         last_seen = datetime('now'), updated_at = datetime('now'), \
         restart_requested = 0 WHERE id = ?",
        &[
            token_hash.into(),
            version.into(),
            host_id.into(),
            runner_instance_id.into(),
            runner_id.into(),
        ],
    )
    .await?;
    Ok(())
}

/// Labels JSON as stored (`"[]"` when absent).
pub async fn labels_of(pool: &DbPool, runner_id: &str) -> Result<String, AppError> {
    Ok(exec::fetch_optional(
        pool,
        "SELECT labels FROM runners WHERE id = ?",
        &[runner_id.into()],
    )
    .await?
    .map(|r| r.text(0))
    .unwrap_or_else(|| "[]".into()))
}

/// Heartbeat write: status, system_info, version + deployment identities, and
/// the liveness stamps. Empty identities keep the stored ones (COALESCE guard).
#[allow(clippy::too_many_arguments)]
pub async fn mark_heartbeat(
    pool: &DbPool,
    runner_id: &str,
    status: &str,
    system_info_json: &str,
    version: Option<&str>,
    host_id: &str,
    runner_instance_id: &str,
    sandbox_instance_id: &str,
) -> Result<(), AppError> {
    exec::execute(
        pool,
        "UPDATE runners SET status = ?, system_info = ?, version = COALESCE(?, version), \
         host_id = COALESCE(NULLIF(?, ''), host_id), \
         runner_instance_id = COALESCE(NULLIF(?, ''), runner_instance_id), \
         sandbox_instance_id = COALESCE(NULLIF(?, ''), sandbox_instance_id), \
         last_seen = datetime('now'), updated_at = datetime('now') WHERE id = ?",
        &[
            status.into(),
            system_info_json.into(),
            version.into(),
            host_id.into(),
            runner_instance_id.into(),
            sandbox_instance_id.into(),
            runner_id.into(),
        ],
    )
    .await?;
    Ok(())
}

/// `(restart_requested, capability_reprobe_requested)` — the two one-shot
/// command flags served (and then cleared) by a heartbeat.
pub async fn pending_commands(pool: &DbPool, runner_id: &str) -> Result<(bool, bool), AppError> {
    Ok(exec::fetch_optional(
        pool,
        "SELECT restart_requested, capability_reprobe_requested FROM runners WHERE id = ?",
        &[runner_id.into()],
    )
    .await?
    .map(|r| (r.i64(0) != 0, r.i64(1) != 0))
    .unwrap_or((false, false)))
}

/// Clear the consumed restart flag.
pub async fn clear_restart_requested(pool: &DbPool, runner_id: &str) -> Result<(), AppError> {
    exec::execute(
        pool,
        "UPDATE runners SET restart_requested = 0 WHERE id = ?",
        &[runner_id.into()],
    )
    .await?;
    Ok(())
}

/// Clear the consumed re-probe flag and stamp when it was served.
pub async fn clear_capability_reprobe_requested(
    pool: &DbPool,
    runner_id: &str,
) -> Result<(), AppError> {
    exec::execute(
        pool,
        "UPDATE runners SET capability_reprobe_requested = 0, \
         capability_reprobe_served_at = datetime('now') WHERE id = ?",
        &[runner_id.into()],
    )
    .await?;
    Ok(())
}

// ───────────────────────────── admin plane ─────────────────────────────
//
// One function per editable field: the column is a literal in each statement,
// so no SQL text is ever assembled from a variable. A generic
// `update(column)` would reintroduce an identifier-injection surface for no
// benefit (`UPDATE runners SET {column} …`).

/// A new runner row. `register_token_hash` is SHA-256 — the plaintext token
/// exists only in the create response and is never persisted.
pub struct NewRunner<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub register_token_hash: &'a str,
    pub labels_json: &'a str,
    pub version: Option<&'a str>,
    pub team_id: Option<&'a str>,
    pub execution_mode: &'a str,
    pub max_concurrent_jobs: i64,
    pub clean_workspace: i64,
    pub shell: i64,
    pub ssh: i64,
    pub docker: i64,
    pub wasm: i64,
    pub ssh_host_key_policy: &'a str,
    pub claim_strategy: &'a str,
}

/// Insert one runner row; it starts `offline` and goes online at first
/// successful register.
pub async fn insert(pool: &DbPool, r: &NewRunner<'_>) -> Result<(), AppError> {
    exec::execute(
        pool,
        "INSERT INTO runners (id, name, register_token, labels, status, version, team_id,
         execution_mode, max_concurrent_jobs, clean_workspace_after_job,
         shell_enabled, ssh_enabled, docker_enabled, wasm_enabled, ssh_host_key_policy, claim_strategy)
         VALUES (?, ?, ?, ?, 'offline', ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        &[
            r.id.into(),
            r.name.into(),
            r.register_token_hash.into(),
            r.labels_json.into(),
            r.version.into(),
            r.team_id.into(),
            r.execution_mode.into(),
            SqlValue::Int(r.max_concurrent_jobs),
            SqlValue::Int(r.clean_workspace),
            SqlValue::Int(r.shell),
            SqlValue::Int(r.ssh),
            SqlValue::Int(r.docker),
            SqlValue::Int(r.wasm),
            r.ssh_host_key_policy.into(),
            r.claim_strategy.into(),
        ],
    )
    .await?;
    Ok(())
}

/// Replace the `clean_workspace_after_job` flag.
pub async fn update_clean_workspace(pool: &DbPool, id: &str, value: bool) -> Result<(), AppError> {
    exec::execute(
        pool,
        "UPDATE runners SET clean_workspace_after_job = ?, updated_at = datetime('now') WHERE id = ?",
        &[SqlValue::Int(value as i64), id.into()],
    )
    .await?;
    Ok(())
}

/// Replace the `shell_enabled` flag.
pub async fn update_shell_enabled(pool: &DbPool, id: &str, value: bool) -> Result<(), AppError> {
    exec::execute(
        pool,
        "UPDATE runners SET shell_enabled = ?, updated_at = datetime('now') WHERE id = ?",
        &[SqlValue::Int(value as i64), id.into()],
    )
    .await?;
    Ok(())
}

/// Replace the `ssh_enabled` flag.
pub async fn update_ssh_enabled(pool: &DbPool, id: &str, value: bool) -> Result<(), AppError> {
    exec::execute(
        pool,
        "UPDATE runners SET ssh_enabled = ?, updated_at = datetime('now') WHERE id = ?",
        &[SqlValue::Int(value as i64), id.into()],
    )
    .await?;
    Ok(())
}

/// Replace the `docker_enabled` flag.
pub async fn update_docker_enabled(pool: &DbPool, id: &str, value: bool) -> Result<(), AppError> {
    exec::execute(
        pool,
        "UPDATE runners SET docker_enabled = ?, updated_at = datetime('now') WHERE id = ?",
        &[SqlValue::Int(value as i64), id.into()],
    )
    .await?;
    Ok(())
}

/// Replace the `wasm_enabled` flag.
pub async fn update_wasm_enabled(pool: &DbPool, id: &str, value: bool) -> Result<(), AppError> {
    exec::execute(
        pool,
        "UPDATE runners SET wasm_enabled = ?, updated_at = datetime('now') WHERE id = ?",
        &[SqlValue::Int(value as i64), id.into()],
    )
    .await?;
    Ok(())
}

/// Replace the `netns_isolation` flag.
pub async fn update_netns_isolation(pool: &DbPool, id: &str, value: bool) -> Result<(), AppError> {
    exec::execute(
        pool,
        "UPDATE runners SET netns_isolation = ?, updated_at = datetime('now') WHERE id = ?",
        &[SqlValue::Int(value as i64), id.into()],
    )
    .await?;
    Ok(())
}

/// Current `execution_mode` string (errors when the row is gone).
pub async fn execution_mode_of(pool: &DbPool, id: &str) -> Result<String, AppError> {
    Ok(exec::fetch_optional(
        pool,
        "SELECT execution_mode FROM runners WHERE id = ?",
        &[id.into()],
    )
    .await?
    .ok_or(sqlx::Error::RowNotFound)?
    .text(0))
}

/// The team-binding move, guarded: refuses while any job of this runner is
/// running, and only writes when the binding actually changes. One
/// `BEGIN IMMEDIATE` so the guard cannot be raced by a concurrent claim.
/// Returns the previous binding.
pub async fn move_team(
    pool: &DbPool,
    id: &str,
    new_team_id: Option<&str>,
) -> Result<Option<String>, AppError> {
    let mut tx = pool
        .begin_immediate()
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;
    let current = tx
        .fetch_optional("SELECT team_id FROM runners WHERE id = ?", &[id.into()])
        .await?
        .ok_or(sqlx::Error::RowNotFound)?
        .opt_text(0);
    if current.as_deref() != new_team_id {
        let running = tx
            .fetch_optional(
                "SELECT COUNT(*) FROM jobs WHERE runner_id = ? AND status = 'running'",
                &[id.into()],
            )
            .await?
            .map(|r| r.i64(0))
            .unwrap_or(0);
        if running > 0 {
            tx.rollback().await.ok();
            return Err(AppError::Conflict(format!(
                "runner team binding cannot change while {running} job(s) are running"
            )));
        }
        tx.execute(
            "UPDATE runners SET team_id = ?, updated_at = datetime('now') WHERE id = ?",
            &[new_team_id.into(), id.into()],
        )
        .await?;
    }
    tx.commit()
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;
    Ok(current)
}

/// The post-update identity row for the audit diff:
/// `(id, name, labels, status, version, system_info, last_seen, created_at,
/// updated_at, team_id)`.
pub type RunnerIdentityRow = (
    String,
    String,
    String,
    String,
    Option<String>,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

pub async fn identity_row(pool: &DbPool, id: &str) -> Result<RunnerIdentityRow, AppError> {
    let r = exec::fetch_optional(
        pool,
        "SELECT id, name, labels, status, version, system_info, last_seen,
                created_at, updated_at, team_id FROM runners WHERE id = ?",
        &[id.into()],
    )
    .await?
    .ok_or(sqlx::Error::RowNotFound)?;
    Ok((
        r.text(0),
        r.text(1),
        r.text(2),
        r.text(3),
        r.opt_text(4),
        r.text(5),
        r.opt_text(6),
        r.opt_text(7),
        r.opt_text(8),
        r.opt_text(9),
    ))
}

/// Replace the execution mode (caller validated the transition first, inside
/// its own `BEGIN IMMEDIATE`).
pub async fn set_execution_mode_tx(
    tx: &mut DbTx<'_>,
    id: &str,
    mode: &str,
) -> Result<(), AppError> {
    tx.execute(
        "UPDATE runners SET execution_mode = ?, updated_at = datetime('now') WHERE id = ?",
        &[mode.into(), id.into()],
    )
    .await?;
    Ok(())
}

/// Current execution mode inside the caller's transaction.
pub async fn execution_mode_tx(tx: &mut DbTx<'_>, id: &str) -> Result<String, AppError> {
    Ok(tx
        .fetch_optional(
            "SELECT execution_mode FROM runners WHERE id = ?",
            &[id.into()],
        )
        .await?
        .ok_or(sqlx::Error::RowNotFound)?
        .text(0))
}

/// `(status, last_seen, system_info)` inside the caller's transaction — the
/// execution-mode transition validator's facts.
pub async fn transition_facts_tx(
    tx: &mut DbTx<'_>,
    id: &str,
) -> Result<(String, Option<String>, String), AppError> {
    Ok(tx
        .fetch_optional(
            "SELECT status, last_seen, system_info FROM runners WHERE id = ?",
            &[id.into()],
        )
        .await?
        .ok_or(sqlx::Error::RowNotFound)
        .map(|r| (r.text(0), r.opt_text(1), r.text(2)))?)
}

/// Was the runner seen within the last 90 seconds? (heartbeat freshness)
///
/// Both probes are separate statements with literal offsets — no caller text
/// ever reaches the SQL, so `datetime()` modifiers stay constant.
pub async fn seen_within_90s_tx(
    tx: &mut DbTx<'_>,
    last_seen: Option<&str>,
) -> Result<bool, AppError> {
    let value = tx
        .fetch_optional(
            "SELECT COALESCE(datetime(?) >= datetime('now', '-90 seconds'), 0)",
            &[last_seen.into()],
        )
        .await?
        .map(|r| r.i64(0))
        .unwrap_or(0);
    Ok(value != 0)
}

/// Is the reported capability check fresh (within 5 minutes, not from the
/// future)? Same shape as [`seen_within_90s_tx`].
pub async fn capability_check_fresh_tx(
    tx: &mut DbTx<'_>,
    checked_at: Option<&str>,
) -> Result<bool, AppError> {
    let value = tx
        .fetch_optional(
            "SELECT COALESCE(datetime(?) >= datetime('now', '-5 minutes') \
         AND datetime(?) <= datetime('now', '+30 seconds'), 0)",
            &[checked_at.into(), checked_at.into()],
        )
        .await?
        .map(|r| r.i64(0))
        .unwrap_or(0);
    Ok(value != 0)
}

/// Replace the labels JSON.
pub async fn update_labels(pool: &DbPool, id: &str, labels_json: &str) -> Result<(), AppError> {
    exec::execute(
        pool,
        "UPDATE runners SET labels = ?, updated_at = datetime('now') WHERE id = ?",
        &[labels_json.into(), id.into()],
    )
    .await?;
    Ok(())
}

/// Replace the concurrency budget.
pub async fn update_max_concurrent(pool: &DbPool, id: &str, max: i64) -> Result<(), AppError> {
    exec::execute(
        pool,
        "UPDATE runners SET max_concurrent_jobs = ?, updated_at = datetime('now') WHERE id = ?",
        &[SqlValue::Int(max), id.into()],
    )
    .await?;
    Ok(())
}

/// Replace the SSH host-key policy string (`known_hosts` | `insecure`).
pub async fn update_ssh_host_key_policy(
    pool: &DbPool,
    id: &str,
    policy: &str,
) -> Result<(), AppError> {
    exec::execute(
        pool,
        "UPDATE runners SET ssh_host_key_policy = ?, updated_at = datetime('now') WHERE id = ?",
        &[policy.into(), id.into()],
    )
    .await?;
    Ok(())
}

/// Replace the claim strategy string (`backfill` | `fifo`).
pub async fn update_claim_strategy(
    pool: &DbPool,
    id: &str,
    strategy: &str,
) -> Result<(), AppError> {
    exec::execute(
        pool,
        "UPDATE runners SET claim_strategy = ?, updated_at = datetime('now') WHERE id = ?",
        &[strategy.into(), id.into()],
    )
    .await?;
    Ok(())
}

/// Concurrency budget of one runner (`max_concurrent_jobs`, at least 1).
pub async fn max_concurrent_of(pool: &DbPool, id: &str) -> Result<i64, AppError> {
    Ok(exec::fetch_scalar_i64(
        pool,
        "SELECT max_concurrent_jobs FROM runners WHERE id = ?",
        &[id.into()],
    )
    .await
    .unwrap_or(1)
    .max(1))
}

/// Running-job count inside the caller's transaction (the execution-mode
/// transition guard reads it under the same `BEGIN IMMEDIATE`).
pub async fn running_job_count_tx(tx: &mut DbTx<'_>, id: &str) -> Result<i64, AppError> {
    Ok(tx
        .fetch_optional(
            "SELECT COUNT(*) FROM jobs WHERE runner_id = ? AND status = 'running'",
            &[id.into()],
        )
        .await?
        .map(|r| r.i64(0))
        .unwrap_or(0))
}

/// Jobs currently running on this runner — the capacity gate's occupancy.
pub async fn running_job_count(pool: &DbPool, id: &str) -> Result<i64, AppError> {
    Ok(exec::fetch_scalar_i64(
        pool,
        "SELECT COUNT(*) FROM jobs WHERE runner_id = ? AND status = 'running'",
        &[id.into()],
    )
    .await
    .unwrap_or(0))
}

/// `(is_frozen, restart_requested)` — the poll dispatch gate.
pub async fn dispatch_gate(pool: &DbPool, id: &str) -> Result<(bool, bool), AppError> {
    Ok(exec::fetch_optional(
        pool,
        "SELECT is_frozen, restart_requested FROM runners WHERE id = ?",
        &[id.into()],
    )
    .await?
    .map(|r| (r.i64(0) != 0, r.i64(1) != 0))
    .unwrap_or((false, false)))
}

/// Delivery gate for the long-poll loop (same two flags).
pub async fn long_poll_gate(pool: &DbPool, id: &str) -> Result<(i64, i64), AppError> {
    Ok(exec::fetch_optional(
        pool,
        "SELECT is_frozen, restart_requested FROM runners WHERE id = ?",
        &[id.into()],
    )
    .await?
    .map(|r| (r.i64(0), r.i64(1)))
    .unwrap_or((0, 0)))
}

/// `(team_id, labels-json)` — the claim path's dispatch boundary and label set.
pub async fn team_and_labels(
    pool: &DbPool,
    id: &str,
) -> Result<(Option<String>, String), AppError> {
    let team_id = exec::fetch_optional(
        pool,
        "SELECT team_id FROM runners WHERE id = ?",
        &[id.into()],
    )
    .await?
    .ok_or(sqlx::Error::RowNotFound)?
    .opt_text(0);
    let labels_raw = exec::fetch_optional(
        pool,
        "SELECT labels FROM runners WHERE id = ?",
        &[id.into()],
    )
    .await?
    .map(|r| r.text(0));
    Ok((team_id, labels_raw.unwrap_or_else(|| "[]".into())))
}

/// `(execution_mode, host_id, sandbox_instance_id, claim_strategy)` — the
/// slot scheduler's dispatch context.
pub async fn dispatch_context_row(
    pool: &DbPool,
    id: &str,
) -> Result<(String, String, String, String), AppError> {
    let r = exec::fetch_optional(
        pool,
        "SELECT execution_mode, host_id, sandbox_instance_id, claim_strategy
         FROM runners WHERE id = ?",
        &[id.into()],
    )
    .await?
    .ok_or(sqlx::Error::RowNotFound)?;
    Ok((r.text(0), r.text(1), r.text(2), r.text(3)))
}

/// One claim candidate: the job facts the poller needs to decide, plus the
/// pipeline labels and the age used by the aging rule.
pub struct ClaimCandidateRow {
    pub job_id: String,
    pub pipeline_id: String,
    pub trigger_params: Option<String>,
    /// Job snapshot as stored (JSON).
    pub snapshot: String,
    pub runner_labels: String,
    pub debug_unredact: i64,
    pub verbose_logs: i64,
    pub trigger_by: String,
    pub pipeline_name: String,
    pub team_id: Option<String>,
    pub trigger_type: String,
    pub environment_id: Option<String>,
    pub run_number: i64,
    pub run_attempt: i64,
    pub waited_seconds: i64,
}

/// The registration queue: pending jobs this runner may claim, queue-fair
/// order, with the pipeline labels and the age used by the aging rule.
///
/// A job is excluded while the environment it targets has a sibling running
/// (chain rule), and `cancel_in_progress` environments offer only their oldest
/// pending job — the sibling cancellation happens inside the claim
/// transaction.
pub async fn claim_candidates(
    pool: &DbPool,
    runner_team_id: Option<&str>,
) -> Result<Vec<ClaimCandidateRow>, AppError> {
    match pool {
        DbPool::Sqlite(pool) => claim_candidates_sqlite(pool, runner_team_id).await,
        #[cfg(feature = "postgres")]
        DbPool::Pg(pool) => claim_candidates_pg(pool, runner_team_id).await,
    }
}

async fn claim_candidates_sqlite(
    pool: &sqlx::SqlitePool,
    runner_team_id: Option<&str>,
) -> Result<Vec<ClaimCandidateRow>, AppError> {
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
    .await?
    .into_iter()
    .map(|r| ClaimCandidateRow {
        job_id: r.get(0),
        pipeline_id: r.get(1),
        trigger_params: r.get(2),
        snapshot: r.get(3),
        runner_labels: r.get(4),
        debug_unredact: r.get(5),
        verbose_logs: r.get(6),
        trigger_by: r.get(7),
        pipeline_name: r.get(8),
        team_id: r.get(9),
        trigger_type: r.get(10),
        environment_id: r.get(11),
        run_number: r.get(12),
        run_attempt: r.get(13),
        waited_seconds: r.get(14),
    })
    .collect())
}

/// Postgres form of [`claim_candidates_sqlite`]: the snapshot team key becomes
/// `jsonb ->> 'team_id'` and the waited seconds an epoch subtraction (the
/// timestamps stay TEXT, so both sides are parsed explicitly).
#[cfg(feature = "postgres")]
async fn claim_candidates_pg(
    pool: &sqlx::PgPool,
    runner_team_id: Option<&str>,
) -> Result<Vec<ClaimCandidateRow>, AppError> {
    Ok(sqlx::query(
        "SELECT j.id, j.pipeline_id, j.trigger_params, COALESCE(j.snapshot, '{}'), p.runner_labels,
                j.debug_unredact, j.verbose_logs, j.trigger_by, p.name,
                COALESCE(j.snapshot::jsonb ->> 'team_id', p.team_id), p.trigger_type,
                j.environment_id, j.run_number, j.run_attempt,
                (EXTRACT(EPOCH FROM now()) - EXTRACT(EPOCH FROM to_timestamp(j.created_at, 'YYYY-MM-DD HH24:MI:SS')))::bigint AS waited_seconds
         FROM jobs j
         JOIN pipelines p ON p.id = j.pipeline_id
         WHERE j.status = 'pending'
           AND ($1::text IS NULL OR (j.snapshot::jsonb ->> 'team_id') = $1::text)
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
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|r| ClaimCandidateRow {
        job_id: r.get(0),
        pipeline_id: r.get(1),
        trigger_params: r.get(2),
        snapshot: r.get(3),
        runner_labels: r.get(4),
        debug_unredact: r.get(5),
        verbose_logs: r.get(6),
        trigger_by: r.get(7),
        pipeline_name: r.get(8),
        team_id: r.get(9),
        trigger_type: r.get(10),
        environment_id: r.get(11),
        run_number: r.get(12),
        run_attempt: r.get(13),
        waited_seconds: r.get(14),
    })
    .collect())
}

/// True when the team row exists — the runner team binding check.
pub async fn team_exists(pool: &DbPool, team_id: &str) -> Result<bool, AppError> {
    Ok(exec::fetch_optional(
        pool,
        "SELECT EXISTS(SELECT 1 FROM teams WHERE id = ?)",
        &[team_id.into()],
    )
    .await?
    .map(|r| r.i64(0) != 0)
    .unwrap_or(false))
}

/// `team_id` of one runner (`None` = shared/unassigned or absent row).
pub async fn team_of(pool: &DbPool, runner_id: &str) -> Result<Option<String>, AppError> {
    Ok(exec::fetch_optional(
        pool,
        "SELECT team_id FROM runners WHERE id = ?",
        &[runner_id.into()],
    )
    .await?
    .ok_or(sqlx::Error::RowNotFound)?
    .opt_text(0))
}

/// How many runner rows carry this exact name (standalone idempotence check).
pub async fn count_by_name(pool: &DbPool, name: &str) -> Result<i64, AppError> {
    Ok(exec::fetch_scalar_i64(
        pool,
        "SELECT COUNT(1) FROM runners WHERE name = ?",
        &[name.into()],
    )
    .await?)
}

/// Mark a pending job failed because its resource-profile grant was revoked
/// while it was queued (the claim path's terminal write).
pub async fn fail_pending_job(pool: &DbPool, job_id: &str) -> Result<(), AppError> {
    exec::execute(
        pool,
        "UPDATE jobs SET status = 'failed', end_time = datetime('now'),
         updated_at = datetime('now') WHERE id = ? AND status = 'pending'",
        &[job_id.into()],
    )
    .await?;
    Ok(())
}

/// The policy columns of one runner, raw (`None` = no such row). Column order:
/// max_concurrent_jobs, clean_workspace_after_job, shell_enabled,
/// ssh_enabled, docker_enabled, wasm_enabled, ssh_host_key_policy,
/// execution_mode, claim_strategy, netns_isolation.
pub async fn policy_row(
    pool: &DbPool,
    runner_id: &str,
) -> Result<Option<(i64, i64, i64, i64, i64, i64, String, String, String, i64)>, AppError> {
    Ok(exec::fetch_optional(
        pool,
        "SELECT max_concurrent_jobs, clean_workspace_after_job,
                shell_enabled, ssh_enabled, docker_enabled, wasm_enabled, ssh_host_key_policy,
                execution_mode, claim_strategy, netns_isolation
         FROM runners WHERE id = ?",
        &[runner_id.into()],
    )
    .await?
    .map(|r| {
        (
            r.i64(0),
            r.i64(1),
            r.i64(2),
            r.i64(3),
            r.i64(4),
            r.i64(5),
            r.text(6),
            r.text(7),
            r.text(8),
            r.i64(9),
        )
    }))
}

/// The claim's write: flip the job to running and stamp the dispatch facts.
/// Guarded by `status = 'pending'`, so a job another poller just took comes back
/// `None` instead of being claimed twice. Returns the frozen run identity:
/// `(id, pipeline_id, trigger_params, environment_id, run_number, run_attempt)`.
pub async fn claim_job_tx(
    tx: &mut DbTx<'_>,
    runner_id: &str,
    job_id: &str,
    slots: i64,
    resource_spec: &str,
    sandbox_key: &str,
) -> Result<Option<(String, String, Option<String>, Option<String>, i64, i64)>, AppError> {
    Ok(tx
        .fetch_optional(
            "UPDATE jobs SET status = 'running', runner_id = ?, start_time = datetime('now'),
         resource_slots = ?, resource_spec = ?, sandbox_instance_id = ?, dispatch_reason = '',
         updated_at = datetime('now')
         WHERE id = ? AND status = 'pending'
         RETURNING id, pipeline_id, trigger_params, environment_id, run_number, run_attempt",
            &[
                runner_id.into(),
                SqlValue::Int(slots),
                resource_spec.into(),
                sandbox_key.into(),
                job_id.into(),
            ],
        )
        .await?
        .map(|r| {
            (
                r.text(0),
                r.text(1),
                r.opt_text(2),
                r.opt_text(3),
                r.i64(4),
                r.i64(5),
            )
        }))
}

/// The claim decision's re-read, inside the caller's `BEGIN IMMEDIATE` scope:
/// `(pipeline_id, environment_id, runner_team_id, job_team_id)`. `None` when
/// the job is gone or no longer pending (the caller rolls back and moves on).
pub async fn claim_candidate_tx(
    tx: &mut DbTx<'_>,
    runner_id: &str,
    job_id: &str,
) -> Result<Option<(String, Option<String>, Option<String>, Option<String>)>, AppError> {
    let sql = match tx {
        DbTx::Sqlite(_) => {
            "SELECT j.pipeline_id, j.environment_id, r.team_id, \
                json_extract(j.snapshot, '$.team_id') \
         FROM jobs j JOIN runners r ON r.id = ? \
         WHERE j.id = ? AND j.status = 'pending'"
        }
        #[cfg(feature = "postgres")]
        DbTx::Pg(_) => {
            "SELECT j.pipeline_id, j.environment_id, r.team_id, \
                (j.snapshot::jsonb ->> 'team_id') \
         FROM jobs j JOIN runners r ON r.id = $1 \
         WHERE j.id = $2 AND j.status = 'pending'"
        }
    };
    let row = tx
        .fetch_optional(sql, &[runner_id.into(), job_id.into()])
        .await?;
    Ok(row.map(|r| (r.text(0), r.opt_text(1), r.opt_text(2), r.opt_text(3))))
}

// ───────────────────────────── claim payload plane ─────────────────────────────
//
// Payload assembly reads per pipeline / environment. `build_claim_payload` keeps
// the crypto and the JSON shaping; every query lives here.

/// One claim-payload task row. Both projections below select the same column
/// order, so the payload builder reads one shape either way.
pub struct ClaimTaskRow {
    pub id: String,
    pub step_order: i64,
    pub name: String,
    pub exec_type: String,
    pub host_id: String,
    pub host_names: String,
    pub host_labels: String,
    pub command_template: String,
    pub work_dir: String,
    pub retry_count: i64,
    pub timeout_seconds: i64,
    pub fail_strategy: String,
    pub step_secrets: String,
    pub step_env: String,
    pub image: String,
    pub volumes: String,
    pub hosts_strategy: String,
    pub privileged: i64,
    pub capabilities: String,
    pub hosts_max_parallel: Option<i64>,
    pub needs_json: String,
}

fn claim_task_row(r: sqlx::sqlite::SqliteRow) -> ClaimTaskRow {
    ClaimTaskRow {
        id: r.get(0),
        step_order: r.get(1),
        name: r.get(2),
        exec_type: r.get(3),
        host_id: r.get(4),
        host_names: r.get(5),
        host_labels: r.get(6),
        command_template: r.get(7),
        work_dir: r.get(8),
        retry_count: r.get(9),
        timeout_seconds: r.get(10),
        fail_strategy: r.get(11),
        step_secrets: r.get(12),
        step_env: r.get(13),
        image: r.get(14),
        volumes: r.get(15),
        hosts_strategy: r.get(16),
        privileged: r.get(17),
        capabilities: r.get(18),
        hosts_max_parallel: r.get(19),
        needs_json: r.get(20),
    }
}

/// Snapshot task rows (frozen definition), step-ordered — the claim payload
/// shape. `snapshot_raw` is the job snapshot as stored.
pub async fn frozen_task_rows(
    pool: &DbPool,
    snapshot_raw: &str,
) -> Result<Vec<ClaimTaskRow>, AppError> {
    match pool {
        DbPool::Sqlite(pool) => frozen_task_rows_sqlite(pool, snapshot_raw).await,
        #[cfg(feature = "postgres")]
        DbPool::Pg(pool) => frozen_task_rows_pg(pool, snapshot_raw).await,
    }
}

async fn frozen_task_rows_sqlite(
    pool: &sqlx::SqlitePool,
    snapshot_raw: &str,
) -> Result<Vec<ClaimTaskRow>, AppError> {
    Ok(sqlx::query(
            "SELECT COALESCE(json_extract(j.value, '$.id'), ''), json_extract(j.value, '$.step_order'),
                    json_extract(j.value, '$.name'), json_extract(j.value, '$.exec_type'),
                    COALESCE(json_extract(j.value, '$.host_id'), ''), COALESCE(json_extract(j.value, '$.host_names'), ''),
                    COALESCE(json_extract(j.value, '$.host_labels'), ''),
                    COALESCE(json_extract(j.value, '$.command_template'), ''),
                    COALESCE(json_extract(j.value, '$.work_dir'), ''),
                    COALESCE(json_extract(j.value, '$.retry_count'), 0),
                    COALESCE(json_extract(j.value, '$.timeout_seconds'), 0),
                    COALESCE(json_extract(j.value, '$.fail_strategy'), 'stop'),
                    COALESCE(json_extract(j.value, '$.step_secrets'), ''),
                    COALESCE(json_extract(j.value, '$.step_env'), '{}'),
                    COALESCE(json_extract(j.value, '$.image'), ''),
                    COALESCE(json_extract(j.value, '$.volumes'), '[]'),
                    COALESCE(json_extract(j.value, '$.hosts_strategy'), 'chain'),
                    COALESCE(json_extract(j.value, '$.privileged'), 0),
                    COALESCE(json_extract(j.value, '$.capabilities'), '[]'),
                    COALESCE(json_extract(j.value, '$.hosts_max_parallel'), NULL),
                    COALESCE(json_extract(j.value, '$.needs'), '[]')
             FROM json_each(?, '$.tasks') j
             ORDER BY json_extract(j.value, '$.step_order')",
    )
    .bind(snapshot_raw)
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(claim_task_row)
    .collect())
}

/// Postgres form of [`frozen_task_rows_sqlite`]: `json_each` becomes
/// `jsonb_array_elements`, and every numeric extraction is cast back to BIGINT
/// so the typed row decodes identically to the SQLite path.
#[cfg(feature = "postgres")]
async fn frozen_task_rows_pg(
    pool: &sqlx::PgPool,
    snapshot_raw: &str,
) -> Result<Vec<ClaimTaskRow>, AppError> {
    Ok(sqlx::query(
        "SELECT COALESCE(elem ->> 'id', ''),
                COALESCE((elem ->> 'step_order')::bigint, 0),
                elem ->> 'name',
                elem ->> 'exec_type',
                COALESCE(elem ->> 'host_id', ''),
                COALESCE(elem ->> 'host_names', ''),
                COALESCE(elem ->> 'host_labels', ''),
                COALESCE(elem ->> 'command_template', ''),
                COALESCE(elem ->> 'work_dir', ''),
                COALESCE((elem ->> 'retry_count')::bigint, 0),
                COALESCE((elem ->> 'timeout_seconds')::bigint, 0),
                COALESCE(elem ->> 'fail_strategy', 'stop'),
                COALESCE(elem ->> 'step_secrets', ''),
                COALESCE(elem ->> 'step_env', '{}'),
                COALESCE(elem ->> 'image', ''),
                COALESCE(elem ->> 'volumes', '[]'),
                COALESCE(elem ->> 'hosts_strategy', 'chain'),
                -- `privileged` is a JSON boolean in snapshots (SQLite's
                -- json_extract yields 0/1); normalise either shape to BIGINT.
                COALESCE(
                    CASE jsonb_typeof(elem -> 'privileged')
                        WHEN 'boolean' THEN (CASE WHEN (elem -> 'privileged')::boolean THEN 1 ELSE 0 END)::bigint
                        WHEN 'number' THEN (elem ->> 'privileged')::bigint
                        ELSE 0
                    END,
                    0),
                COALESCE(elem ->> 'capabilities', '[]'),
                COALESCE((elem ->> 'hosts_max_parallel')::bigint, NULL),
                COALESCE(elem ->> 'needs', '[]')
         FROM jsonb_array_elements(($1::jsonb) -> 'tasks') AS elem
         ORDER BY COALESCE((elem ->> 'step_order')::bigint, 0)",
    )
    .bind(snapshot_raw)
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|r| ClaimTaskRow {
        id: r.get(0),
        step_order: r.get(1),
        name: r.get(2),
        exec_type: r.get(3),
        host_id: r.get(4),
        host_names: r.get(5),
        host_labels: r.get(6),
        command_template: r.get(7),
        work_dir: r.get(8),
        retry_count: r.get(9),
        timeout_seconds: r.get(10),
        fail_strategy: r.get(11),
        step_secrets: r.get(12),
        step_env: r.get(13),
        image: r.get(14),
        volumes: r.get(15),
        hosts_strategy: r.get(16),
        privileged: r.get(17),
        capabilities: r.get(18),
        hosts_max_parallel: r.get(19),
        needs_json: r.get(20),
    })
    .collect())
}

/// Live task rows (un-frozen pipelines), step-ordered — the claim payload shape.
pub async fn live_task_rows(
    pool: &DbPool,
    pipeline_id: &str,
) -> Result<Vec<ClaimTaskRow>, AppError> {
    Ok(exec::fetch_all(
        pool,
        "SELECT id, step_order, name, exec_type, COALESCE(host_id, ''), host_names, host_labels, command_template,
                    work_dir, retry_count, timeout_seconds, fail_strategy, step_secrets, step_env, image, volumes,
                    hosts_strategy,
                    privileged, capabilities, hosts_max_parallel, needs_json
             FROM tasks WHERE pipeline_id = ? ORDER BY step_order",
        &[pipeline_id.into()],
    )
    .await?
    .into_iter()
    .map(|r| ClaimTaskRow {
        id: r.text(0),
        step_order: r.i64(1),
        name: r.text(2),
        exec_type: r.text(3),
        host_id: r.text(4),
        host_names: r.text(5),
        host_labels: r.text(6),
        command_template: r.text(7),
        work_dir: r.text(8),
        retry_count: r.i64(9),
        timeout_seconds: r.i64(10),
        fail_strategy: r.text(11),
        step_secrets: r.text(12),
        step_env: r.text(13),
        image: r.text(14),
        volumes: r.text(15),
        hosts_strategy: r.text(16),
        privileged: r.i64(17),
        capabilities: r.text(18),
        hosts_max_parallel: r.opt_i64(19),
        needs_json: r.text(20),
    })
    .collect())
}

/// `(name, value)` of a pipeline's explicit secret bindings, same-team only —
/// stale/cross-team rows are ignored by the join predicate.
pub async fn bound_secret_values(
    pool: &DbPool,
    pipeline_id: &str,
) -> Result<Vec<(String, String)>, AppError> {
    Ok(exec::fetch_all(
        pool,
        "SELECT s.name, s.value
         FROM pipeline_secrets ps
         JOIN secrets s ON s.id = ps.secret_id
         JOIN pipelines p ON p.id = ps.pipeline_id AND s.team_id = p.team_id AND p.team_id != ''
         WHERE ps.pipeline_id = ?",
        &[pipeline_id.into()],
    )
    .await?
    .into_iter()
    .map(|r| (r.text(0), r.text(1)))
    .collect())
}

/// `(name, value)` of secrets a pipeline inherits from its team (opt-in
/// `team_shared = 1`), excluding the unassigned pseudo-team.
pub async fn team_shared_secret_values(
    pool: &DbPool,
    pipeline_id: &str,
) -> Result<Vec<(String, String)>, AppError> {
    Ok(exec::fetch_all(
        pool,
        "SELECT s.name, s.value
         FROM secrets s
         JOIN pipelines p ON p.team_id = s.team_id AND p.team_id != ''
         WHERE p.id = ? AND s.team_shared = 1",
        &[pipeline_id.into()],
    )
    .await?
    .into_iter()
    .map(|r| (r.text(0), r.text(1)))
    .collect())
}

/// `(name, value)` of a pipeline's explicitly bound platform variables.
pub async fn bound_variable_values(
    pool: &DbPool,
    pipeline_id: &str,
) -> Result<Vec<(String, String)>, AppError> {
    Ok(exec::fetch_all(
        pool,
        "SELECT pv.name, pv.value
         FROM pipeline_vars pvars
         JOIN platform_vars pv ON pv.id = pvars.var_id
         WHERE pvars.pipeline_id = ?",
        &[pipeline_id.into()],
    )
    .await?
    .into_iter()
    .map(|r| (r.text(0), r.text(1)))
    .collect())
}

/// `(name, value)` of an environment's variable overrides.
pub async fn environment_variable_overrides(
    pool: &DbPool,
    environment_id: &str,
) -> Result<Vec<(String, String)>, AppError> {
    Ok(exec::fetch_all(
        pool,
        "SELECT pv.name, o.value
         FROM environment_var_overrides o
         JOIN platform_vars pv ON pv.id = o.var_id
         WHERE o.environment_id = ?",
        &[environment_id.into()],
    )
    .await?
    .into_iter()
    .map(|r| (r.text(0), r.text(1)))
    .collect())
}

/// `(step_order, input_defs_json)` of a pipeline's marketplace resolutions.
pub async fn resolution_inputs(
    pool: &DbPool,
    pipeline_id: &str,
) -> Result<Vec<(i64, String)>, AppError> {
    Ok(exec::fetch_all(
        pool,
        "SELECT step_order, input_defs_json FROM step_resolutions WHERE pipeline_id = ?",
        &[pipeline_id.into()],
    )
    .await?
    .into_iter()
    .map(|r| (r.i64(0), r.text(1)))
    .collect())
}

/// Clear the cancel notices a runner explicitly acked after applying them.
pub async fn acknowledge_cancelled(
    pool: &DbPool,
    runner_id: &str,
    ids: &[String],
) -> Result<(), AppError> {
    for id in ids {
        exec::execute(
            pool,
            "UPDATE jobs SET cancel_ack_pending = 0 \
             WHERE id = ? AND runner_id = ? AND status = 'cancelled' AND cancel_ack_pending = 1",
            &[id.as_str().into(), runner_id.into()],
        )
        .await?;
    }
    Ok(())
}

/// Undelivered cancellation IDs. They stay pending until a later poll acks
/// them, so a lost response is safely retried (oldest first, bounded).
pub async fn pending_cancelled_for_delivery(
    pool: &DbPool,
    runner_id: &str,
) -> Result<Vec<String>, AppError> {
    Ok(exec::fetch_all(
        pool,
        "SELECT id FROM jobs \
         WHERE runner_id = ? AND status = 'cancelled' AND cancel_ack_pending = 1 \
         ORDER BY updated_at, id LIMIT 32",
        &[runner_id.into()],
    )
    .await?
    .into_iter()
    .map(|r| r.text(0))
    .collect())
}
// ───────────────────────── shared admin row shapes ─────────────────────────
//
// The two admin read screens select different column sets; both map into a
// typed struct here so a renamed/reordered column fails to compile instead of
// silently shifting a `get(n)` index.

pub struct RunnerListRow {
    pub id: String,
    pub name: String,
    pub labels: String,
    pub status: String,
    pub version: Option<String>,
    pub system_info: String,
    pub last_seen: Option<String>,
    pub is_frozen: i64,
    pub max_concurrent_jobs: i64,
    pub clean_workspace_after_job: i64,
    pub shell_enabled: i64,
    pub ssh_enabled: i64,
    pub docker_enabled: i64,
    pub wasm_enabled: i64,
    pub ssh_host_key_policy: String,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    pub team_id: Option<String>,
    pub running_jobs: i64,
    pub execution_mode: String,
    pub host_id: String,
    pub runner_instance_id: String,
    pub claim_strategy: String,
    pub netns_isolation: i64,
    pub capability_reprobe_requested: i64,
    pub capability_reprobe_requested_at: Option<String>,
    pub capability_reprobe_served_at: Option<String>,
}

pub struct RunnerDetailRow {
    pub id: String,
    pub name: String,
    pub labels: String,
    pub status: String,
    pub version: Option<String>,
    pub system_info: String,
    pub last_seen: Option<String>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    pub team_id: Option<String>,
    pub host_id: String,
    pub runner_instance_id: String,
    pub capability_reprobe_requested: i64,
    pub capability_reprobe_requested_at: Option<String>,
    pub capability_reprobe_served_at: Option<String>,
}

fn runner_list_row(r: exec::GenericRow) -> RunnerListRow {
    RunnerListRow {
        id: r.text(0),
        name: r.text(1),
        labels: r.text(2),
        status: r.text(3),
        version: r.opt_text(4),
        system_info: r.text(5),
        last_seen: r.opt_text(6),
        is_frozen: r.i64(7),
        max_concurrent_jobs: r.i64(8),
        clean_workspace_after_job: r.i64(9),
        shell_enabled: r.i64(10),
        ssh_enabled: r.i64(11),
        docker_enabled: r.i64(12),
        wasm_enabled: r.i64(13),
        ssh_host_key_policy: r.text(14),
        created_at: r.opt_text(15),
        updated_at: r.opt_text(16),
        team_id: r.opt_text(17),
        running_jobs: r.i64(18),
        execution_mode: r.text(19),
        host_id: r.text(20),
        runner_instance_id: r.text(21),
        claim_strategy: r.text(22),
        netns_isolation: r.i64(23),
        capability_reprobe_requested: r.i64(24),
        capability_reprobe_requested_at: r.opt_text(25),
        capability_reprobe_served_at: r.opt_text(26),
    }
}

/// Every runner for the admin list, newest first, with its running-job count.
pub async fn list_for_admin(pool: &DbPool) -> Result<Vec<RunnerListRow>, AppError> {
    let rows = exec::fetch_all(
        pool,
        "SELECT r.id, r.name, r.labels, r.status, r.version, r.system_info, r.last_seen, r.is_frozen,
                r.max_concurrent_jobs, r.clean_workspace_after_job,
                r.shell_enabled, r.ssh_enabled, r.docker_enabled, r.wasm_enabled, r.ssh_host_key_policy,
                r.created_at, r.updated_at, r.team_id,
                (SELECT COUNT(*) FROM jobs j WHERE j.runner_id = r.id AND j.status = 'running') AS running_jobs,
                r.execution_mode, r.host_id, r.runner_instance_id, r.claim_strategy,
                r.netns_isolation,
                r.capability_reprobe_requested, r.capability_reprobe_requested_at,
                r.capability_reprobe_served_at
         FROM runners r ORDER BY r.created_at DESC",
        &[],
    )
    .await?;
    Ok(rows.into_iter().map(runner_list_row).collect())
}

fn runner_detail_row(r: exec::GenericRow) -> RunnerDetailRow {
    RunnerDetailRow {
        id: r.text(0),
        name: r.text(1),
        labels: r.text(2),
        status: r.text(3),
        version: r.opt_text(4),
        system_info: r.text(5),
        last_seen: r.opt_text(6),
        created_at: r.opt_text(7),
        updated_at: r.opt_text(8),
        team_id: r.opt_text(9),
        host_id: r.text(10),
        runner_instance_id: r.text(11),
        capability_reprobe_requested: r.i64(12),
        capability_reprobe_requested_at: r.opt_text(13),
        capability_reprobe_served_at: r.opt_text(14),
    }
}

/// One runner for the admin detail screen (`None` = not found).
pub async fn detail_of(pool: &DbPool, id: &str) -> Result<Option<RunnerDetailRow>, AppError> {
    Ok(exec::fetch_optional(
        pool,
        "SELECT id, name, labels, status, version, system_info, last_seen,
                created_at, updated_at, team_id, host_id, runner_instance_id,
                capability_reprobe_requested, capability_reprobe_requested_at,
                capability_reprobe_served_at
         FROM runners WHERE id = ?",
        &[id.into()],
    )
    .await?
    .map(runner_detail_row))
}

/// Jobs currently running on this runner — display-only count.
pub async fn running_jobs_of(pool: &DbPool, id: &str) -> Result<i64, AppError> {
    Ok(exec::fetch_scalar_i64(
        pool,
        "SELECT COUNT(*) FROM jobs WHERE runner_id = ? AND status = 'running'",
        &[id.into()],
    )
    .await
    .unwrap_or(0))
}

/// `(name, stored value)` for the named team secrets a claim may expose, in
/// one round trip. The `IN (...)` list is built from `?` placeholders — only
/// the *count* varies, never SQL text.
pub async fn team_secret_values_by_name(
    pool: &DbPool,
    pipeline_id: &str,
    names: &[&str],
) -> Result<Vec<(String, String)>, AppError> {
    if names.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = vec!["?"; names.len()].join(",");
    let query = format!(
        "SELECT s.name, s.value FROM secrets s
         JOIN pipelines p ON p.team_id = s.team_id AND p.team_id != ''
         WHERE p.id = ? AND s.name IN ({placeholders})"
    );
    let mut binds: Vec<SqlValue> = vec![pipeline_id.into()];
    binds.extend(names.iter().map(|name| SqlValue::from(*name)));
    Ok(exec::fetch_all(pool, &query, &binds)
        .await?
        .into_iter()
        .map(|r| (r.text(0), r.text(1)))
        .collect())
}

/// Validates switching `runner_id` to `mode` against the runner's live state:
/// sandbox capability freshness for non-Host modes and no running jobs. Must
/// be called inside the caller's `BEGIN IMMEDIATE` transaction that also
/// performs the update; on error the caller must roll the transaction back.
pub async fn validate_execution_mode_transition(
    tx: &mut DbTx<'_>,
    runner_id: &str,
    mode: RunnerExecutionMode,
) -> Result<(), AppError> {
    let (status, last_seen, system_info_raw) =
        crate::store::runners::transition_facts_tx(tx, runner_id).await?;
    if mode != RunnerExecutionMode::Host {
        if status != "online" {
            return Err(AppError::Conflict(
                "runner must be online and report fresh sandbox capabilities before selecting this execution mode".into(),
            ));
        }
        let fresh = crate::store::runners::seen_within_90s_tx(tx, last_seen.as_deref()).await?;
        let capabilities = serde_json::from_str::<SystemInfo>(&system_info_raw)
            .ok()
            .and_then(|info| info.execution_capabilities);
        let Some(capabilities) = capabilities else {
            return Err(AppError::Conflict(
                "runner has not reported sandbox capabilities yet; wait for its next heartbeat"
                    .into(),
            ));
        };
        // One per-tier lookup for every sandboxed tier — Host never reaches here.
        let Some((backend, reason)) = capabilities.backend(mode) else {
            return Err(AppError::Conflict(format!(
                "runner does not support {} execution: host has no sandbox backend",
                mode.as_str()
            )));
        };
        let checked_fresh = crate::store::runners::capability_check_fresh_tx(
            tx,
            capabilities.checked_at.as_deref(),
        )
        .await?;
        if !fresh || !checked_fresh || backend != ExecutionBackendStatus::Supported {
            let detail = reason.unwrap_or("capability check is stale or unsupported");
            return Err(AppError::Conflict(format!(
                "runner does not support {} execution: {detail}",
                mode.as_str()
            )));
        }
    }
    let running = crate::store::runners::running_job_count_tx(tx, runner_id).await?;
    if running > 0 {
        return Err(AppError::Conflict(format!(
            "execution mode cannot change while {running} job(s) are running"
        )));
    }
    Ok(())
}

/// Switch one runner's execution mode under one `BEGIN IMMEDIATE`: the
/// transition rule and the write share the lock, so a concurrent claim or
/// heartbeat cannot interleave between them.
pub async fn change_execution_mode(
    pool: &DbPool,
    runner_id: &str,
    mode: RunnerExecutionMode,
) -> Result<(), AppError> {
    let mut tx = pool
        .begin_immediate()
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;

    let current = tx
        .fetch_optional(
            "SELECT execution_mode FROM runners WHERE id = ?",
            &[runner_id.into()],
        )
        .await?
        .ok_or(sqlx::Error::RowNotFound)?
        .text(0);

    if current != mode.as_str() {
        if let Err(error) = validate_execution_mode_transition(&mut tx, runner_id, mode).await {
            tx.rollback()
                .await
                .map_err(|e| AppError::Internal(e.to_string()))?;
            return Err(error);
        }
    }

    tx.execute(
        "UPDATE runners SET execution_mode = ?, updated_at = datetime('now') WHERE id = ?",
        &[mode.as_str().into(), runner_id.into()],
    )
    .await?;

    tx.commit()
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;
    Ok(())
}
