use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Runner {
    pub id: Uuid,
    pub name: String,
    #[serde(skip_serializing)]
    pub register_token: String,
    #[serde(skip_serializing)]
    pub token: Option<String>,
    pub labels: Vec<String>,
    pub status: RunnerStatus,
    pub version: Option<String>,
    pub last_seen: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum RunnerStatus {
    Online,
    Offline,
    Busy,
}

impl RunnerStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            RunnerStatus::Online => "online",
            RunnerStatus::Offline => "offline",
            RunnerStatus::Busy => "busy",
        }
    }
}
