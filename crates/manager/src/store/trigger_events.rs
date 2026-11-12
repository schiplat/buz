//! Pipeline trigger event log (git / webhook / manual / cron): writes,
//! delivery-id claims, finalization and the team-scoped listing.
//!
//! `claim_delivery` is the webhook idempotency gate: the partial unique index
//! on `(pipeline_id, delivery_id)` decides who proceeds to trigger, so no
//! transaction is held — `INSERT OR IGNORE` is the serialisation point.
//!
//! On the dual-backend dispatcher (`store::exec`): statements travel as plain
//! SQLite-dialect strings with backend-neutral binds, rows decode by column
//! name. `INSERT OR IGNORE` is rewriter-covered — it becomes
//! `ON CONFLICT DO NOTHING` on the Postgres path.

use crate::store::exec::{self, DbPool, SqlValue};

use shared::error::AppError;
use uuid::Uuid;

#[derive(Debug, Clone, Default)]
pub struct TriggerEventWrite {
    pub pipeline_id: String,
    pub source: String,
    pub provider: Option<String>,
    pub event_type: String,
    pub delivery_id: Option<String>,
    pub repo_full_name: String,
    pub git_ref: String,
    pub branch: String,
    pub tag: String,
    pub commit_sha: String,
    pub commit_message: String,
    pub sender: String,
    pub compare_url: String,
    pub decision: String,
    pub skip_reason: String,
    pub job_id: Option<String>,
}

pub async fn insert_event(pool: &DbPool, e: &TriggerEventWrite) -> Result<String, AppError> {
    let id = Uuid::new_v4().to_string();
    let delivery = e
        .delivery_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    exec::execute(
        pool,
        "INSERT INTO pipeline_trigger_events (
            id, pipeline_id, source, provider, event_type, delivery_id,
            repo_full_name, git_ref, branch, tag, commit_sha, commit_message,
            sender, compare_url, received_at, decision, skip_reason, job_id, created_at
         ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, datetime('now'), ?, ?, ?, datetime('now'))",
        &[
            id.as_str().into(),
            e.pipeline_id.as_str().into(),
            e.source.as_str().into(),
            e.provider.as_deref().into(),
            e.event_type.as_str().into(),
            delivery.into(),
            e.repo_full_name.as_str().into(),
            e.git_ref.as_str().into(),
            e.branch.as_str().into(),
            e.tag.as_str().into(),
            e.commit_sha.as_str().into(),
            e.commit_message.as_str().into(),
            e.sender.as_str().into(),
            e.compare_url.as_str().into(),
            e.decision.as_str().into(),
            e.skip_reason.as_str().into(),
            e.job_id.as_deref().into(),
        ],
    )
    .await?;
    Ok(id)
}

pub async fn find_by_delivery(
    pool: &DbPool,
    pipeline_id: &str,
    delivery_id: &str,
) -> Result<Option<(String, Option<String>, String)>, AppError> {
    let delivery_id = delivery_id.trim();
    if delivery_id.is_empty() {
        return Ok(None);
    }
    let row = exec::fetch_optional(
        pool,
        "SELECT id, job_id, decision FROM pipeline_trigger_events
         WHERE pipeline_id = ? AND delivery_id = ?",
        &[pipeline_id.into(), delivery_id.into()],
    )
    .await?;
    Ok(row.map(|r| {
        (
            r.text_of("id"),
            r.opt_text_of("job_id"),
            r.text_of("decision"),
        )
    }))
}

/// Result of claiming a delivery slot before `do_trigger` (prevents double jobs).
#[derive(Debug)]
pub enum ClaimResult {
    /// This request owns the row and should proceed to filter/trigger.
    Claimed { event_id: String },
    /// Another request already claimed or finished this delivery.
    Existing {
        event_id: String,
        job_id: Option<String>,
        decision: String,
    },
}

/// Atomically claim `(pipeline_id, delivery_id)` with `decision=processing`.
/// Uses `INSERT OR IGNORE` against the partial unique index so concurrent
/// webhooks cannot both pass the old find-then-insert check.
pub async fn claim_delivery(pool: &DbPool, e: &TriggerEventWrite) -> Result<ClaimResult, AppError> {
    let delivery = e
        .delivery_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());

    // No stable key → insert a normal row (no cross-request idempotency).
    let Some(did) = delivery else {
        let mut row = e.clone();
        if row.decision.is_empty() {
            row.decision = "processing".into();
        }
        let id = insert_event(pool, &row).await?;
        return Ok(ClaimResult::Claimed { event_id: id });
    };

    let id = Uuid::new_v4().to_string();
    let n = exec::execute(
        pool,
        "INSERT OR IGNORE INTO pipeline_trigger_events (
            id, pipeline_id, source, provider, event_type, delivery_id,
            repo_full_name, git_ref, branch, tag, commit_sha, commit_message,
            sender, compare_url, received_at, decision, skip_reason, job_id, created_at
         ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, datetime('now'), 'processing', '', NULL, datetime('now'))",
        &[
            id.as_str().into(),
            e.pipeline_id.as_str().into(),
            e.source.as_str().into(),
            e.provider.as_deref().into(),
            e.event_type.as_str().into(),
            did.into(),
            e.repo_full_name.as_str().into(),
            e.git_ref.as_str().into(),
            e.branch.as_str().into(),
            e.tag.as_str().into(),
            e.commit_sha.as_str().into(),
            e.commit_message.as_str().into(),
            e.sender.as_str().into(),
            e.compare_url.as_str().into(),
        ],
    )
    .await?;

    if n == 1 {
        return Ok(ClaimResult::Claimed { event_id: id });
    }

    match find_by_delivery(pool, &e.pipeline_id, did).await? {
        Some((event_id, job_id, decision)) => Ok(ClaimResult::Existing {
            event_id,
            job_id,
            decision,
        }),
        None => {
            // Extremely rare: ignore raced with delete. Fall back to insert.
            let mut row = e.clone();
            row.decision = "processing".into();
            let event_id = insert_event(pool, &row).await?;
            Ok(ClaimResult::Claimed { event_id })
        }
    }
}

pub async fn finalize_event(
    pool: &DbPool,
    event_id: &str,
    decision: &str,
    skip_reason: &str,
    job_id: Option<&str>,
) -> Result<(), AppError> {
    exec::execute(
        pool,
        "UPDATE pipeline_trigger_events
         SET decision = ?, skip_reason = ?, job_id = ?
         WHERE id = ?",
        &[
            decision.into(),
            skip_reason.into(),
            job_id.into(),
            event_id.into(),
        ],
    )
    .await?;
    Ok(())
}

/// Drop a claimed row so a failed trigger can be retried by the platform.
pub async fn release_claim(pool: &DbPool, event_id: &str) -> Result<(), AppError> {
    exec::execute(
        pool,
        "DELETE FROM pipeline_trigger_events WHERE id = ? AND decision = 'processing'",
        &[event_id.into()],
    )
    .await?;
    Ok(())
}

/// Record a simple non-git trigger after a successful `do_trigger`.
pub async fn record_simple_trigger(
    pool: &DbPool,
    pipeline_id: &str,
    source: &str,
    triggered_by: &str,
    job_id: &str,
) {
    let sender = if source == "manual" {
        triggered_by.to_string()
    } else {
        String::new()
    };
    let ev = TriggerEventWrite {
        pipeline_id: pipeline_id.to_string(),
        source: source.to_string(),
        event_type: source.to_string(),
        sender,
        decision: "triggered".into(),
        job_id: Some(job_id.to_string()),
        ..Default::default()
    };
    if let Err(e) = insert_event(pool, &ev).await {
        tracing::warn!("failed to record trigger event ({source}): {e}");
    }
}

/// Listing filters for `list_events`; `None` / empty means "no filter".
#[derive(Debug, Default)]
pub struct ListEventsFilter {
    pub source: Option<String>,
    pub decision: Option<String>,
}

/// One page of a pipeline's trigger events joined with their job status,
/// plus the total count after filtering. `include_debug_jobs` is `false`
/// unless the caller is a platform admin — debug (unredacted) jobs are
/// admin-only records, and their events must not surface to anyone else.
pub async fn list_events(
    pool: &DbPool,
    pipeline_id: &str,
    filter: ListEventsFilter,
    include_debug_jobs: bool,
    limit: i64,
    offset: i64,
) -> Result<(i64, Vec<serde_json::Value>), AppError> {
    // Debug (unredacted) jobs are admin-only records — their trigger events
    // (and the clickable job link in the UI) must not surface to non-admins.
    let debug_filter = if include_debug_jobs {
        String::new()
    } else {
        " AND (e.job_id IS NULL OR NOT EXISTS (
             SELECT 1 FROM jobs j2 WHERE j2.id = e.job_id AND j2.debug_unredact = 1))"
            .to_string()
    };

    let mut sql = String::from(
        "SELECT e.id, e.source, e.provider, e.event_type, e.delivery_id,
                e.repo_full_name, e.git_ref, e.branch, e.tag, e.commit_sha, e.commit_message,
                e.sender, e.compare_url, e.received_at, e.decision, e.skip_reason, e.job_id,
                j.status
         FROM pipeline_trigger_events e
         LEFT JOIN jobs j ON j.id = e.job_id
         WHERE e.pipeline_id = ?",
    );
    let mut count_sql =
        String::from("SELECT COUNT(*) FROM pipeline_trigger_events e WHERE e.pipeline_id = ?");

    if let Some(ref s) = filter.source {
        if !s.is_empty() {
            sql.push_str(" AND e.source = ?");
            count_sql.push_str(" AND source = ?");
        }
    }
    if let Some(ref d) = filter.decision {
        if !d.is_empty() {
            sql.push_str(" AND e.decision = ?");
            count_sql.push_str(" AND decision = ?");
        }
    }
    sql.push_str(&debug_filter);
    count_sql.push_str(&debug_filter);
    sql.push_str(" ORDER BY e.received_at DESC LIMIT ? OFFSET ?");

    // Shared prefix binds (pipeline, source, decision) feed both statements in
    // the same order; the list query appends LIMIT/OFFSET.
    let mut binds: Vec<SqlValue> = vec![pipeline_id.into()];
    if let Some(ref s) = filter.source {
        if !s.is_empty() {
            binds.push(s.as_str().into());
        }
    }
    if let Some(ref d) = filter.decision {
        if !d.is_empty() {
            binds.push(d.as_str().into());
        }
    }

    let total = exec::fetch_scalar_i64(pool, &count_sql, &binds).await?;

    let mut list_binds = binds.clone();
    list_binds.push(SqlValue::Int(limit));
    list_binds.push(SqlValue::Int(offset));
    let rows = exec::fetch_all(pool, &sql, &list_binds).await?;

    let items: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|r| {
            serde_json::json!({
                "id": r.text_of("id"),
                "source": r.text_of("source"),
                "provider": r.opt_text_of("provider"),
                "event_type": r.text_of("event_type"),
                "delivery_id": r.opt_text_of("delivery_id"),
                "repo_full_name": r.text_of("repo_full_name"),
                "ref": r.text_of("git_ref"),
                "git_ref": r.text_of("git_ref"),
                "branch": r.text_of("branch"),
                "tag": r.text_of("tag"),
                "commit_sha": r.text_of("commit_sha"),
                "commit_message": r.text_of("commit_message"),
                "sender": r.text_of("sender"),
                "compare_url": r.text_of("compare_url"),
                "received_at": r.text_of("received_at"),
                "decision": r.text_of("decision"),
                "skip_reason": r.text_of("skip_reason"),
                "job_id": r.opt_text_of("job_id"),
                "job_status": r.opt_text_of("status"),
            })
        })
        .collect();

    Ok((total, items))
}
