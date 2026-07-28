use sqlx::{SqlitePool, Row};
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Mutex;
use std::time::Duration;
use tokio::time;

use crate::state::AppState;

/// Minimum interval between cron triggers for the same pipeline (seconds).
/// Prevents double-firing when the scheduler ticks more often than the
/// cron expression granularity.
const MIN_TRIGGER_GAP_SECS: i64 = 120;

/// Normalize a cron expression to 7-field form accepted by the `cron` 0.15 crate.
/// Accepts both 5-field (min hr dom mon dow) and 7-field formats.
fn normalize_cron(expr: &str) -> String {
    let fields: Vec<&str> = expr.split_whitespace().collect();
    match fields.len() {
        5 => format!("0 {} *", expr),
        6 => format!("{} *", expr), // sec + 5 fields → add year wildcard
        7 => expr.to_string(),
        _ => {
            // Unrecognised format — best-effort
            tracing::warn!("Unrecognised cron expression ({} fields): '{}'", fields.len(), expr);
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

        let last_triggers: Mutex<HashMap<String, chrono::DateTime<chrono::Utc>>> =
            Mutex::new(HashMap::new());

        let mut interval = time::interval(Duration::from_secs(check_interval_seconds));

        loop {
            interval.tick().await;

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

            let now = chrono::Utc::now();

            for (pipeline_id, pipeline_name, cron_expr) in &pipelines {
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
                            expr, pipeline_name, e
                        );
                        continue;
                    }
                };

                // Check if the cron expression would have fired within the last
                // check-interval window.
                let window_start = now - chrono::Duration::seconds(check_interval_seconds as i64);
                let mut upcoming = schedule.upcoming(chrono::Utc);
                let should_fire = upcoming.next().map_or(false, |next_fire| {
                    next_fire <= now && next_fire > window_start
                });

                if !should_fire {
                    continue;
                }

                // Rate-limit: check when this pipeline was last triggered by cron
                {
                    let map = last_triggers.lock().unwrap();
                    if let Some(last) = map.get(pipeline_id) {
                        let gap = (now - *last).num_seconds();
                        if gap < MIN_TRIGGER_GAP_SECS {
                            tracing::debug!(
                                "Cron scheduler: skipping '{}' (triggered {}s ago)",
                                pipeline_name, gap
                            );
                            continue;
                        }
                    }
                }

                tracing::info!(
                    "Cron scheduler: triggering pipeline '{}' ({}) via cron '{}'",
                    pipeline_name, pipeline_id, expr
                );

                match crate::api::do_trigger(
                    &state,
                    pipeline_id,
                    "cron",
                    &HashMap::new(),
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
                        // Record trigger time
                        let mut map = last_triggers.lock().unwrap();
                        map.insert(pipeline_id.clone(), now);
                    }
                    Err(e) => {
                        tracing::error!(
                            "Cron scheduler: failed to trigger '{}': {}",
                            pipeline_name, e
                        );
                    }
                }
            }
        }
    });
}

async fn fetch_active_cron_pipelines(
    pool: &SqlitePool,
) -> Result<Vec<(String, String, Option<String>)>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT p.id, p.name, p.cron_expr
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
            )
        })
        .collect())
}

#[cfg(test)]
#[path = "tests/cron_scheduler_tests.rs"]
mod tests;
