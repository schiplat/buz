//! Ops-plane sandbox storage: host enrollment state, the immutable config
//! version history, the pending-change (approval queue) rows, and
//! team resource-profile grants.
//!
//! Versioning invariant: every accepted config change stores a history row
//! at `max(desired, applied) + 1`, so a restored database can never collide
//! with a version a host already applied.

use crate::store::exec::{self, DbPool, DbTx, SqlValue};
use crate::store::sql::Sql;

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

fn host_state(r: exec::GenericRow) -> HostState {
    HostState {
        host_id: r.text(0),
        instance_id: r.text(1),
        status: r.text(2),
        max_resource_slots: r.i64(3),
        desired_version: r.i64(4),
        applied_version: r.i64(5),
        applied_sha256: r.text(6),
        applied_at: r.opt_text(7),
        last_fetch_at: r.opt_text(8),
        last_fetch_error: r.opt_text(9),
        consecutive_fetch_failures: r.i64(10),
        created_at: r.opt_text(11),
        updated_at: r.opt_text(12),
        live: r.i64(13) != 0,
    }
}

/// One enrolled host's full state (`None` = not enrolled).
pub async fn host_state_of(
    pool: &DbPool,
    host_id: &str,
    instance_id: &str,
) -> Result<Option<HostState>, AppError> {
    Ok(exec::fetch_optional(
        pool,
        "SELECT host_id, instance_id, status, max_resource_slots, desired_version,
                applied_version, applied_sha256, applied_at, last_fetch_at,
                last_fetch_error, consecutive_fetch_failures, created_at, updated_at,
                CASE WHEN last_fetch_at >= datetime('now', '-300 seconds') THEN 1 ELSE 0 END AS live
         FROM sandbox_hosts WHERE host_id = ? AND instance_id = ?",
        &[host_id.into(), instance_id.into()],
    )
    .await?
    .map(host_state))
}

/// Every enrolled host, (host_id, instance_id)-ordered.
pub async fn list_host_states(pool: &DbPool) -> Result<Vec<HostState>, AppError> {
    let rows = exec::fetch_all(
        pool,
        "SELECT host_id, instance_id, status, max_resource_slots, desired_version,
                applied_version, applied_sha256, applied_at, last_fetch_at,
                last_fetch_error, consecutive_fetch_failures, created_at, updated_at,
                CASE WHEN last_fetch_at >= datetime('now', '-300 seconds') THEN 1 ELSE 0 END AS live
         FROM sandbox_hosts ORDER BY host_id, instance_id",
        &[],
    )
    .await?;
    Ok(rows.into_iter().map(host_state).collect())
}

/// `(host_id, instance_id, document)` of every host's latest config version.
pub async fn latest_documents_all(
    pool: &DbPool,
) -> Result<Vec<(String, String, String)>, AppError> {
    let rows = exec::fetch_all(
        pool,
        "SELECT c.host_id, c.instance_id, c.document
             FROM sandbox_host_configs c
             JOIN (SELECT host_id, instance_id, MAX(version) AS version
                   FROM sandbox_host_configs GROUP BY host_id, instance_id) latest
               ON c.host_id = latest.host_id
              AND c.instance_id = latest.instance_id
              AND c.version = latest.version",
        &[],
    )
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| (r.text(0), r.text(1), r.text(2)))
        .collect())
}

/// `(host_id, sandbox_instance_id, name)` of every runner bound to a host.
pub async fn attached_runners_all(
    pool: &DbPool,
) -> Result<Vec<(String, String, String)>, AppError> {
    let rows = exec::fetch_all(
        pool,
        "SELECT host_id, sandbox_instance_id, name FROM runners
         WHERE host_id != '' ORDER BY name",
        &[],
    )
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| (r.text(0), r.text(1), r.text(2)))
        .collect())
}

/// Names of the runners reporting one host.
pub async fn runner_names_for_host(pool: &DbPool, host_id: &str) -> Result<Vec<String>, AppError> {
    Ok(exec::fetch_all(
        pool,
        "SELECT name FROM runners WHERE host_id = ? ORDER BY name",
        &[host_id.into()],
    )
    .await?
    .into_iter()
    .map(|r| r.text(0))
    .collect())
}

/// Runners still bound to one (host, instance) pair — the delete guard.
pub async fn runner_count_for_pair(
    pool: &DbPool,
    host_id: &str,
    instance_id: &str,
) -> Result<i64, AppError> {
    Ok(exec::fetch_scalar_i64(
        pool,
        "SELECT COUNT(*) FROM runners WHERE host_id = ? AND sandbox_instance_id = ?",
        &[host_id.into(), instance_id.into()],
    )
    .await?)
}

/// Enroll a (host, instance) pair; re-enrolling rotates the token and applies
/// the slot budget only when the request carries a positive one.
pub async fn enroll_host(
    pool: &DbPool,
    host_id: &str,
    instance_id: &str,
    max_resource_slots: i64,
    token_hash: &str,
) -> Result<(), AppError> {
    exec::execute(
        pool,
        "INSERT INTO sandbox_hosts (host_id, instance_id, status, max_resource_slots, token_hash)
         VALUES (?, ?, 'enrolled', ?, ?)
         ON CONFLICT(host_id, instance_id) DO UPDATE SET
           token_hash = excluded.token_hash,
           max_resource_slots = CASE WHEN excluded.max_resource_slots > 0
                                     THEN excluded.max_resource_slots
                                     ELSE sandbox_hosts.max_resource_slots END,
           updated_at = datetime('now')",
        &[
            host_id.into(),
            instance_id.into(),
            SqlValue::Int(max_resource_slots),
            token_hash.into(),
        ],
    )
    .await?;
    Ok(())
}

/// Delete one enrollment and cascade its pending changes and config
/// versions, all in one dispatcher transaction. The table names come from
/// the fixed list below (never caller text), so the interpolation stays
/// identifier-safe.
pub async fn delete_host_cascade(
    pool: &DbPool,
    host_id: &str,
    instance_id: &str,
) -> Result<(), AppError> {
    let mut tx = pool.begin_immediate().await?;
    for table in [
        "sandbox_config_changes",
        "sandbox_host_configs",
        "sandbox_hosts",
    ] {
        let sql = format!("DELETE FROM {table} WHERE host_id = ? AND instance_id = ?");
        tx.execute(&sql, &[host_id.into(), instance_id.into()])
            .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// `(desired_version, applied_version)` of one host — the version allocator's
/// inputs. Errors when the host is not enrolled.
pub async fn host_versions(
    pool: &DbPool,
    host_id: &str,
    instance_id: &str,
) -> Result<(i64, i64), AppError> {
    let row = exec::fetch_optional(
        pool,
        "SELECT desired_version, applied_version FROM sandbox_hosts
         WHERE host_id = ? AND instance_id = ?",
        &[host_id.into(), instance_id.into()],
    )
    .await?
    .ok_or(sqlx::Error::RowNotFound)?;
    Ok((row.i64(0), row.i64(1)))
}

/// Store one immutable config version row.
pub async fn insert_config_version(
    pool: &DbPool,
    host_id: &str,
    instance_id: &str,
    version: i64,
    document_json: &str,
    document_sha256: &str,
    created_by: &str,
) -> Result<(), AppError> {
    exec::execute(
        pool,
        "INSERT INTO sandbox_host_configs (host_id, instance_id, version, document, document_sha256, created_by)
         VALUES (?, ?, ?, ?, ?, ?)",
        &[
            host_id.into(),
            instance_id.into(),
            SqlValue::Int(version),
            document_json.into(),
            document_sha256.into(),
            created_by.into(),
        ],
    )
    .await?;
    Ok(())
}

/// Point the host's desired version at `version`.
pub async fn bump_desired_version(
    pool: &DbPool,
    host_id: &str,
    instance_id: &str,
    version: i64,
) -> Result<(), AppError> {
    exec::execute(
        pool,
        "UPDATE sandbox_hosts SET desired_version = ?, updated_at = datetime('now')
         WHERE host_id = ? AND instance_id = ?",
        &[SqlValue::Int(version), host_id.into(), instance_id.into()],
    )
    .await?;
    Ok(())
}

/// Version history of one host, newest first.
pub async fn config_versions(
    pool: &DbPool,
    host_id: &str,
    instance_id: &str,
) -> Result<Vec<(i64, String, String, String, String)>, AppError> {
    let rows = exec::fetch_all(
        pool,
        "SELECT version, document_sha256, created_by, created_at, document
         FROM sandbox_host_configs
         WHERE host_id = ? AND instance_id = ?
         ORDER BY version DESC",
        &[host_id.into(), instance_id.into()],
    )
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| (r.i64(0), r.text(1), r.text(2), r.text(3), r.text(4)))
        .collect())
}

/// One stored config version's document (`None` = no such version).
pub async fn config_document_at(
    pool: &DbPool,
    host_id: &str,
    instance_id: &str,
    version: i64,
) -> Result<Option<String>, AppError> {
    Ok(exec::fetch_optional(
        pool,
        "SELECT document FROM sandbox_host_configs
         WHERE host_id = ? AND instance_id = ? AND version = ?",
        &[host_id.into(), instance_id.into(), SqlValue::Int(version)],
    )
    .await?
    .map(|r| r.text(0)))
}

/// Rotate a host's sandboxd token hash.
pub async fn update_token_hash(
    pool: &DbPool,
    host_id: &str,
    instance_id: &str,
    token_hash: &str,
) -> Result<(), AppError> {
    exec::execute(
        pool,
        "UPDATE sandbox_hosts SET token_hash = ?, updated_at = datetime('now')
         WHERE host_id = ? AND instance_id = ?",
        &[token_hash.into(), host_id.into(), instance_id.into()],
    )
    .await?;
    Ok(())
}

/// Update a host's slot budget (0 = inherit the platform default).
pub async fn update_max_slots(
    pool: &DbPool,
    host_id: &str,
    instance_id: &str,
    max_resource_slots: i64,
) -> Result<(), AppError> {
    exec::execute(
        pool,
        "UPDATE sandbox_hosts SET max_resource_slots = ?, updated_at = datetime('now')
         WHERE host_id = ? AND instance_id = ?",
        &[
            SqlValue::Int(max_resource_slots),
            host_id.into(),
            instance_id.into(),
        ],
    )
    .await?;
    Ok(())
}

/// One pending-change INSERT: a platform-defaults catalog change.
pub async fn insert_defaults_pending_change(
    pool: &DbPool,
    id: &str,
    requested_by: &str,
    before_document: &str,
    after_document: &str,
) -> Result<(), AppError> {
    exec::execute(
        pool,
        "INSERT INTO sandbox_config_changes
             (id, host_id, instance_id, status, requested_by, before_document, after_document)
             VALUES (?, 'platform-defaults', 'defaults', 'pending', ?, ?, ?)",
        &[
            id.into(),
            requested_by.into(),
            before_document.into(),
            after_document.into(),
        ],
    )
    .await?;
    Ok(())
}

/// One pending-change INSERT: a per-host config document change.
pub async fn insert_host_pending_change(
    pool: &DbPool,
    id: &str,
    host_id: &str,
    instance_id: &str,
    requested_by: &str,
    before_document: &str,
    after_document: &str,
) -> Result<(), AppError> {
    exec::execute(
        pool,
        "INSERT INTO sandbox_config_changes
             (id, host_id, instance_id, status, requested_by, before_document, after_document)
             VALUES (?, ?, ?, 'pending', ?, ?, ?)",
        &[
            id.into(),
            host_id.into(),
            instance_id.into(),
            requested_by.into(),
            before_document.into(),
            after_document.into(),
        ],
    )
    .await?;
    Ok(())
}

/// One pending-change INSERT: a resource-profile grant request.
pub async fn insert_grant_pending_change(
    pool: &DbPool,
    id: &str,
    requested_by: &str,
    after_document: &str,
) -> Result<(), AppError> {
    exec::execute(
        pool,
        "INSERT INTO sandbox_config_changes
             (id, host_id, instance_id, status, requested_by, before_document, after_document)
             VALUES (?, 'resource-grants', 'grant', 'pending', ?, '{}', ?)",
        &[id.into(), requested_by.into(), after_document.into()],
    )
    .await?;
    Ok(())
}

/// One pending-change INSERT: a resource-profile revocation record.
pub async fn insert_revoke_pending_change(
    pool: &DbPool,
    id: &str,
    requested_by: &str,
    before_document: &str,
    after_document: &str,
) -> Result<(), AppError> {
    exec::execute(
        pool,
        "INSERT INTO sandbox_config_changes
             (id, host_id, instance_id, status, requested_by, before_document, after_document)
             VALUES (?, 'resource-grants', 'revoke', 'pending', ?, ?, ?)",
        &[
            id.into(),
            requested_by.into(),
            before_document.into(),
            after_document.into(),
        ],
    )
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
    pool: &DbPool,
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
    let binds: Vec<SqlValue> = status_filter
        .first()
        .map(|s| vec![s.as_str().into()])
        .unwrap_or_default();
    let count_sql = format!("SELECT COUNT(*) FROM sandbox_config_changes {where_clause}");
    let total = exec::fetch_scalar_i64(pool, &count_sql, &binds).await?;
    let sql = format!(
        "SELECT id, host_id, instance_id, status, requested_by, after_document, created_at, decided_at,
                approved_by, decision_comment
         FROM sandbox_config_changes {where_clause}
         ORDER BY CASE WHEN status = 'pending' THEN created_at ELSE decided_at END DESC
         LIMIT {} OFFSET {}",
        page_size,
        (page - 1) * page_size,
    );
    let rows: Vec<ChangeRow> = exec::fetch_all(pool, &sql, &binds)
        .await?
        .into_iter()
        .map(|r| {
            (
                r.text(0),
                r.text(1),
                r.text(2),
                r.text(3),
                r.text(4),
                r.text(5),
                r.opt_text(6),
                r.opt_text(7),
                r.opt_text(8),
                r.opt_text(9),
            )
        })
        .collect();
    Ok((total, rows))
}

/// `(status, requested_by, host_id, instance_id, after_document)` of one
/// pending change (`None` = no such change).
pub type ChangeFacts = (String, String, String, String, String);

pub async fn change_facts(pool: &DbPool, id: &str) -> Result<Option<ChangeFacts>, AppError> {
    Ok(exec::fetch_optional(
        pool,
        "SELECT status, requested_by, host_id, instance_id, after_document
         FROM sandbox_config_changes WHERE id = ?",
        &[id.into()],
    )
    .await?
    .map(|r| (r.text(0), r.text(1), r.text(2), r.text(3), r.text(4))))
}

/// Pipelines bound to one (team, profile) — the grant blast radius.
pub async fn pipeline_count_for_profile(
    pool: &DbPool,
    team_id: &str,
    profile: &str,
) -> Result<i64, AppError> {
    Ok(exec::fetch_scalar_i64(
        pool,
        "SELECT COUNT(*) FROM pipelines WHERE team_id = ? AND resource_profile = ?",
        &[team_id.into(), profile.into()],
    )
    .await?)
}

/// Pending jobs of one (team, profile) — the revocation blast radius that
/// will fail terminally at claim.
///
/// The snapshot walk carries explicit SQLite (`json_extract`) and Postgres
/// (`->>` over jsonb) bodies.
pub async fn queued_jobs_for_profile(
    pool: &DbPool,
    team_id: &str,
    profile: &str,
) -> Result<i64, AppError> {
    const SQL: Sql<'static> = Sql::Dual {
        sqlite: "SELECT COUNT(*) FROM jobs WHERE status = 'pending'
                 AND COALESCE(json_extract(snapshot, '$.team_id'), '') = ?
                 AND COALESCE(json_extract(snapshot, '$.resource_profile'), '') = ?",
        pg: "SELECT COUNT(*) FROM jobs WHERE status = 'pending'
             AND COALESCE(snapshot::jsonb ->> 'team_id', '') = $1
             AND COALESCE(snapshot::jsonb ->> 'resource_profile', '') = $2",
    };
    Ok(exec::fetch_scalar_i64_sql(pool, SQL, &[team_id.into(), profile.into()]).await?)
}

/// Grant (or re-grant) one team profile.
pub async fn replace_grant(
    pool: &DbPool,
    team_id: &str,
    profile: &str,
    granted_by: &str,
) -> Result<(), AppError> {
    // Portable upsert on the primary key: a re-grant refreshes `granted_by`
    // in place (SQLite's `INSERT OR REPLACE` would have swapped the row).
    const SQL: Sql<'static> = Sql::Shared(
        "INSERT INTO team_resource_grants (team_id, profile, granted_by)
         VALUES (?, ?, ?)
         ON CONFLICT (team_id, profile) DO UPDATE SET granted_by = EXCLUDED.granted_by",
    );
    exec::execute_sql(
        pool,
        SQL,
        &[team_id.into(), profile.into(), granted_by.into()],
    )
    .await?;
    Ok(())
}

/// Grant one team profile, ignoring an existing row.
pub async fn insert_grant_ignore(
    pool: &DbPool,
    team_id: &str,
    profile: &str,
    granted_by: &str,
) -> Result<(), AppError> {
    exec::execute(
        pool,
        "INSERT INTO team_resource_grants (team_id, profile, granted_by)
         VALUES (?, ?, ?)
         ON CONFLICT(team_id, profile) DO NOTHING",
        &[team_id.into(), profile.into(), granted_by.into()],
    )
    .await?;
    Ok(())
}

/// Revoke one team profile; `false` when it was not granted.
pub async fn delete_grant(pool: &DbPool, team_id: &str, profile: &str) -> Result<bool, AppError> {
    let affected = exec::execute(
        pool,
        "DELETE FROM team_resource_grants WHERE team_id = ? AND profile = ?",
        &[team_id.into(), profile.into()],
    )
    .await?;
    Ok(affected != 0)
}

/// Mark a pending change approved.
pub async fn approve_change_row(
    pool: &DbPool,
    id: &str,
    approved_by: &str,
    decision_comment: Option<&str>,
) -> Result<(), AppError> {
    exec::execute(
        pool,
        "UPDATE sandbox_config_changes SET status = 'approved', approved_by = ?,
                decision_comment = ?, decided_at = datetime('now') WHERE id = ?",
        &[approved_by.into(), decision_comment.into(), id.into()],
    )
    .await?;
    Ok(())
}

/// Mark a pending change rejected.
pub async fn reject_change_row(
    pool: &DbPool,
    id: &str,
    rejected_by: &str,
    decision_comment: Option<&str>,
) -> Result<(), AppError> {
    exec::execute(
        pool,
        "UPDATE sandbox_config_changes SET status = 'rejected', approved_by = ?,
            decision_comment = ?, decided_at = datetime('now') WHERE id = ?",
        &[rejected_by.into(), decision_comment.into(), id.into()],
    )
    .await?;
    Ok(())
}

/// The full grant allowlist with team names, name/profile-ordered.
pub async fn list_grants(
    pool: &DbPool,
) -> Result<Vec<(String, String, String, Option<String>)>, AppError> {
    Ok(exec::fetch_all(
        pool,
        "SELECT g.team_id, t.name, g.profile, g.created_at
         FROM team_resource_grants g JOIN teams t ON t.id = g.team_id
         ORDER BY t.name, g.profile",
        &[],
    )
    .await?
    .into_iter()
    .map(|r| (r.text(0), r.text(1), r.text(2), r.opt_text(3)))
    .collect())
}

/// True when the team exists — the grant path's referential check.
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

/// `(host_id, instance_id)` for a sandboxd bearer-token hash (`None` = not
/// enrolled or the token was rotated away).
pub async fn sandbox_host_for_token_hash(
    pool: &DbPool,
    token_hash: &str,
) -> Result<Option<(String, String)>, AppError> {
    Ok(exec::fetch_optional(
        pool,
        "SELECT host_id, instance_id FROM sandbox_hosts WHERE token_hash = ? AND token_hash != ''",
        &[token_hash.into()],
    )
    .await?
    .map(|r| (r.text(0), r.text(1))))
}

/// One host row plus its current desired document (platform-defaults bump).
pub struct HostDocumentRow {
    pub host_id: String,
    pub instance_id: String,
    pub desired_version: i64,
    pub applied_version: i64,
    pub desired_document: Option<String>,
}

/// Hosts with their desired config document — the defaults-change scan.
pub async fn host_documents_for_bump(pool: &DbPool) -> Result<Vec<HostDocumentRow>, AppError> {
    Ok(exec::fetch_all(
        pool,
        "SELECT h.host_id, h.instance_id, h.desired_version, h.applied_version,
                c.document AS desired_document
         FROM sandbox_hosts h
         LEFT JOIN sandbox_host_configs c
           ON c.host_id = h.host_id AND c.instance_id = h.instance_id
              AND c.version = h.desired_version",
        &[],
    )
    .await?
    .into_iter()
    .map(|r| HostDocumentRow {
        host_id: r.text(0),
        instance_id: r.text(1),
        desired_version: r.i64(2),
        applied_version: r.i64(3),
        desired_document: r.opt_text(4),
    })
    .collect())
}

/// Store one host's bumped document version and point the host at it.
pub async fn bump_host_desired_version(
    pool: &DbPool,
    host_id: &str,
    instance_id: &str,
    version: i64,
    document: &str,
    document_sha256: &str,
) -> Result<(), AppError> {
    exec::execute(
        pool,
        "INSERT INTO sandbox_host_configs
         (host_id, instance_id, version, document, document_sha256, created_by)
         VALUES (?, ?, ?, ?, ?, 'platform-defaults')
         ON CONFLICT DO NOTHING",
        &[
            host_id.into(),
            instance_id.into(),
            SqlValue::Int(version),
            document.into(),
            document_sha256.into(),
        ],
    )
    .await?;
    exec::execute(
        pool,
        "UPDATE sandbox_hosts SET desired_version = ?, updated_at = datetime('now')
         WHERE host_id = ? AND instance_id = ?",
        &[SqlValue::Int(version), host_id.into(), instance_id.into()],
    )
    .await?;
    Ok(())
}

// ────────────── claim-path reads (inside the claim transaction) ──────────────
//
// `slot_scheduler::decide` runs inside the claim's BEGIN IMMEDIATE; these are
// its storage reads, named for the fact each returns. They take the claim
// transaction so every read shares its lock.

/// `(max_resource_slots, desired_version, applied_version)` of one host —
/// the slot-budget facts (`None` = host not enrolled).
pub async fn host_slot_facts(
    tx: &mut DbTx<'_>,
    host_id: &str,
    instance_id: &str,
) -> Result<Option<(i64, i64, i64)>, AppError> {
    Ok(tx
        .fetch_optional(
            "SELECT max_resource_slots, desired_version, applied_version
         FROM sandbox_hosts WHERE host_id = ? AND instance_id = ?",
            &[host_id.into(), instance_id.into()],
        )
        .await?
        .map(|r| (r.i64(0), r.i64(1), r.i64(2))))
}

/// Stored config document at one version, read through the claim
/// transaction (`None` = no such version). Transaction-based core of
/// [`config_document_at`].
pub async fn config_document_at_conn(
    tx: &mut DbTx<'_>,
    host_id: &str,
    instance_id: &str,
    version: i64,
) -> Result<Option<String>, AppError> {
    Ok(tx
        .fetch_optional(
            "SELECT document FROM sandbox_host_configs
         WHERE host_id = ? AND instance_id = ? AND version = ?",
            &[host_id.into(), instance_id.into(), SqlValue::Int(version)],
        )
        .await?
        .map(|r| r.text(0)))
}

/// `sandbox_default_max_slots` platform setting (0 = unlimited).
pub async fn default_max_slots_setting(tx: &mut DbTx<'_>) -> Result<i64, AppError> {
    Ok(tx
        .fetch_optional(
            "SELECT COALESCE(CAST(value AS INTEGER), 0) FROM platform_settings WHERE key = 'sandbox_default_max_slots'",
            &[],
        )
        .await?
        .map(|r| r.i64(0))
        .unwrap_or(0))
}

/// `sandbox_default_profiles` platform setting JSON (`None` = unset).
pub async fn default_profiles_setting(tx: &mut DbTx<'_>) -> Result<Option<String>, AppError> {
    Ok(tx
        .fetch_optional(
            "SELECT value FROM platform_settings WHERE key = 'sandbox_default_profiles'",
            &[],
        )
        .await?
        .map(|r| r.text(0)))
}

/// True when the config row predates the staleness window close — a host
/// lagging its desired version for too long stops dispatching non-default
/// profiles.
pub async fn config_row_is_stale(
    tx: &mut DbTx<'_>,
    stale_seconds: i64,
    host_id: &str,
    instance_id: &str,
    version: i64,
) -> Result<bool, AppError> {
    Ok(tx
        .fetch_optional(
            "SELECT CASE WHEN datetime(created_at) <= datetime('now', ? || ' seconds') THEN 1 ELSE 0 END
         FROM sandbox_host_configs WHERE host_id = ? AND instance_id = ? AND version = ?",
            &[
                format!("-{}", stale_seconds.max(0)).into(),
                host_id.into(),
                instance_id.into(),
                SqlValue::Int(version),
            ],
        )
        .await?
        .map(|r| r.i64(0) != 0)
        .unwrap_or(false))
}

/// Slots currently in use by running jobs on one sandboxd instance — the
/// budget's occupancy half.
///
/// Superseded for dispatch decisions by [`reconcile_running_slots_tx`]: the
/// plain SUM read is not serialized between concurrent claim transactions,
/// which on Postgres lets two claimants both observe the pre-claim sum and
/// oversell the budget (write skew).
pub async fn running_slots_used(tx: &mut DbTx<'_>, sandbox_key: &str) -> Result<i64, AppError> {
    Ok(tx
        .fetch_optional(
            "SELECT CAST(COALESCE(SUM(resource_slots), 0) AS BIGINT) FROM jobs
         WHERE status = 'running' AND sandbox_instance_id = ?",
            &[sandbox_key.into()],
        )
        .await?
        .map(|r| r.i64(0))
        .unwrap_or(0))
}

/// Refresh the host's `running_slots` occupancy cache from the running-jobs
/// sum and return the reconciled value — inside the claim transaction.
///
/// Serialization point between concurrent claimants, in two steps:
///
/// 1. **Lock + freshness anchor** (`Sql::Dual`): Postgres takes `FOR UPDATE`
///    on the host row — a claimant blocked here resumes only after the other
///    claimant committed, and its following statements then run under a fresh
///    READ COMMITTED snapshot that includes the just-claimed job. SQLite
///    needs no `FOR UPDATE` (the `BEGIN IMMEDIATE` writer lock already
///    serialises the whole transaction); the plain SELECT is the same anchor.
/// 2. **Guarded reconcile UPDATE**: recomputes `running_slots` from the
///    running-jobs sum — a derived cache, refresh-on-read. Job completion
///    frees slots at the next candidate's decide; there is no decrement
///    bookkeeping to miss.
///
/// A single `UPDATE ... SET col = (subquery)` cannot replace this pair: when
/// its row lock waits, Postgres re-checks the WHERE but does **not**
/// re-evaluate the target-list subquery (EvalPlanQual keeps the old
/// snapshot), so the write would carry a stale sum — exactly the oversell
/// the concurrent-polls test caught.
///
/// `None` = host not enrolled.
pub async fn reconcile_running_slots_tx(
    tx: &mut DbTx<'_>,
    host_id: &str,
    instance_id: &str,
    sandbox_key: &str,
) -> Result<Option<i64>, AppError> {
    const LOCK_HOST_ROW: Sql<'static> = Sql::Dual {
        sqlite: "SELECT running_slots FROM sandbox_hosts
             WHERE host_id = ? AND instance_id = ?",
        pg: "SELECT running_slots FROM sandbox_hosts
             WHERE host_id = $1 AND instance_id = $2
             FOR UPDATE",
    };
    let _locked = tx
        .fetch_optional_sql(LOCK_HOST_ROW, &[host_id.into(), instance_id.into()])
        .await?;
    Ok(tx
        .fetch_optional_sql(
            Sql::Shared(
                "UPDATE sandbox_hosts
         SET running_slots =
             (SELECT CAST(COALESCE(SUM(resource_slots), 0) AS BIGINT) FROM jobs
              WHERE status = 'running' AND sandbox_instance_id = ?)
         WHERE host_id = ? AND instance_id = ?
         RETURNING running_slots",
            ),
            &[sandbox_key.into(), host_id.into(), instance_id.into()],
        )
        .await?
        .map(|r| r.i64(0)))
}
