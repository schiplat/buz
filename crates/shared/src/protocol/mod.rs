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
    #[serde(default)]
    pub user_id: Option<String>,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub is_platform_admin: Option<bool>,
}

/// First-boot admin creation (only when `users` table is empty).
#[derive(Debug, Serialize, Deserialize)]
pub struct SetupAdminRequest {
    pub username: String,
    pub password: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SetupStatusResponse {
    pub needs_setup: bool,
}

/// Authenticated user changes their own password.
#[derive(Debug, Serialize, Deserialize)]
pub struct ChangePasswordRequest {
    pub current_password: String,
    pub new_password: String,
}

// --- Users / Teams (Phase 2 RBAC) ---

#[derive(Debug, Serialize, Deserialize)]
pub struct CreateUserRequest {
    pub username: String,
    pub password: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub is_platform_admin: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UpdateUserRequest {
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub is_platform_admin: Option<bool>,
    #[serde(default)]
    pub is_active: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CreateTeamRequest {
    pub name: String,
    pub slug: String,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UpdateTeamRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UpsertTeamMemberRequest {
    pub user_id: String,
    /// owner | maintainer | developer | viewer
    pub role: String,
}

// --- Runner ---

#[derive(Debug, Serialize, Deserialize)]
pub struct RegisterRequest {
    pub name: String,
    pub register_token: String,
    /// Deprecated: labels are UI-managed. Ignored by Manager on register.
    #[serde(default)]
    pub labels: Vec<String>,
    pub version: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RegisterResponse {
    pub runner_id: String,
    pub token: String,
    /// UI-managed policy snapshot (concurrency, engines, cleanup).
    #[serde(default)]
    pub policy: RunnerPolicyConfig,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct HeartbeatRequest {
    pub status: String,
    #[serde(default)]
    pub system_info: Option<SystemInfo>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct HeartbeatResponse {
    /// UI-managed policy snapshot.
    #[serde(default)]
    pub policy: RunnerPolicyConfig,
}

/// Runner strategy settings controlled from Manager UI (not machine-local paths).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunnerPolicyConfig {
    #[serde(default = "default_max_concurrent_jobs")]
    pub max_concurrent_jobs: u32,
    #[serde(default)]
    pub clean_workspace_after_job: bool,
    #[serde(default = "default_true")]
    pub shell_enabled: bool,
    #[serde(default = "default_true")]
    pub ssh_enabled: bool,
    #[serde(default)]
    pub docker_enabled: bool,
    #[serde(default)]
    pub wasm_enabled: bool,
    /// `insecure` | `known_hosts`
    #[serde(default = "default_ssh_host_key_policy")]
    pub ssh_host_key_policy: String,
}

impl Default for RunnerPolicyConfig {
    fn default() -> Self {
        Self {
            max_concurrent_jobs: 1,
            clean_workspace_after_job: false,
            shell_enabled: true,
            ssh_enabled: true,
            docker_enabled: false,
            wasm_enabled: false,
            ssh_host_key_policy: default_ssh_host_key_policy(),
        }
    }
}

fn default_max_concurrent_jobs() -> u32 {
    1
}

fn default_true() -> bool {
    true
}

fn default_ssh_host_key_policy() -> String {
    "insecure".into()
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
    /// Max parallel jobs (1..=32). Defaults to 1 when omitted.
    #[serde(default)]
    pub max_concurrent_jobs: Option<u32>,
    #[serde(default)]
    pub clean_workspace_after_job: Option<bool>,
    #[serde(default)]
    pub shell_enabled: Option<bool>,
    #[serde(default)]
    pub ssh_enabled: Option<bool>,
    #[serde(default)]
    pub docker_enabled: Option<bool>,
    #[serde(default)]
    pub wasm_enabled: Option<bool>,
    /// `insecure` | `known_hosts`
    #[serde(default)]
    pub ssh_host_key_policy: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CreateRunnerResponse {
    pub id: String,
    pub name: String,
    pub register_token: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UpdateRunnerRequest {
    pub labels: Option<Vec<String>>,
    pub max_concurrent_jobs: Option<u32>,
    pub clean_workspace_after_job: Option<bool>,
    pub shell_enabled: Option<bool>,
    pub ssh_enabled: Option<bool>,
    pub docker_enabled: Option<bool>,
    pub wasm_enabled: Option<bool>,
    pub ssh_host_key_policy: Option<String>,
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
    /// Concurrency mode: queue | discard-old | discard-new. Default is discard-old.
    #[serde(default)]
    pub concurrency_mode: Option<String>,
    /// Max queued jobs (only for mode=queue). Default 3.
    #[serde(default)]
    pub concurrency_max_queue: Option<i32>,
    /// Queue timeout seconds. Default 1800 (30 minutes).
    #[serde(default)]
    pub concurrency_queue_timeout_seconds: Option<i32>,
    #[serde(default)]
    pub owner_group: Option<String>,
    /// Team that owns this pipeline (required in Phase 2).
    #[serde(default)]
    pub team_id: Option<String>,
    /// Explicitly authorized secret IDs from the secrets table.
    #[serde(default)]
    pub secret_ids: Option<Vec<String>>,
    /// Explicitly bound variable IDs from the platform_vars table.
    #[serde(default)]
    pub var_ids: Option<Vec<String>>,
    pub env: Option<Vec<EnvPair>>,
    pub steps: Vec<StepDef>,
    /// Input parameter definitions for manual triggers.
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
    /// When true, values are redacted in logs, audit, and job snapshots (still injected as env at runtime).
    #[serde(default)]
    pub masking: Option<bool>,
}

impl PipelineInputDef {
    pub fn is_masking(&self) -> bool {
        self.masking.unwrap_or(false)
    }
}

/// Names of input params marked `masking: true`.
pub fn masking_input_names(defs: &[PipelineInputDef]) -> Vec<String> {
    defs.iter()
        .filter(|d| d.is_masking())
        .map(|d| d.name.clone())
        .collect()
}

/// Copy `inputs`, replacing values whose keys are in `masking_keys` with `***`.
/// Keys present in `inputs` but absent from any definition list are left as-is unless
/// listed in `masking_keys` (callers may also treat unknown keys as masked).
pub fn redact_input_map(
    inputs: &std::collections::HashMap<String, String>,
    masking_keys: &[String],
) -> std::collections::HashMap<String, String> {
    let set: std::collections::HashSet<&str> = masking_keys.iter().map(String::as_str).collect();
    inputs
        .iter()
        .map(|(k, v)| {
            if set.contains(k.as_str()) {
                (k.clone(), "***".to_string())
            } else {
                (k.clone(), v.clone())
            }
        })
        .collect()
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
    /// Private key content or password — stored encrypted in DB and sent to runner at dispatch time.
    #[serde(default)]
    pub credential_data: Option<String>,
    /// Optional passphrase for encrypted private keys — stored encrypted in DB.
    #[serde(default)]
    pub credential_passphrase: Option<String>,
    #[serde(default)]
    pub labels: Option<Vec<String>>,
    /// Optional team scope. Null/omitted = shared (platform-admin managed).
    #[serde(default)]
    pub team_id: Option<String>,
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
    pub credential_data: Option<String>,
    pub credential_passphrase: Option<String>,
    pub labels: Option<Vec<String>>,
    /// Set to empty string to clear team (shared host).
    pub team_id: Option<String>,
}

/// Probe SSH connectivity using form values and/or a stored host credential.
#[derive(Debug, Serialize, Deserialize)]
pub struct TestHostRequest {
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    pub username: String,
    #[serde(default)]
    pub credential_type: Option<String>,
    /// Private key or password. When empty and `host_id` is set, stored ciphertext is used.
    #[serde(default)]
    pub credential_data: Option<String>,
    #[serde(default)]
    pub credential_passphrase: Option<String>,
    /// Optional existing host — decrypt stored credentials when form fields are blank.
    #[serde(default)]
    pub host_id: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TestHostResponse {
    pub ok: bool,
    pub latency_ms: u64,
    pub message: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct StepDef {
    pub name: String,
    pub run: String,
    pub uses: Option<String>,
    /// Container image when `uses` is `docker`.
    #[serde(default)]
    pub image: Option<String>,
    /// Extra host→container bind mounts ("host:container"), docker only.
    #[serde(default)]
    pub volumes: Option<Vec<String>>,
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
    /// Step-level secret names — scoped only to this step
    #[serde(default)]
    pub secrets: Option<Vec<String>>,
    /// Run container in privileged mode (docker engine only).
    /// Required for Docker-in-Docker. Has host-escaping security implications.
    #[serde(default)]
    pub privileged: Option<bool>,
    /// Manager Secret name whose JSON value holds registry credentials for `docker login`.
    #[serde(default)]
    pub registry_secret: Option<String>,
}

// --- Secrets ---

#[derive(Debug, Serialize, Deserialize)]
pub struct CreateSecretRequest {
    pub name: String,
    pub value: String,
    #[serde(default)]
    pub owner_group: Option<String>,
    #[serde(default)]
    pub team_id: Option<String>,
    /// When true, same-team pipelines auto-receive this secret at job claim.
    /// Defaults to false (explicit bind / step reference / registry_secret only).
    #[serde(default)]
    pub team_shared: Option<bool>,
    /// `generic` (default) or `registry` (JSON credentials for `registry_secret`).
    #[serde(default)]
    pub kind: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UpdateSecretRequest {
    pub value: String,
    #[serde(default)]
    pub owner_group: Option<String>,
    #[serde(default)]
    pub team_id: Option<String>,
    /// When set, updates the opt-in team auto-inject flag.
    #[serde(default)]
    pub team_shared: Option<bool>,
    /// When set, updates secret kind (`generic` | `registry`).
    #[serde(default)]
    pub kind: Option<String>,
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
