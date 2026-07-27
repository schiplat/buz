use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: Uuid,
    pub pipeline_id: Uuid,
    pub step_order: i32,
    pub name: String,
    pub exec_type: ExecType,
    pub host_id: Option<Uuid>,
    pub command_template: String,
    pub work_dir: String,
    pub retry_count: i32,
    pub timeout_seconds: i32,
    pub fail_strategy: FailStrategy,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ExecType {
    Local,
    Ssh,
    Docker,
    Wasm,
}

impl ExecType {
    pub fn as_str(&self) -> &'static str {
        match self {
            ExecType::Local => "local",
            ExecType::Ssh => "ssh",
            ExecType::Docker => "docker",
            ExecType::Wasm => "wasm",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum FailStrategy {
    Stop,
    Continue,
    Ignore,
}

impl FailStrategy {
    pub fn as_str(&self) -> &'static str {
        match self {
            FailStrategy::Stop => "stop",
            FailStrategy::Continue => "continue",
            FailStrategy::Ignore => "ignore",
        }
    }
}
