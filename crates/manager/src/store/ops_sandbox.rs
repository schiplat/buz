//! Ops-plane sandbox storage: host enrollment state, the immutable config
//! version history, the pending-change (approval queue) rows, and
//! team resource-profile grants.
//!
//! Versioning invariant: every accepted config change stores a history row
//! at `max(desired, applied) + 1`, so a restored database can never collide
//! with a version a host already applied.

use sqlx::{Row, SqlitePool};

use shared::error::AppError;

/// One `sandbox_hosts` row plus the server-derived liveness flag.
#[derive(Debug, Clone)]
pub struct HostState {
    pub host_id: String,
    pub instance_id: String,
    pub status: String,
    pub max_resource_slots: i64,
    pub desired_version: i64,
    pub applied_version: i64,
    pub applied_sha256: String,
    pub applied_at: Option<String>,
    pub last_fetch_at: Option<String>,
    pub last_fetch_error: Option<String>,
    pub consecutive_fetch_failures: i64,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    /// sandboxd fetched within the last 5 minutes (computed against DB time).
    pub live: bool,
}

fn host_state(r: sqlx::sqlite::SqliteRow) -> HostState {
    HostState {
        host_id: r.get(0),
        instance_id: r.get(1),
        status: r.get(2),
        max_resource_slots: r.get(3),
        desired_version: r.get(4),
        applied_version: r.get(5),
        applied_sha256: r.get(6),
        applied_at: r.get(7),
        last_fetch_at: r.get(8),
        last_fetch_error: r.get(9),
        consecutive_fetch_failures: r.get(10),
        created_at: r.get(11),
        updated_at: r.get(12),
        live: r.get::<i64, _>(13) != 0,
    }
}

/// One enrolled host's full state (`None` = not enrolled).
pub async fn host_state_of(
    pool: &SqlitePool,
    host_id: &str,
    instance_id: &str,
) -> Result<Option<HostState>, AppError> {
    let row = sqlx::query(
        "SELECT host_id, instance_id, status, max_resource_slots, desired_version,
                applied_version, applied_sha256, applied_at, last_fetch_at,
                last_fetch_error, consecutive_fetch_failures, created_at, updated_at,
                COALESCE(last_fetch_at >= datetime('now', '-300 seconds'), 0) AS live
         FROM sandbox_hosts WHERE host_id = ? AND instance_id = ?",
    )
    .bind(host_id)
    .bind(instance_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(host_state))
}

/// Every enrolled host, (host_id, instance_id)-ordered.
pub async fn list_host_states(pool: &SqlitePool) -> Result<Vec<HostState>, AppError> {
    let rows = sqlx::query(
        "SELECT host_id, instance_id, status, max_resource_slots, desired_version,
                applied_version, applied_sha256, applied_at, last_fetch_at,
                last_fetch_error, consecutive_fetch_failures, created_at, updated_at,
                COALESCE(last_fetch_at >= datetime('now', '-300 seconds'), 0) AS live
         FROM sandbox_hosts ORDER BY host_id, instance_id",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(host_state).collect())
}

/// `(host_id, instance_id, document)` of every host's latest config version.
pub async fn latest_documents_all(
    pool: &SqlitePool,
) -> Result<Vec<(String, String, String)>, AppError> {
    Ok(sqlx::query_as::<_, (String, String, String)>(
        "SELECT c.host_id, c.instance_id, c.document
             FROM sandbox_host_configs c
             JOIN (SELECT host_id, instance_id, MAX(version) AS version
                   FROM sandbox_host_configs GROUP BY host_id, instance_id) latest
               ON c.host_id = latest.host_id
              AND c.instance_id = latest.instance_id
              AND c.version = latest.version",
    )
    .fetch_all(pool)
    .await?)
}

/// `(host_id, sandbox_instance_id, name)` of every runner bound to a host.
pub async fn attached_runners_all(
    pool: &SqlitePool,
) -> Result<Vec<(String, String, String)>, AppError> {
    Ok(sqlx::query_as(
        "SELECT host_id, sandbox_instance_id, name FROM runners
         WHERE host_id != '' ORDER BY name",
    )
    .fetch_all(pool)
    .await?)
}

/// Names of the runners reporting one host.
pub async fn runner_names_for_host(
    pool: &SqlitePool,
    host_id: &str,
) -> Result<Vec<String>, AppError> {
    Ok(
        sqlx::query_scalar("SELECT name FROM runners WHERE host_id = ? ORDER BY name")
            .bind(host_id)
            .fetch_all(pool)
            .await?,
    )
}

/// Runners still bound to one (host, instance) pair — the delete guard.
pub async fn runner_count_for_pair(
    pool: &SqlitePool,
    host_id: &str,
    instance_id: &str,
) -> Result<i64, AppError> {
    Ok(sqlx::query_scalar(
        "SELECT COUNT(*) FROM runners WHERE host_id = ? AND sandbox_instance_id = ?",
    )
    .bind(host_id)
    .bind(instance_id)
    .fetch_one(pool)
    .await?)
}

/// Enroll a (host, instance) pair; re-enrolling rotates the token and applies
/// the slot budget only when the request carries a positive one.
pub async fn enroll_host(
    pool: &SqlitePool,
    host_id: &str,
    instance_id: &str,
    max_resource_slots: i64,
    token_hash: &str,
) -> Result<(), AppError> {
    sqlx::query(
        "INSERT INTO sandbox_hosts (host_id, instance_id, status, max_resource_slots, token_hash)
         VALUES (?, ?, 'enrolled', ?, ?)
         ON CONFLICT(host_id, instance_id) DO UPDATE SET
           token_hash = excluded.token_hash,
           max_resource_slots = CASE WHEN excluded.max_resource_slots > 0
                                     THEN excluded.max_resource_slots
                                     ELSE sandbox_hosts.max_resource_slots END,
           updated_at = datetime('now')",
    )
    .bind(host_id)
    .bind(instance_id)
    .bind(max_resource_slots)
    .bind(token_hash)
    .execute(pool)
    .await?;
    Ok(())
}

/// Delete one enrollment and cascade its pending changes and config
/// versions (one plain `BEGIN` transaction, as before).
pub async fn delete_host_cascade(
    pool: &SqlitePool,
    host_id: &str,
    instance_id: &str,
) -> Result<(), AppError> {
    let mut tx = pool.begin().await?;
    for table in [
        "sandbox_config_changes",
        "sandbox_host_configs",
        "sandbox_hosts",
    ] {
        sqlx::query(&format!(
            "DELETE FROM {table} WHERE host_id = ? AND instance_id = ?"
        ))
        .bind(host_id)
        .bind(instance_id)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// `(desired_version, applied_version)` of one host — the version allocator's
/// inputs. Errors when the host is not enrolled.
pub async fn host_versions(
    pool: &SqlitePool,
    host_id: &str,
    instance_id: &str,
) -> Result<(i64, i64), AppError> {
    Ok(sqlx::query_as(
        "SELECT desired_version, applied_version FROM sandbox_hosts
         WHERE host_id = ? AND instance_id = ?",
    )
    .bind(host_id)
    .bind(instance_id)
    .fetch_one(pool)
    .await?)
}

/// Store one immutable config version row.
pub async fn insert_config_version(
    pool: &SqlitePool,
    host_id: &str,
    instance_id: &str,
    version: i64,
    document_json: &str,
    document_sha256: &str,
    created_by: &str,
) -> Result<(), AppError> {
    sqlx::query(
        "INSERT INTO sandbox_host_configs (host_id, instance_id, version, document, document_sha256, created_by)
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(host_id)
    .bind(instance_id)
    .bind(version)
    .bind(document_json)
    .bind(document_sha256)
    .bind(created_by)
    .execute(pool)
    .await?;
    Ok(())
}

/// Point the host's desired version at `version`.
pub async fn bump_desired_version(
    pool: &SqlitePool,
    host_id: &str,
    instance_id: &str,
    version: i64,
) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE sandbox_hosts SET desired_version = ?, updated_at = datetime('now')
         WHERE host_id = ? AND instance_id = ?",
    )
    .bind(version)
    .bind(host_id)
    .bind(instance_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Version history of one host, newest first.
pub async fn config_versions(
    pool: &SqlitePool,
    host_id: &str,
    instance_id: &str,
) -> Result<Vec<(i64, String, String, String, String)>, AppError> {
    Ok(sqlx::query_as(
        "SELECT version, document_sha256, created_by, created_at, document
         FROM sandbox_host_configs
         WHERE host_id = ? AND instance_id = ?
         ORDER BY version DESC",
    )
    .bind(host_id)
    .bind(instance_id)
    .fetch_all(pool)
    .await?)
}

/// One stored config version's document (`None` = no such version).
pub async fn config_document_at(
    pool: &SqlitePool,
    host_id: &str,
    instance_id: &str,
    version: i64,
) -> Result<Option<String>, AppError> {
    let mut conn = pool
        .acquire()
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;
    config_document_at_conn(&mut conn, host_id, instance_id, version).await
}

/// Rotate a host's sandboxd token hash.
pub async fn update_token_hash(
    pool: &SqlitePool,
    host_id: &str,
    instance_id: &str,
    token_hash: &str,
) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE sandbox_hosts SET token_hash = ?, updated_at = datetime('now')
         WHERE host_id = ? AND instance_id = ?",
    )
    .bind(token_hash)
    .bind(host_id)
    .bind(instance_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Update a host's slot budget (0 = inherit the platform default).
pub async fn update_max_slots(
    pool: &SqlitePool,
    host_id: &str,
    instance_id: &str,
    max_resource_slots: i64,
) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE sandbox_hosts SET max_resource_slots = ?, updated_at = datetime('now')
         WHERE host_id = ? AND instance_id = ?",
    )
    .bind(max_resource_slots)
    .bind(host_id)
    .bind(instance_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// One pending-change INSERT: a platform-defaults catalog change.
pub async fn insert_defaults_pending_change(
    pool: &SqlitePool,
    id: &str,
    requested_by: &str,
    before_document: &str,
    after_document: &str,
) -> Result<(), AppError> {
    sqlx::query(
        "INSERT INTO sandbox_config_changes
             (id, host_id, instance_id, status, requested_by, before_document, after_document)
             VALUES (?, 'platform-defaults', 'defaults', 'pending', ?, ?, ?)",
    )
    .bind(id)
    .bind(requested_by)
    .bind(before_document)
    .bind(after_document)
    .execute(pool)
    .await?;
    Ok(())
}

/// One pending-change INSERT: a per-host config document change.
pub async fn insert_host_pending_change(
    pool: &SqlitePool,
    id: &str,
    host_id: &str,
    instance_id: &str,
    requested_by: &str,
    before_document: &str,
    after_document: &str,
) -> Result<(), AppError> {
    sqlx::query(
        "INSERT INTO sandbox_config_changes
             (id, host_id, instance_id, status, requested_by, before_document, after_document)
             VALUES (?, ?, ?, 'pending', ?, ?, ?)",
    )
    .bind(id)
    .bind(host_id)
    .bind(instance_id)
    .bind(requested_by)
    .bind(before_document)
    .bind(after_document)
    .execute(pool)
    .await?;
    Ok(())
}

/// One pending-change INSERT: a resource-profile grant request.
pub async fn insert_grant_pending_change(
    pool: &SqlitePool,
    id: &str,
    requested_by: &str,
    after_document: &str,
) -> Result<(), AppError> {
    sqlx::query(
        "INSERT INTO sandbox_config_changes
             (id, host_id, instance_id, status, requested_by, before_document, after_document)
             VALUES (?, 'resource-grants', 'grant', 'pending', ?, '{}', ?)",
    )
    .bind(id)
    .bind(requested_by)
    .bind(after_document)
    .execute(pool)
    .await?;
    Ok(())
}

/// One pending-change INSERT: a resource-profile revocation record.
pub async fn insert_revoke_pending_change(
    pool: &SqlitePool,
    id: &str,
    requested_by: &str,
    before_document: &str,
    after_document: &str,
) -> Result<(), AppError> {
    sqlx::query(
        "INSERT INTO sandbox_config_changes
             (id, host_id, instance_id, status, requested_by, before_document, after_document)
             VALUES (?, 'resource-grants', 'revoke', 'pending', ?, ?, ?)",
    )
    .bind(id)
    .bind(requested_by)
    .bind(before_document)
    .bind(after_document)
    .execute(pool)
    .await?;
    Ok(())
}

/// `(version, sha, created_by, created_at, document)` is the history tuple;
/// the approval queue row is `(id, host, instance, status, requested_by,
/// after_document, created_at, decided_at, approved_by, comment)`.
pub type ChangeRow = (
    String,
    String,
    String,
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

/// The approval queue: `(total, rows)` for one status view. `status =
/// "decided"` merges approved + rejected and pages; other statuses filter
/// exactly and stay single-page.
pub async fn list_changes(
    pool: &SqlitePool,
    status: &str,
    page: i64,
    page_size: i64,
) -> Result<(i64, Vec<ChangeRow>), AppError> {
    let (where_clause, status_filter): (String, Vec<String>) = if status == "decided" {
        (
            "WHERE status IN ('approved', 'rejected')".to_string(),
            vec![],
        )
    } else {
        ("WHERE status = ?".to_string(), vec![status.to_string()])
    };
    let total: i64 = sqlx::query_scalar(&format!(
        "SELECT COUNT(*) FROM sandbox_config_changes {where_clause}"
    ))
    .bind(status_filter.first())
    .fetch_one(pool)
    .await?;
    let sql = format!(
        "SELECT id, host_id, instance_id, status, requested_by, after_document, created_at, decided_at,
                approved_by, decision_comment
         FROM sandbox_config_changes {where_clause}
         ORDER BY CASE WHEN status = 'pending' THEN created_at ELSE decided_at END DESC
         LIMIT {} OFFSET {}",
        page_size,
        (page - 1) * page_size,
    );
    let rows: Vec<ChangeRow> = sqlx::query_as(&sql)
        .bind(status_filter.first())
        .fetch_all(pool)
        .await?;
    Ok((total, rows))
}

/// `(status, requested_by, host_id, instance_id, after_document)` of one
/// pending change (`None` = no such change).
pub type ChangeFacts = (String, String, String, String, String);

pub async fn change_facts(pool: &SqlitePool, id: &str) -> Result<Option<ChangeFacts>, AppError> {
    Ok(sqlx::query_as(
        "SELECT status, requested_by, host_id, instance_id, after_document
         FROM sandbox_config_changes WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?)
}

/// Pipelines bound to one (team, profile) — the grant blast radius.
pub async fn pipeline_count_for_profile(
    pool: &SqlitePool,
    team_id: &str,
    profile: &str,
) -> Result<i64, AppError> {
    Ok(sqlx::query_scalar(
        "SELECT COUNT(*) FROM pipelines WHERE team_id = ? AND resource_profile = ?",
    )
    .bind(team_id)
    .bind(profile)
    .fetch_one(pool)
    .await?)
}

/// Pending jobs of one (team, profile) — the revocation blast radius that
/// will fail terminally at claim.
pub async fn queued_jobs_for_profile(
    pool: &SqlitePool,
    team_id: &str,
    profile: &str,
) -> Result<i64, AppError> {
    Ok(sqlx::query_scalar(
        "SELECT COUNT(*) FROM jobs WHERE status = 'pending'
               AND COALESCE(json_extract(snapshot, '$.team_id'), '') = ?
               AND COALESCE(json_extract(snapshot, '$.resource_profile'), '') = ?",
    )
    .bind(team_id)
    .bind(profile)
    .fetch_one(pool)
    .await?)
}

/// Grant (or re-grant) one team profile.
pub async fn replace_grant(
    pool: &SqlitePool,
    team_id: &str,
    profile: &str,
    granted_by: &str,
) -> Result<(), AppError> {
    sqlx::query(
        "INSERT OR REPLACE INTO team_resource_grants (team_id, profile, granted_by)
                 VALUES (?, ?, ?)",
    )
    .bind(team_id)
    .bind(profile)
    .bind(granted_by)
    .execute(pool)
    .await?;
    Ok(())
}

/// Grant one team profile, ignoring an existing row.
pub async fn insert_grant_ignore(
    pool: &SqlitePool,
    team_id: &str,
    profile: &str,
    granted_by: &str,
) -> Result<(), AppError> {
    sqlx::query(
        "INSERT INTO team_resource_grants (team_id, profile, granted_by)
         VALUES (?, ?, ?)
         ON CONFLICT(team_id, profile) DO NOTHING",
    )
    .bind(team_id)
    .bind(profile)
    .bind(granted_by)
    .execute(pool)
    .await?;
    Ok(())
}

/// Revoke one team profile; `false` when it was not granted.
pub async fn delete_grant(
    pool: &SqlitePool,
    team_id: &str,
    profile: &str,
) -> Result<bool, AppError> {
    let result = sqlx::query("DELETE FROM team_resource_grants WHERE team_id = ? AND profile = ?")
        .bind(team_id)
        .bind(profile)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() != 0)
}

/// Mark a pending change approved.
pub async fn approve_change_row(
    pool: &SqlitePool,
    id: &str,
    approved_by: &str,
    decision_comment: Option<&str>,
) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE sandbox_config_changes SET status = 'approved', approved_by = ?,
                decision_comment = ?, decided_at = datetime('now') WHERE id = ?",
    )
    .bind(approved_by)
    .bind(decision_comment)
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Mark a pending change rejected.
pub async fn reject_change_row(
    pool: &SqlitePool,
    id: &str,
    rejected_by: &str,
    decision_comment: Option<&str>,
) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE sandbox_config_changes SET status = 'rejected', approved_by = ?,
            decision_comment = ?, decided_at = datetime('now') WHERE id = ?",
    )
    .bind(rejected_by)
    .bind(decision_comment)
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}

/// The full grant allowlist with team names, name/profile-ordered.
pub async fn list_grants(
    pool: &SqlitePool,
) -> Result<Vec<(String, String, String, Option<String>)>, AppError> {
    Ok(sqlx::query_as(
        "SELECT g.team_id, t.name, g.profile, g.created_at
         FROM team_resource_grants g JOIN teams t ON t.id = g.team_id
         ORDER BY t.name, g.profile",
    )
    .fetch_all(pool)
    .await?)
}

/// True when the team exists — the grant path's referential check.
pub async fn team_exists(pool: &SqlitePool, team_id: &str) -> Result<bool, AppError> {
    Ok(
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM teams WHERE id = ?)")
            .bind(team_id)
            .fetch_one(pool)
            .await?,
    )
}

// ────────────── claim-path reads (inside the claim transaction) ──────────────
//
// `slot_scheduler::decide` runs inside the claim's BEGIN IMMEDIATE; these are
// its storage reads, named for the fact each returns. They take the
// transaction's connection so every read shares the claim's lock.

/// `(max_resource_slots, desired_version, applied_version)` of one host —
/// the slot-budget facts (`None` = host not enrolled).
pub async fn host_slot_facts(
    conn: &mut sqlx::SqliteConnection,
    host_id: &str,
    instance_id: &str,
) -> Result<Option<(i64, i64, i64)>, AppError> {
    Ok(sqlx::query_as(
        "SELECT max_resource_slots, desired_version, applied_version
         FROM sandbox_hosts WHERE host_id = ? AND instance_id = ?",
    )
    .bind(host_id)
    .bind(instance_id)
    .fetch_optional(conn)
    .await?)
}

/// Stored config document at one version, read through the claim
/// transaction's connection (`None` = no such version). Connection-based core
/// of [`config_document_at`].
pub async fn config_document_at_conn(
    conn: &mut sqlx::SqliteConnection,
    host_id: &str,
    instance_id: &str,
    version: i64,
) -> Result<Option<String>, AppError> {
    let row: Option<String> = sqlx::query_scalar(
        "SELECT document FROM sandbox_host_configs
         WHERE host_id = ? AND instance_id = ? AND version = ?",
    )
    .bind(host_id)
    .bind(instance_id)
    .bind(version)
    .fetch_optional(conn)
    .await?;
    Ok(row)
}

/// `sandbox_default_max_slots` platform setting (0 = unlimited).
pub async fn default_max_slots_setting(conn: &mut sqlx::SqliteConnection) -> Result<i64, AppError> {
    Ok(sqlx::query_scalar(
        "SELECT COALESCE(CAST(value AS INTEGER), 0) FROM platform_settings WHERE key = 'sandbox_default_max_slots'",
    )
    .fetch_optional(conn)
    .await?
    .unwrap_or(0))
}

/// `sandbox_default_profiles` platform setting JSON (`None` = unset).
pub async fn default_profiles_setting(
    conn: &mut sqlx::SqliteConnection,
) -> Result<Option<String>, AppError> {
    Ok(sqlx::query_scalar(
        "SELECT value FROM platform_settings WHERE key = 'sandbox_default_profiles'",
    )
    .fetch_optional(conn)
    .await?)
}

/// True when the config row predates the staleness window close — a host
/// lagging its desired version for too long stops dispatching non-default
/// profiles.
pub async fn config_row_is_stale(
    conn: &mut sqlx::SqliteConnection,
    stale_seconds: i64,
    host_id: &str,
    instance_id: &str,
    version: i64,
) -> Result<bool, AppError> {
    Ok(sqlx::query_scalar(
        "SELECT COALESCE(datetime(created_at) <= datetime('now', ? || ' seconds'), 0)
         FROM sandbox_host_configs WHERE host_id = ? AND instance_id = ? AND version = ?",
    )
    .bind(format!("-{}", stale_seconds.max(0)))
    .bind(host_id)
    .bind(instance_id)
    .bind(version)
    .fetch_optional(conn)
    .await?
    .unwrap_or(false))
}

/// Slots currently in use by running jobs on one sandboxd instance — the
/// budget's occupancy half.
pub async fn running_slots_used(
    conn: &mut sqlx::SqliteConnection,
    sandbox_key: &str,
) -> Result<i64, AppError> {
    Ok(sqlx::query_scalar(
        "SELECT COALESCE(SUM(resource_slots), 0) FROM jobs
         WHERE status = 'running' AND sandbox_instance_id = ?",
    )
    .bind(sandbox_key)
    .fetch_one(conn)
    .await?)
}
