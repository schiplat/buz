use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pipeline {
    pub id: Uuid,
    pub name: String,
    pub description: String,
    pub trigger_type: TriggerType,
    pub webhook_path: Option<String>,
    pub cron_expr: Option<String>,
    pub runner_labels: Vec<String>,
    pub timeout_seconds: i32,
    pub source: PipelineSource,
    pub yaml_path: Option<String>,
    pub concurrency_group: Option<String>,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum TriggerType {
    Manual,
    Webhook,
    Cron,
}

impl TriggerType {
    pub fn as_str(&self) -> &'static str {
        match self {
            TriggerType::Manual => "manual",
            TriggerType::Webhook => "webhook",
            TriggerType::Cron => "cron",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum PipelineSource {
    Form,
    Yaml,
}

impl PipelineSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            PipelineSource::Form => "form",
            PipelineSource::Yaml => "yaml",
        }
    }
}
