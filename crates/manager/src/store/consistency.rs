//! Read-only team-binding consistency report.
//!
//! The write-path guards (secret moves, team deletion) prevent new rows from
//! drifting, but data created before those guards — or by direct DB edits —
//! can still be invisible at runtime:
//!
//! - pipelines/secrets with an empty `team_id` (pre-team legacy rows),
//! - pipelines/secrets/hosts whose `team_id` no longer names a team,
//! - `pipeline_secrets` rows whose pipeline and secret sit in different teams
//!   (the runtime's same-team join silently ignores them).
//!
//! Nothing here writes; callers decide whether to log or print the report.

use serde::Serialize;

use crate::store::exec::{self, DbPool};
use shared::error::AppError;

#[derive(Debug, Default, Clone, Serialize)]
pub struct ConsistencyReport {
    pub pipelines_missing_team: i64,
    pub secrets_missing_team: i64,
    pub pipelines_unknown_team: i64,
    pub secrets_unknown_team: i64,
    pub hosts_unknown_team: i64,
    pub cross_team_secret_bindings: i64,
}

impl ConsistencyReport {
    pub fn is_clean(&self) -> bool {
        self.pipelines_missing_team == 0
            && self.secrets_missing_team == 0
            && self.pipelines_unknown_team == 0
            && self.secrets_unknown_team == 0
            && self.hosts_unknown_team == 0
            && self.cross_team_secret_bindings == 0
    }
}

async fn count(pool: &DbPool, sql: &str) -> Result<i64, AppError> {
    Ok(exec::fetch_scalar_i64(pool, sql, &[]).await?)
}

/// Scan the three binding surfaces. Six counts, one read per count; the tables
/// are small and this is an explicit operator action (CLI) plus a one-shot
/// startup probe, never a per-request path.
pub async fn check(pool: &DbPool) -> Result<ConsistencyReport, AppError> {
    Ok(ConsistencyReport {
        pipelines_missing_team: count(
            pool,
            "SELECT COUNT(*) FROM pipelines WHERE team_id IS NULL OR team_id = ''",
        )
        .await?,
        secrets_missing_team: count(
            pool,
            "SELECT COUNT(*) FROM secrets WHERE team_id IS NULL OR team_id = ''",
        )
        .await?,
        pipelines_unknown_team: count(
            pool,
            "SELECT COUNT(*) FROM pipelines
             WHERE team_id IS NOT NULL AND team_id != ''
               AND NOT EXISTS (SELECT 1 FROM teams t WHERE t.id = pipelines.team_id)",
        )
        .await?,
        secrets_unknown_team: count(
            pool,
            "SELECT COUNT(*) FROM secrets
             WHERE team_id IS NOT NULL AND team_id != ''
               AND NOT EXISTS (SELECT 1 FROM teams t WHERE t.id = secrets.team_id)",
        )
        .await?,
        hosts_unknown_team: count(
            pool,
            "SELECT COUNT(*) FROM hosts
             WHERE team_id IS NOT NULL AND team_id != ''
               AND NOT EXISTS (SELECT 1 FROM teams t WHERE t.id = hosts.team_id)",
        )
        .await?,
        cross_team_secret_bindings: count(
            pool,
            "SELECT COUNT(*) FROM pipeline_secrets ps
             JOIN pipelines p ON p.id = ps.pipeline_id
             JOIN secrets s ON s.id = ps.secret_id
             WHERE p.team_id != '' AND s.team_id != '' AND p.team_id != s.team_id",
        )
        .await?,
    })
}
