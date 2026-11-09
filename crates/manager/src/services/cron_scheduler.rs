use sqlx::{Row, SqlitePool};
use std::str::FromStr;
use std::time::Duration;
use tokio::time;

use crate::state::AppState;

/// Minimum interval between cron triggers for the same pipeline (seconds).
/// Prevents double-firing when the scheduler ticks more often than the
/// cron expression granularity.
const MIN_TRIGGER_GAP_SECS: i64 = 120;

/// Normalize a cron expression to 7-field form accepted by the `cron` 0.15 crate.
/// Accepts both 5-field (min hr dom mon dow) and 7-field formats.
pub fn normalize_cron(expr: &str) -> String {
    let fields: Vec<&str> = expr.split_whitespace().collect();
    match fields.len() {
        5 => format!("0 {} *", expr),
        6 => format!("{} *", expr), // sec + 5 fields → add year wildcard
        7 => expr.to_string(),
        _ => {
            // Unrecognised format — best-effort
            tracing::warn!(
                "Unrecognised cron expression ({} fields): '{}'",
                fields.len(),
                expr
            );
            expr.to_string()
        }
    }
}

/// Periodically scan active cron pipelines and trigger jobs when their
/// cron expression matches the current time.
pub fn spawn_scheduler(state: AppState, check_interval_seconds: u64) {
    tokio::spawn(async move {
        tracing::info!(
            "Cron scheduler started (check interval: {}s, min gap: {}s)",
            check_interval_seconds,
            MIN_TRIGGER_GAP_SECS
        );

        let instance_id = uuid::Uuid::new_v4().to_string();
        tracing::info!("Cron scheduler instance: {instance_id}");

        let mut interval = time::interval(Duration::from_secs(check_interval_seconds));

        loop {
            interval.tick().await;

            // Single-leader guard: with two manager instances only the lease
            // holder scans. Shares the `scheduler_lease` table with the
            // dispatch scheduler on a dedicated row so the two never evict
            // each other.
            match crate::services::scheduler::try_acquire_lease(
                &state.pool,
                crate::services::scheduler::CRON_TRIGGER_LEASE_ID,
                &instance_id,
            )
            .await
            {
                Ok(true) => {}
                Ok(false) => continue, // another instance leads cron triggers
                Err(e) => {
                    tracing::error!("Cron scheduler lease error: {e}");
                    continue;
                }
            }

            let pipelines = match fetch_active_cron_pipelines(&state.pool).await {
                Ok(p) => p,
                Err(e) => {
                    tracing::error!("Cron scheduler query failed: {e}");
                    continue;
                }
            };

            if pipelines.is_empty() {
                continue;
            }

            for (pipeline_id, pipeline_name, cron_expr, environment) in &pipelines {
                let expr = match cron_expr {
                    Some(e) => e,
                    None => continue,
                };

                let normalized = normalize_cron(expr);
                let schedule = match cron::Schedule::from_str(&normalized) {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::warn!(
                            "Cron scheduler: invalid expression '{}' for pipeline '{}': {}",
                            expr,
                            pipeline_name,
                            e
                        );
                        continue;
                    }
                };

                if environment.is_empty() {
                    tracing::error!(
                        pipeline = %pipeline_name,
                        "Cron trigger rejected: explicit environment is not configured"
                    );
                    continue;
                }

                // Atomic claim: min-gap + fire-window check + the
                // `cron_last_fired_at` stamp in one IMMEDIATE transaction.
                // The stamp is persisted, so a leadership flip after a crash
                // cannot re-fire the same window the way the old per-process
                // rate limiter allowed. A failed `do_trigger` still consumes
                // the stamp — that prevents tight retry loops; the next
                // matching window retries.
                match claim_cron_fire(&state.pool, pipeline_id, &schedule, check_interval_seconds)
                    .await
                {
                    Ok(true) => {}
                    Ok(false) => continue,
                    Err(e) => {
                        tracing::error!(
                            pipeline = %pipeline_name,
                            error = %e,
                            "Cron scheduler claim failed"
                        );
                        continue;
                    }
                }

                tracing::info!(
                    "Cron scheduler: triggering pipeline '{}' ({}) via cron '{}' for '{}'",
                    pipeline_name,
                    pipeline_id,
                    expr,
                    environment
                );

                match crate::api::do_trigger(
                    &state,
                    pipeline_id,
                    "cron",
                    &std::collections::HashMap::new(),
                    false,
                    false,
                    Some(environment.as_str()),
                    None,
                    None,
                )
                .await
                {
                    Ok(resp) => {
                        let body = resp.0;
                        if let Some(job_id) = body
                            .data
                            .as_ref()
                            .and_then(|d| d.get("job_id"))
                            .and_then(|j| j.as_str())
                        {
                            tracing::info!("Cron scheduler: job {} created", job_id);
                        }
                    }
                    Err(e) => {
                        tracing::error!(
                            "Cron scheduler: failed to trigger '{}': {}",
                            pipeline_name,
                            e
                        );
                    }
                }
            }
        }
    });
}

/// Atomically decide-and-stamp one cron fire opportunity for `pipeline_id`.
///
/// Gap check (min [`MIN_TRIGGER_GAP_SECS`] since `cron_last_fired_at`), the
/// fire-window check (the expression produced a fire point within the last
/// `check_interval_secs`), and the stamp run inside one `BEGIN IMMEDIATE`
/// transaction — the persisted stamp outlives any instance crash. Exposed
/// for testing.
pub async fn claim_cron_fire(
    pool: &SqlitePool,
    pipeline_id: &str,
    schedule: &cron::Schedule,
    check_interval_secs: u64,
) -> anyhow::Result<bool> {
    let now = chrono::Utc::now();
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;

    let last_raw: String =
        sqlx::query_scalar("SELECT cron_last_fired_at FROM pipelines WHERE id = ?")
            .bind(pipeline_id)
            .fetch_optional(&mut *tx)
            .await?
            .unwrap_or_default();
    let last_fired = crate::services::scheduler::parse_sqlite_datetime(&last_raw);
    if let Some(last) = last_fired {
        let gap = (now - last).num_seconds();
        if gap < MIN_TRIGGER_GAP_SECS {
            tx.commit().await?;
            return Ok(false);
        }
    }

    // Fire when the schedule produced a fire point inside the last
    // check-interval window. `Schedule::upcoming` starts strictly AFTER now
    // (cron crate adds +1s to the query origin), so the naive
    // `upcoming().next() <= now` shipped until 2.1.5 never held and cron
    // pipelines silently never fired — `after(&window_start)` is the
    // inclusive-at-edges equivalent of the documented intent.
    let window_start = now - chrono::Duration::seconds(check_interval_secs as i64);
    let should_fire = schedule
        .after(&window_start)
        .next()
        .is_some_and(|fire_in_window| fire_in_window <= now);
    if !should_fire {
        tx.commit().await?;
        return Ok(false);
    }

    sqlx::query("UPDATE pipelines SET cron_last_fired_at = datetime('now') WHERE id = ?")
        .bind(pipeline_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(true)
}

pub async fn fetch_active_cron_pipelines(
    pool: &SqlitePool,
) -> Result<Vec<(String, String, Option<String>, String)>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT p.id, p.name, p.cron_expr, p.cron_environment
         FROM pipelines p
         WHERE p.trigger_type = 'cron'
           AND p.is_active = 1
           AND p.cron_expr IS NOT NULL
           AND p.cron_expr != ''",
    )
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|r| {
            (
                r.get::<String, _>(0),
                r.get::<String, _>(1),
                r.get::<Option<String>, _>(2),
                r.get::<String, _>(3),
            )
        })
        .collect())
}
