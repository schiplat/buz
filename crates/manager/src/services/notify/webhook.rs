//! Generic webhook notification channel.
//!
//! POSTs a flat JSON event envelope to the secret-resolved URL and treats
//! any 2xx as delivered (no vendor response parsing). The URL is an
//! operator-managed team secret (`${secrets.NAME}`) — the same trust
//! posture as the Feishu channel: whoever can edit team secrets chooses
//! where notifications go.

use async_trait::async_trait;
use chrono::Utc;

use super::{NotifyChannel, NotifyError, NotifyEvent};

pub struct WebhookChannel {
    url: String,
    client: reqwest::Client,
}

impl WebhookChannel {
    pub fn from_url(url: String) -> Self {
        Self {
            url,
            client: crate::http_util::client_builder()
                .build()
                .expect("reqwest client build cannot fail"),
        }
    }
}

#[async_trait]
impl NotifyChannel for WebhookChannel {
    async fn send(&self, event: &NotifyEvent) -> Result<(), NotifyError> {
        let resp = self
            .client
            .post(&self.url)
            .json(&build_webhook_payload(event))
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            let body = if body.len() > 300 {
                format!("{}…", &body[..300])
            } else {
                body
            };
            return Err(NotifyError::Message(format!(
                "webhook receiver HTTP {status}: {body}"
            )));
        }
        Ok(())
    }
}

/// Flat, self-describing event envelope — no vendor format, one JSON object
/// with stable keys so generic receivers can route on `event` + `status`.
pub fn build_webhook_payload(event: &NotifyEvent) -> serde_json::Value {
    serde_json::json!({
        "event": "job.completed",
        "pipeline": event.pipeline_name,
        "pipeline_id": event.pipeline_id,
        "job_id": event.job_id,
        "job_url": event.job_url,
        "status": event.status,
        "triggered_by": event.trigger_by,
        "run_number": event.run_number,
        "duration_seconds": event.duration_secs,
        "timestamp": Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    })
}
