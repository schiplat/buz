//! Aggregate metrics samples for the unauthenticated `/metrics` scrape.
//!
//! Counts only — no identifiers. Every query is best-effort: an error yields
//! `None`/empty and the scrape degrades instead of failing.

use std::collections::HashMap;

use sqlx::{Row, SqlitePool};

/// Grouped job counts by status.
pub async fn job_status_counts(pool: &SqlitePool) -> HashMap<String, i64> {
    let rows = sqlx::query("SELECT status, COUNT(*) AS cnt FROM jobs GROUP BY status")
        .fetch_all(pool)
        .await;
    match rows {
        Ok(rows) => rows
            .into_iter()
            .map(|r| (r.get::<String, _>(0), r.get::<i64, _>(1)))
            .collect(),
        Err(e) => {
            tracing::warn!(error = %e, "metrics: failed to sample job counts");
            HashMap::new()
        }
    }
}

/// Best-effort scalar count; `None` when the query fails (omit the line).
async fn scalar(pool: &SqlitePool, sql: &str) -> Option<i64> {
    match sqlx::query_scalar(sql).fetch_one(pool).await {
        Ok(v) => Some(v),
        Err(e) => {
            tracing::warn!(error = %e, sql, "metrics: count query failed");
            None
        }
    }
}

#[derive(Debug, Default)]
pub struct MetricCounts {
    pub runners_total: Option<i64>,
    pub runners_frozen: Option<i64>,
    pub pipelines_total: Option<i64>,
    pub pipelines_active: Option<i64>,
}

/// One sample of the entity counts behind the `/metrics` gauges.
pub async fn entity_counts(pool: &SqlitePool) -> MetricCounts {
    MetricCounts {
        runners_total: scalar(pool, "SELECT COUNT(*) FROM runners").await,
        runners_frozen: scalar(pool, "SELECT COUNT(*) FROM runners WHERE is_frozen != 0").await,
        pipelines_total: scalar(pool, "SELECT COUNT(*) FROM pipelines").await,
        pipelines_active: scalar(pool, "SELECT COUNT(*) FROM pipelines WHERE is_active = 1").await,
    }
}
