//! Incoming webhook lookups: the pipeline behind a webhook path and its
//! team-secret signing value.
//!
//! On the dual-backend dispatcher (`store::exec`): the statements are
//! backend-neutral and rows decode by column name.

use crate::store::exec::{self, DbPool};

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
pub async fn find_by_path(pool: &DbPool, webhook_path: &str) -> Result<WebhookPipeline, AppError> {
    let pipeline = exec::fetch_optional(
        pool,
        "SELECT id, name, is_active, input_defs, webhook_secret, team_id, webhook_environment \
         FROM pipelines WHERE trigger_type = 'webhook' AND webhook_path = ?",
        &[webhook_path.into()],
    )
    .await?
    .ok_or_else(|| {
        AppError::NotFound(format!(
            "no webhook pipeline found for path '/{webhook_path}'"
        ))
    })?;

    Ok(WebhookPipeline {
        id: pipeline.text_of("id"),
        name: pipeline.text_of("name"),
        active: pipeline.i64_of("is_active") != 0,
        input_defs_json: pipeline.text_of("input_defs"),
        webhook_secret_name: pipeline.text_of("webhook_secret"),
        team_id: pipeline.text_of("team_id"),
        webhook_environment: pipeline.text_of("webhook_environment"),
    })
}

/// Resolve a team secret's ciphertext by name (`None` = no such secret).
/// Decryption happens at the call site — it needs the JWT secret.
pub async fn team_secret_value(
    pool: &DbPool,
    team_id: &str,
    name: &str,
) -> Result<Option<String>, AppError> {
    Ok(exec::fetch_optional(
        pool,
        "SELECT value FROM secrets WHERE team_id = ? AND name = ?",
        &[team_id.into(), name.into()],
    )
    .await?
    .map(|r| r.text_of("value")))
}
