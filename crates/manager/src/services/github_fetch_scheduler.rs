//! Auto-fetch scheduler for GitHub connections.
//!
//! Every `check_interval_seconds`, the current leader scans `github_connections`
//! whose `fetch_interval_minutes` is set and whose `last_fetch_at` is past due,
//! then re-pulls each configured `fetch_owners` repo list. Leader election uses
//! a dedicated row of the shared `scheduler_lease` table so the job-dispatch
//! scheduler and this scheduler never kick each other out.
//!
//! Manual fetches via `POST /github-sync/repos/fetch` also update
//! `last_fetch_at`, so a manual sync suppresses the next auto run.

use std::time::Duration;
use tracing;
use uuid::Uuid;

use crate::crypto;
use crate::state::AppState;

use super::github_actions_sync::{fetch_and_upsert_repos, GitHubClient};
use super::scheduler::try_acquire_lease;

/// Lease row owned by the GitHub auto-fetch scheduler.
pub(crate) const FETCH_LEASE_ID: i64 = 2;

/// Start the GitHub auto-fetch scheduler background task.
pub fn spawn_fetch_scheduler(state: AppState, check_interval_seconds: u64) {
    let instance_id = Uuid::new_v4().to_string();
    tokio::spawn(async move {
        tracing::info!(
            "GitHub fetch scheduler started (check interval: {}s, lease id: {})",
            check_interval_seconds,
            FETCH_LEASE_ID
        );

        loop {
            match try_acquire_lease(&state.pool, FETCH_LEASE_ID, &instance_id).await {
                Ok(true) => {
                    if let Err(e) = run_due_fetches(&state).await {
                        tracing::error!("GitHub fetch scheduler cycle failed: {e}");
                    }
                }
                Ok(false) => {
                    // Another instance is the leader — wait and retry.
                }
                Err(e) => {
                    tracing::error!("GitHub fetch scheduler lease error: {e}");
                }
            }

            tokio::time::sleep(Duration::from_secs(check_interval_seconds)).await;
        }
    });
}

struct DueConnection {
    id: String,
    name: String,
    api_base_url: String,
    token_ciphertext: String,
    owners: Vec<String>,
}

/// True when `last_fetch_at` (SQLite UTC `YYYY-MM-DD HH:MM:SS`) is older than
/// `interval_minutes`. Null or unparseable timestamps are treated as due so a
/// broken record re-fetches instead of stalling.
pub fn connection_is_due(last_fetch_at: Option<&str>, interval_minutes: i64) -> bool {
    let Some(last) = last_fetch_at else {
        return true;
    };
    let Ok(last) = chrono::NaiveDateTime::parse_from_str(last, "%Y-%m-%d %H:%M:%S") else {
        return true;
    };
    let last = last.and_utc();
    chrono::Utc::now().signed_duration_since(last) >= chrono::Duration::minutes(interval_minutes)
}

/// Fetch every connection whose `last_fetch_at` is past its configured interval.
async fn run_due_fetches(state: &AppState) -> anyhow::Result<()> {
    use sqlx::Row;

    let rows = sqlx::query(
        "SELECT id, name, api_base_url, token_ciphertext, fetch_owners, fetch_interval_minutes, last_fetch_at
         FROM github_connections
         WHERE fetch_interval_minutes IS NOT NULL AND fetch_interval_minutes > 0
           AND fetch_owners != ''",
    )
    .fetch_all(&state.pool)
    .await?;

    let mut due: Vec<DueConnection> = Vec::new();
    for row in rows {
        let interval: i64 = row.get(5);
        let last_fetch_at: Option<String> = row.get(6);
        if !connection_is_due(last_fetch_at.as_deref(), interval) {
            continue;
        }
        due.push(DueConnection {
            id: row.get(0),
            name: row.get(1),
            api_base_url: row.get(2),
            token_ciphertext: row.get(3),
            owners: row
                .get::<String, _>(4)
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect(),
        });
    }

    if !due.is_empty() {
        tracing::info!(
            due = due.len(),
            "github-sync: auto fetch cycle due connections"
        );
    }

    for conn in due {
        let token = match crypto::decrypt(&conn.token_ciphertext, &state.config.auth.jwt_secret) {
            Ok(t) => t,
            Err(e) => {
                tracing::error!(
                    connection = %conn.name,
                    "github-sync: auto fetch skipped, token decrypt failed: {e}"
                );
                continue;
            }
        };
        let client = GitHubClient::new(&conn.api_base_url, &token);

        tracing::info!(
            connection = %conn.name,
            connection_id = %conn.id,
            owners = ?conn.owners,
            "github-sync: auto fetch due"
        );

        for owner in &conn.owners {
            match fetch_and_upsert_repos(&state.pool, &conn.id, &conn.name, owner, &client).await {
                Ok((added, existing)) => {
                    tracing::info!(
                        connection = %conn.name,
                        owner = %owner,
                        added,
                        existing,
                        "github-sync: auto fetch owner complete"
                    );
                }
                Err(e) => {
                    tracing::error!(
                        connection = %conn.name,
                        owner = %owner,
                        "github-sync: auto fetch owner failed: {e}"
                    );
                }
            }
        }

        // Stamp the fetch time even when an owner failed, so a permanently
        // failing owner does not retry on every scheduler tick.
        sqlx::query(
            "UPDATE github_connections SET last_fetch_at = datetime('now'), updated_at = datetime('now') WHERE id = ?",
        )
        .bind(&conn.id)
        .execute(&state.pool)
        .await?;
        tracing::info!(connection = %conn.name, "github-sync: auto fetch cycle done");
    }

    Ok(())
}
