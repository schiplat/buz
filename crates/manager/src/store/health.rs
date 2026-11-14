//! Health-probe samples: database ping and non-terminal job backlog.
//!
//! First store module on the dual-backend dispatcher (`store::exec`): both
//! functions speak backend-neutral statements and receive `&DbPool`, so the
//! same probes serve SQLite and Postgres.

use std::collections::HashMap;

use crate::store::exec::{self, DbPool};

/// `SELECT 1` liveness probe. Returns the round-trip latency in ms, or the
/// error for the readiness verdict.
pub async fn db_probe(pool: &DbPool) -> Result<f64, sqlx::Error> {
    let start = std::time::Instant::now();
    exec::fetch_scalar_i64(pool, "SELECT 1", &[]).await?;
    Ok(start.elapsed().as_secs_f64() * 1000.0)
}

/// Snapshot of non-terminal job counts — an early warning for scheduling
/// backpressure or a stuck queue. Best-effort: query errors yield zeros.
pub async fn jobs_backlog(pool: &DbPool) -> serde_json::Value {
    let rows = exec::fetch_all(
        pool,
        "SELECT status, COUNT(*) AS cnt FROM jobs
         WHERE status IN ('pending', 'running', 'queued', 'waiting_approval')
         GROUP BY status",
        &[],
    )
    .await;

    let counts: HashMap<String, i64> = match rows {
        Ok(rows) => rows.into_iter().map(|r| (r.text(0), r.i64(1))).collect(),
        Err(e) => {
            tracing::warn!(error = %e, "health: failed to sample job backlog");
            HashMap::new()
        }
    };
    serde_json::json!({
        "pending": counts.get("pending").copied().unwrap_or(0),
        "running": counts.get("running").copied().unwrap_or(0),
        "queued": counts.get("queued").copied().unwrap_or(0),
        "waiting_approval": counts.get("waiting_approval").copied().unwrap_or(0),
    })
}
