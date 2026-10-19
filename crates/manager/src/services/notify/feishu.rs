//! Feishu (Lark) custom bot webhook channel.

use async_trait::async_trait;

use super::{NotifyChannel, NotifyError, NotifyEvent};

/// Feishu custom-robot webhook sender.
pub struct FeishuChannel {
    webhook_url: String,
    client: reqwest::Client,
}

impl FeishuChannel {
    pub fn from_webhook(webhook_url: String) -> Self {
        Self {
            webhook_url,
            client: reqwest::Client::new(),
        }
    }
}

#[async_trait]
impl NotifyChannel for FeishuChannel {
    async fn send(&self, event: &NotifyEvent) -> Result<(), NotifyError> {
        let body = build_feishu_card(event);
        let resp = self
            .client
            .post(&self.webhook_url)
            .json(&body)
            .send()
            .await?;
        let status = resp.status();
        let body_text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(NotifyError::Message(format!(
                "feishu webhook HTTP {status}: {body_text}"
            )));
        }
        // Feishu may return 200 with {"code": nonzero, ...}
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&body_text) {
            if let Some(code) = v.get("code").and_then(|c| c.as_i64()) {
                if code != 0 {
                    let msg = v.get("msg").and_then(|m| m.as_str()).unwrap_or("unknown");
                    return Err(NotifyError::Message(format!(
                        "feishu webhook code={code}: {msg}"
                    )));
                }
            }
        }
        Ok(())
    }
}

/// Build an interactive card payload for Feishu custom robots.
pub fn build_feishu_card(event: &NotifyEvent) -> serde_json::Value {
    let (title, template) = match event.status.as_str() {
        "success" => ("Pipeline succeeded", "green"),
        "failed" | "timeout" => ("Pipeline failed", "red"),
        "cancelled" => ("Pipeline cancelled", "orange"),
        other => (other, "blue"),
    };

    let duration = match event.duration_secs {
        Some(s) if s >= 3600 => format!("{}h {}m", s / 3600, (s % 3600) / 60),
        Some(s) if s >= 60 => format!("{}m {}s", s / 60, s % 60),
        Some(s) => format!("{s}s"),
        None => "—".into(),
    };

    let mut elements = vec![serde_json::json!({
        "tag": "div",
        "fields": [
            {
                "is_short": true,
                "text": {
                    "tag": "lark_md",
                    "content": format!("**Pipeline**\n{}", event.pipeline_name)
                }
            },
            {
                "is_short": true,
                "text": {
                    "tag": "lark_md",
                    "content": format!("**Status**\n{}", event.status)
                }
            },
            {
                "is_short": true,
                "text": {
                    "tag": "lark_md",
                    "content": format!("**Job**\n{}", event.job_id)
                }
            },
            {
                "is_short": true,
                "text": {
                    "tag": "lark_md",
                    "content": event.run_number.map_or_else(
                        || "**Build**\n—".to_string(),
                        |n| format!("**Build**\n#{n}"),
                    )
                }
            },
            {
                "is_short": true,
                "text": {
                    "tag": "lark_md",
                    "content": format!("**Duration**\n{duration}")
                }
            },
            {
                "is_short": true,
                "text": {
                    "tag": "lark_md",
                    "content": format!("**Triggered by**\n{}", event.trigger_by)
                }
            }
        ]
    })];

    if let Some(url) = event.job_url.as_deref().filter(|u| !u.is_empty()) {
        elements.push(serde_json::json!({
            "tag": "action",
            "actions": [{
                "tag": "button",
                "text": { "tag": "plain_text", "content": "View Job" },
                "type": "primary",
                "url": url
            }]
        }));
    }

    serde_json::json!({
        "msg_type": "interactive",
        "card": {
            "header": {
                "title": { "tag": "plain_text", "content": title },
                "template": template
            },
            "elements": elements
        }
    })
}
