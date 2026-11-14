//! End-of-build notification dispatch (Manager-side).
//!
//! Channels are declared in pipeline YAML (`notify:`), frozen into the job
//! snapshot at trigger (expressions kept), and resolved + sent asynchronously
//! when the job reaches a terminal status.

pub mod feishu;
pub mod webhook;

use async_trait::async_trait;

use shared::pipeline_yaml::{notify_should_send, NotifyChannelYaml};
use shared::secret_ref::resolve_secret_slot;

use crate::crypto;
use crate::store::exec::DbPool;

#[allow(unused_imports)] // re-export for unit tests / future callers
pub use feishu::{build_feishu_card, FeishuChannel};
pub use webhook::{build_webhook_payload, WebhookChannel};

/// Job context passed to every notify channel.
#[derive(Debug, Clone)]
pub struct NotifyEvent {
    pub pipeline_name: String,
    /// Pipeline id (available for future pipeline deep links).
    #[allow(dead_code)]
    pub pipeline_id: String,
    pub job_id: String,
    pub status: String,
    pub trigger_by: String,
    pub duration_secs: Option<i64>,
    /// Per-pipeline build number (GHA run_number); None for pre-feature jobs.
    pub run_number: Option<i64>,
    /// Absolute Job Detail URL when `server.public_base_url` is configured.
    pub job_url: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum NotifyError {
    #[error("{0}")]
    Message(String),
    #[error(transparent)]
    Http(#[from] reqwest::Error),
}

// clippy 1.99 (`double_must_use`): async_trait wraps `send` in a boxed future
// it marks `#[must_use]`, on top of the `Result` the signature already carries.
// The duplication lives in the macro expansion, not in our code.
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait NotifyChannel: Send + Sync {
    async fn send(&self, event: &NotifyEvent) -> Result<(), NotifyError>;
}

/// Build `{public_base_url}/jobs/{job_id}` when the base URL is non-empty.
pub fn job_detail_url(public_base_url: &str, job_id: &str) -> Option<String> {
    let base = public_base_url.trim().trim_end_matches('/');
    if base.is_empty() || job_id.trim().is_empty() {
        return None;
    }
    Some(format!("{base}/jobs/{job_id}"))
}

/// Fire-and-forget notify after a non-terminal → terminal transition.
pub fn spawn_job_notify(
    pool: DbPool,
    jwt_secret: String,
    public_base_url: String,
    job_id: String,
    new_status: String,
) {
    tokio::spawn(async move {
        if let Err(e) =
            send_job_notifications(&pool, &jwt_secret, &public_base_url, &job_id, &new_status).await
        {
            tracing::warn!(
                job_id = %job_id,
                status = %new_status,
                error = %e,
                "job notify dispatch failed"
            );
        }
    });
}

async fn send_job_notifications(
    pool: &DbPool,
    jwt_secret: &str,
    public_base_url: &str,
    job_id: &str,
    new_status: &str,
) -> Result<(), NotifyError> {
    let row = crate::store::jobs::notify_job_row(pool, job_id)
        .await
        .map_err(|e| NotifyError::Message(format!("load job for notify: {e}")))?
        .ok_or_else(|| NotifyError::Message("job not found for notify".into()))?;

    let snapshot_raw = row.snapshot;
    let trigger_by = row.trigger_by;
    let pipeline_id = row.pipeline_id;
    let start_time = row.start_time;
    let end_time = row.end_time;
    let pipeline_name = row.pipeline_name.unwrap_or_else(|| "(unknown)".into());

    let snapshot: serde_json::Value =
        serde_json::from_str(&snapshot_raw).unwrap_or(serde_json::json!({}));
    let channels: Vec<NotifyChannelYaml> = match snapshot.get("notify") {
        Some(v) if !v.is_null() => serde_json::from_value(v.clone()).unwrap_or_default(),
        _ => return Ok(()),
    };
    if channels.is_empty() {
        return Ok(());
    }

    let duration_secs = duration_secs_from_sqlite(start_time.as_deref(), end_time.as_deref());
    let run_number = row.run_number;
    let event = NotifyEvent {
        pipeline_name,
        pipeline_id: pipeline_id.clone(),
        job_id: job_id.to_string(),
        status: new_status.to_string(),
        trigger_by,
        duration_secs,
        run_number: (run_number > 0).then_some(run_number),
        job_url: job_detail_url(public_base_url, job_id),
    };

    for (i, ch) in channels.iter().enumerate() {
        if !notify_should_send(ch.on.as_deref(), new_status) {
            continue;
        }
        let Some(secret_name) = resolve_secret_slot(&ch.webhook) else {
            tracing::warn!(
                job_id = %job_id,
                channel = i,
                "notify webhook is not a secret reference — skipped"
            );
            continue;
        };
        let webhook_url =
            match load_team_secret_value(pool, jwt_secret, &pipeline_id, &secret_name).await {
                Ok(Some(url)) => url,
                Ok(None) => {
                    tracing::warn!(
                        job_id = %job_id,
                        secret = %secret_name,
                        "notify webhook secret not found — skipped"
                    );
                    continue;
                }
                Err(e) => {
                    tracing::warn!(
                        job_id = %job_id,
                        secret = %secret_name,
                        error = %e,
                        "notify webhook secret load failed — skipped"
                    );
                    continue;
                }
            };

        let channel_type = ch.channel_type.trim();
        let result = match channel_type {
            "feishu" => {
                let channel = FeishuChannel::from_webhook(webhook_url);
                channel.send(&event).await
            }
            "webhook" => {
                let channel = WebhookChannel::from_url(webhook_url);
                channel.send(&event).await
            }
            other => {
                tracing::warn!(
                    job_id = %job_id,
                    channel_type = %other,
                    "unknown notify type at send time — skipped"
                );
                continue;
            }
        };
        if let Err(e) = result {
            tracing::warn!(
                job_id = %job_id,
                channel = i,
                channel_type = %channel_type,
                error = %e,
                "notify channel send failed"
            );
        }
    }
    Ok(())
}

async fn load_team_secret_value(
    pool: &DbPool,
    jwt_secret: &str,
    pipeline_id: &str,
    secret_name: &str,
) -> Result<Option<String>, NotifyError> {
    let stored = crate::store::pipelines::team_secret_value_of(pool, pipeline_id, secret_name)
        .await
        .map_err(|e| NotifyError::Message(format!("query secret: {e}")))?;

    let Some(stored) = stored else {
        return Ok(None);
    };
    let plain = crypto::decrypt(&stored, jwt_secret)
        .map_err(|e| NotifyError::Message(format!("decrypt secret '{secret_name}': {e}")))?;
    Ok(Some(plain))
}

fn duration_secs_from_sqlite(start: Option<&str>, end: Option<&str>) -> Option<i64> {
    let start = start?;
    let end = end?;
    let start_dt = parse_sqlite_datetime(start)?;
    let end_dt = parse_sqlite_datetime(end)?;
    Some((end_dt - start_dt).num_seconds().max(0))
}

fn parse_sqlite_datetime(raw: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    let s = raw.trim();
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
        return Some(dt.with_timezone(&chrono::Utc));
    }
    // SQLite `datetime('now')` → "YYYY-MM-DD HH:MM:SS"
    chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S")
        .ok()
        .map(|ndt| ndt.and_utc())
}
