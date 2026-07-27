use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Variable {
    pub id: Uuid,
    pub pipeline_id: Uuid,
    pub var_name: String,
    pub source_type: SecretSourceType,
    pub vault_path: Option<String>,
    pub is_secret: bool,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum SecretSourceType {
    Vault,
    Env,
    Manual,
}

impl SecretSourceType {
    pub fn as_str(&self) -> &'static str {
        match self {
            SecretSourceType::Vault => "vault",
            SecretSourceType::Env => "env",
            SecretSourceType::Manual => "manual",
        }
    }
}
