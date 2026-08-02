//! Unified pipeline trigger event log (git / webhook / manual / cron).

use axum::{
    extract::{Path, Query, State},
    Json,
};
use shared::error::AppError;
use shared::protocol::response::ApiResponse;
use shared::rbac::Perm;
use sqlx::Row;
use uuid::Uuid;

use crate::rbac::AuthUser;
use crate::state::AppState;

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

pub async fn insert_event(
    pool: &sqlx::SqlitePool,
    e: &TriggerEventWrite,
) -> Result<String, AppError> {
    let id = Uuid::new_v4().to_string();
    let delivery = e
        .delivery_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    sqlx::query(
        "INSERT INTO pipeline_trigger_events (
            id, pipeline_id, source, provider, event_type, delivery_id,
            repo_full_name, git_ref, branch, tag, commit_sha, commit_message,
            sender, compare_url, received_at, decision, skip_reason, job_id, created_at
         ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, datetime('now'), ?, ?, ?, datetime('now'))",
    )
    .bind(&id)
    .bind(&e.pipeline_id)
    .bind(&e.source)
    .bind(&e.provider)
    .bind(&e.event_type)
    .bind(delivery)
    .bind(&e.repo_full_name)
    .bind(&e.git_ref)
    .bind(&e.branch)
    .bind(&e.tag)
    .bind(&e.commit_sha)
    .bind(&e.commit_message)
    .bind(&e.sender)
    .bind(&e.compare_url)
    .bind(&e.decision)
    .bind(&e.skip_reason)
    .bind(&e.job_id)
    .execute(pool)
    .await?;
    Ok(id)
}

pub async fn find_by_delivery(
    pool: &sqlx::SqlitePool,
    pipeline_id: &str,
    delivery_id: &str,
) -> Result<Option<(String, Option<String>, String)>, AppError> {
    let delivery_id = delivery_id.trim();
    if delivery_id.is_empty() {
        return Ok(None);
    }
    let row = sqlx::query(
        "SELECT id, job_id, decision FROM pipeline_trigger_events
         WHERE pipeline_id = ? AND delivery_id = ?",
    )
    .bind(pipeline_id)
    .bind(delivery_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| (r.get(0), r.get(1), r.get(2))))
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
pub async fn claim_delivery(
    pool: &sqlx::SqlitePool,
    e: &TriggerEventWrite,
) -> Result<ClaimResult, AppError> {
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
    let res = sqlx::query(
        "INSERT OR IGNORE INTO pipeline_trigger_events (
            id, pipeline_id, source, provider, event_type, delivery_id,
            repo_full_name, git_ref, branch, tag, commit_sha, commit_message,
            sender, compare_url, received_at, decision, skip_reason, job_id, created_at
         ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, datetime('now'), 'processing', '', NULL, datetime('now'))",
    )
    .bind(&id)
    .bind(&e.pipeline_id)
    .bind(&e.source)
    .bind(&e.provider)
    .bind(&e.event_type)
    .bind(did)
    .bind(&e.repo_full_name)
    .bind(&e.git_ref)
    .bind(&e.branch)
    .bind(&e.tag)
    .bind(&e.commit_sha)
    .bind(&e.commit_message)
    .bind(&e.sender)
    .bind(&e.compare_url)
    .execute(pool)
    .await?;

    if res.rows_affected() == 1 {
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
    pool: &sqlx::SqlitePool,
    event_id: &str,
    decision: &str,
    skip_reason: &str,
    job_id: Option<&str>,
) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE pipeline_trigger_events
         SET decision = ?, skip_reason = ?, job_id = ?
         WHERE id = ?",
    )
    .bind(decision)
    .bind(skip_reason)
    .bind(job_id)
    .bind(event_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Drop a claimed row so a failed trigger can be retried by the platform.
pub async fn release_claim(pool: &sqlx::SqlitePool, event_id: &str) -> Result<(), AppError> {
    sqlx::query("DELETE FROM pipeline_trigger_events WHERE id = ? AND decision = 'processing'")
        .bind(event_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Record a simple non-git trigger after a successful `do_trigger`.
pub async fn record_simple_trigger(
    pool: &sqlx::SqlitePool,
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

#[derive(Debug, serde::Deserialize)]
pub struct ListQuery {
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub decision: Option<String>,
    #[serde(default = "default_page")]
    pub page: u32,
    #[serde(default = "default_page_size")]
    pub page_size: u32,
}

fn default_page() -> u32 {
    1
}
fn default_page_size() -> u32 {
    20
}

/// GET /pipelines/:id/trigger-events
pub async fn list(
    auth: AuthUser,
    Path(pipeline_id): Path<String>,
    State(state): State<AppState>,
    Query(q): Query<ListQuery>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let team_id: String = sqlx::query_scalar("SELECT team_id FROM pipelines WHERE id = ?")
        .bind(&pipeline_id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(|| AppError::NotFound("pipeline not found".into()))?;
    auth.check_team(&team_id, Perm::TeamView)?;

    let page = q.page.max(1);
    let page_size = q.page_size.clamp(1, 100);
    let offset = (page - 1) * page_size;

    // Debug (unredacted) jobs are admin-only records — their trigger events
    // (and the clickable job link in the UI) must not surface to non-admins.
    let debug_filter = if auth.is_platform_admin {
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

    if let Some(ref s) = q.source {
        if !s.is_empty() {
            sql.push_str(" AND e.source = ?");
            count_sql.push_str(" AND source = ?");
        }
    }
    if let Some(ref d) = q.decision {
        if !d.is_empty() {
            sql.push_str(" AND e.decision = ?");
            count_sql.push_str(" AND decision = ?");
        }
    }
    sql.push_str(&debug_filter);
    count_sql.push_str(&debug_filter);
    sql.push_str(" ORDER BY e.received_at DESC LIMIT ? OFFSET ?");

    let mut count_q = sqlx::query_scalar::<_, i64>(&count_sql).bind(&pipeline_id);
    let mut list_q = sqlx::query(&sql).bind(&pipeline_id);

    if let Some(ref s) = q.source {
        if !s.is_empty() {
            count_q = count_q.bind(s);
            list_q = list_q.bind(s);
        }
    }
    if let Some(ref d) = q.decision {
        if !d.is_empty() {
            count_q = count_q.bind(d);
            list_q = list_q.bind(d);
        }
    }
    list_q = list_q.bind(page_size as i64).bind(offset as i64);

    let total: i64 = count_q.fetch_one(&state.pool).await?;
    let rows = list_q.fetch_all(&state.pool).await?;

    let items: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|r| {
            serde_json::json!({
                "id": r.get::<String, _>(0),
                "source": r.get::<String, _>(1),
                "provider": r.get::<Option<String>, _>(2),
                "event_type": r.get::<String, _>(3),
                "delivery_id": r.get::<Option<String>, _>(4),
                "repo_full_name": r.get::<String, _>(5),
                "ref": r.get::<String, _>(6),
                "git_ref": r.get::<String, _>(6),
                "branch": r.get::<String, _>(7),
                "tag": r.get::<String, _>(8),
                "commit_sha": r.get::<String, _>(9),
                "commit_message": r.get::<String, _>(10),
                "sender": r.get::<String, _>(11),
                "compare_url": r.get::<String, _>(12),
                "received_at": r.get::<String, _>(13),
                "decision": r.get::<String, _>(14),
                "skip_reason": r.get::<String, _>(15),
                "job_id": r.get::<Option<String>, _>(16),
                "job_status": r.get::<Option<String>, _>(17),
            })
        })
        .collect();

    Ok(Json(ApiResponse::success(serde_json::json!({
        "items": items,
        "total": total,
        "page": page,
        "page_size": page_size,
    }))))
}
