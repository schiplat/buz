pub mod response;

use serde::{Deserialize, Serialize};

/// A field that accepts either a single value or an array.
/// YAML: `host: prod-01` or `host: [prod-01, prod-02]`
/// JSON: `"prod-01"` or `["prod-01", "prod-02"]`
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum OneOrMany {
    One(String),
    Many(Vec<String>),
}

impl OneOrMany {
    pub fn to_vec(&self) -> Vec<String> {
        match self {
            OneOrMany::One(s) => vec![s.clone()],
            OneOrMany::Many(v) => v.clone(),
        }
    }

    pub fn join(&self, sep: &str) -> String {
        self.to_vec().join(sep)
    }

    pub fn is_empty(&self) -> bool {
        match self {
            OneOrMany::One(s) => s.is_empty(),
            OneOrMany::Many(v) => v.is_empty(),
        }
    }
}

// --- Auth ---

#[derive(Debug, Serialize, Deserialize)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LoginResponse {
    pub token: String,
}

// --- Runner ---

#[derive(Debug, Serialize, Deserialize)]
pub struct RegisterRequest {
    pub name: String,
    pub register_token: String,
    pub labels: Vec<String>,
    pub version: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RegisterResponse {
    pub runner_id: String,
    pub token: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct HeartbeatRequest {
    pub status: String,
    #[serde(default)]
    pub system_info: Option<SystemInfo>,
}

/// Hardware / OS information collected by the Runner and reported to the Center.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemInfo {
    pub runner_version: String,
    pub hostname: String,
    pub os: String,
    pub kernel_version: String,
    pub cpu_arch: String,
    pub cpu_cores: usize,
    pub cpu_model: String,
    pub cpu_usage_percent: f32,
    pub memory_total_mb: u64,
    pub memory_used_mb: u64,
    pub swap_total_mb: u64,
    pub swap_used_mb: u64,
    pub disk_total_gb: f64,
    pub disk_used_gb: f64,
    pub network_interfaces: Vec<NetworkInterfaceInfo>,
    pub ip_addresses: Vec<String>,
    pub uptime_seconds: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkInterfaceInfo {
    pub name: String,
    pub mac: String,
    pub ips: Vec<String>,
}

// --- Runner Management ---

#[derive(Debug, Serialize, Deserialize)]
pub struct CreateRunnerRequest {
    pub name: String,
    pub labels: Option<Vec<String>>,
    pub version: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CreateRunnerResponse {
    pub id: String,
    pub name: String,
    pub register_token: String,
}

// --- Job ---

#[derive(Debug, Serialize, Deserialize)]
pub struct ClaimJobRequest {
    pub runner_id: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UpdateJobStatusRequest {
    pub status: String,
    pub step_order: Option<i32>,
}

// --- Log ---

#[derive(Debug, Serialize, Deserialize)]
pub struct LogChunk {
    pub chunk_index: i32,
    pub step_order: i32,
    pub content: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CreatePipelineRequest {
    pub name: String,
    pub description: Option<String>,
    pub trigger_type: String,
    pub webhook_path: Option<String>,
    pub cron_expr: Option<String>,
    pub runner_labels: Vec<String>,
    pub timeout_seconds: Option<i32>,
    pub concurrency_group: Option<String>,
    pub env: Option<Vec<EnvPair>>,
    pub secrets: Option<Vec<String>>,
    pub steps: Vec<StepDef>,
    /// Input parameter definitions for manual triggers.
    /// Each entry maps param name → (description, required?, default).
    #[serde(default)]
    pub input_defs: Option<Vec<PipelineInputDef>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineInputDef {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub required: Option<bool>,
    #[serde(default)]
    pub default: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TriggerPipelineRequest {
    /// Optional key-value inputs (e.g. version, branch, environment).
    /// These are stored in the job snapshot and exported as INPUT_* env vars on the Runner.
    #[serde(default)]
    pub inputs: std::collections::HashMap<String, String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct EnvPair {
    pub name: String,
    pub value: String,
}

// --- Host ---

#[derive(Debug, Serialize, Deserialize)]
pub struct CreateHostRequest {
    pub name: String,
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    pub username: String,
    #[serde(default)]
    pub credential_type: Option<String>,
    pub credential_ref: String,
    #[serde(default)]
    pub labels: Option<Vec<String>>,
}

fn default_port() -> u16 {
    22
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UpdateHostRequest {
    pub name: Option<String>,
    pub host: Option<String>,
    pub port: Option<u16>,
    pub username: Option<String>,
    pub credential_type: Option<String>,
    pub credential_ref: Option<String>,
    pub labels: Option<Vec<String>>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct StepDef {
    pub name: String,
    pub run: String,
    pub uses: Option<String>,
    /// Host name(s) — single string or array. Only relevant for SSH steps.
    #[serde(default)]
    pub host: Option<OneOrMany>,
    /// Host labels to match — single string or array. SSH steps will run on all hosts matching any of these labels.
    #[serde(default, alias = "host_labels")]
    pub host_labels: Option<OneOrMany>,
    pub dir: Option<String>,
    pub timeout: Option<String>,
    pub retry: Option<i32>,
    #[serde(rename = "continue-on-error", alias = "continue_on_error")]
    pub continue_on_error: Option<bool>,
}

// --- Secrets ---

#[derive(Debug, Serialize, Deserialize)]
pub struct CreateSecretRequest {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UpdateSecretRequest {
    pub value: String,
}

// --- Variables ---

#[derive(Debug, Serialize, Deserialize)]
pub struct CreateVariableRequest {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UpdateVariableRequest {
    pub value: String,
}
