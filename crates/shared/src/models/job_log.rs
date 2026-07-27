use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobLog {
    pub id: Uuid,
    pub job_id: Uuid,
    pub chunk_index: i32,
    pub step_order: i32,
    pub log_content: String,
    pub is_masked: bool,
    pub created_at: DateTime<Utc>,
}
