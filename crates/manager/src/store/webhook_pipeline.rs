//! Incoming webhook lookups: the pipeline behind a webhook path and its
//! team-secret signing value.

use sqlx::{Row, SqlitePool};

use shared::error::AppError;

/// A webhook-backed pipeline, as needed to verify and dispatch a delivery.
#[derive(Debug, Clone)]
pub struct WebhookPipeline {
    pub id: String,
    pub name: String,
    pub active: bool,
    pub input_defs_json: String,
    pub webhook_secret_name: String,
    pub team_id: String,
    pub webhook_environment: String,
}

/// Find the pipeline bound to `webhook_path` (case-sensitive exact match).
pub async fn find_by_path(
    pool: &SqlitePool,
    webhook_path: &str,
) -> Result<WebhookPipeline, AppError> {
    let pipeline = sqlx::query(
        "SELECT id, name, is_active, input_defs, webhook_secret, team_id, webhook_environment \
         FROM pipelines WHERE trigger_type = 'webhook' AND webhook_path = ?",
    )
    .bind(webhook_path)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| {
        AppError::NotFound(format!(
            "no webhook pipeline found for path '/{webhook_path}'"
        ))
    })?;

    Ok(WebhookPipeline {
        id: pipeline.get(0),
        name: pipeline.get(1),
        active: pipeline.get::<i64, _>(2) != 0,
        input_defs_json: pipeline.get(3),
        webhook_secret_name: pipeline.get(4),
        team_id: pipeline.get(5),
        webhook_environment: pipeline.get(6),
    })
}

/// Resolve a team secret's ciphertext by name (`None` = no such secret).
/// Decryption happens at the call site — it needs the JWT secret.
pub async fn team_secret_value(
    pool: &SqlitePool,
    team_id: &str,
    name: &str,
) -> Result<Option<String>, AppError> {
    Ok(
        sqlx::query("SELECT value FROM secrets WHERE team_id = ? AND name = ?")
            .bind(team_id)
            .bind(name)
            .fetch_optional(pool)
            .await?
            .map(|r| r.get(0)),
    )
}
