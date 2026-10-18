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
    /// Super admin can view all audit logs and all API tokens.
    #[serde(default)]
    pub is_super_admin: Option<bool>,
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
    #[serde(default)]
    pub is_super_admin: Option<bool>,
}

// --- API tokens ---

#[derive(Debug, Serialize, Deserialize)]
pub struct CreateApiTokenRequest {
    /// Human-friendly label, e.g. "ci-script".
    pub name: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CreateApiTokenResponse {
    pub id: String,
    pub name: String,
    /// Full token value. Returned **once** at creation and never again.
    pub token: String,
    /// Display prefix (e.g. `buz_ab12…`) shown in list views.
    pub prefix: String,
    pub created_at: String,
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

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct HeartbeatResponse {
    /// UI-managed policy snapshot.
    #[serde(default)]
    pub policy: RunnerPolicyConfig,
    /// One-shot restart command: the Runner exits cleanly (exit 0) so its
    /// supervisor (docker `restart: unless-stopped` / systemd `Restart=always`)
    /// can bring it back up. `false` is the steady state; the Manager clears the
    /// DB flag once it has been served.
    #[serde(default)]
    pub restart: bool,
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
    /// Runner↔Manager clock skew in seconds (`Manager - Runner`); positive means
    /// the Runner is behind. Measured against the Manager's `Date` header at
    /// startup and refreshed on heartbeats — `None` when not measured yet or
    /// when the check is disabled (`[clock] enabled = false`).
    #[serde(default)]
    pub clock_skew_seconds: Option<i64>,
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
    /// Optional value constraint: string / number / boolean / host / choice.
    #[serde(default, rename = "type")]
    pub input_type: Option<String>,
    #[serde(default)]
    pub pattern: Option<String>,
    #[serde(default)]
    pub options: Option<Vec<String>>,
}

impl PipelineInputDef {
    pub fn is_masking(&self) -> bool {
        self.masking.unwrap_or(false)
    }

    pub fn env_key(&self) -> String {
        input_env_key(&self.name)
    }
}

pub fn input_env_key(name: &str) -> String {
    let chars: Vec<char> = name.chars().collect();
    let mut key = String::from("INPUT_");
    let mut separator_pending = false;
    for (index, ch) in chars.iter().copied().enumerate() {
        if !ch.is_ascii_alphanumeric() {
            separator_pending = !key.ends_with('_');
            continue;
        }
        let previous = index.checked_sub(1).and_then(|i| chars.get(i)).copied();
        let next = chars.get(index + 1).copied();
        let starts_word = ch.is_ascii_uppercase()
            && (previous.is_some_and(|p| p.is_ascii_lowercase() || p.is_ascii_digit())
                || (previous.is_some_and(|p| p.is_ascii_uppercase())
                    && next.is_some_and(|n| n.is_ascii_lowercase())));
        if (separator_pending || starts_word) && !key.ends_with('_') {
            key.push('_');
        }
        key.push(ch.to_ascii_uppercase());
        separator_pending = false;
    }
    let suffix = key.strip_prefix("INPUT_").unwrap_or_default();
    let normalized = suffix.trim_end_matches('_');
    if normalized.is_empty() {
        String::from("INPUT_")
    } else {
        format!("INPUT_{normalized}")
    }
}

/// Names of input params marked masking: true.
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

/// One input validation failure, keyed by input name for frontend field-level display.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputFieldError {
    pub name: String,
    pub message: String,
}

/// Constraint kinds an input can declare in YAML (`on.manual.inputs.*`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputTypeKind {
    String,
    Number,
    Boolean,
    Host,
    Choice,
    /// String list: one item per line, serialized to `INPUT_<NAME>` with
    /// newline separators (multi-target deploys). No nested maps — use a
    /// JSON string for structured values.
    List,
}

impl InputTypeKind {
    pub fn parse(raw: Option<&str>) -> Option<Self> {
        match raw.unwrap_or("string").trim() {
            "" | "string" => Some(Self::String),
            "number" => Some(Self::Number),
            "boolean" => Some(Self::Boolean),
            "host" => Some(Self::Host),
            "choice" => Some(Self::Choice),
            "list" => Some(Self::List),
            _ => None,
        }
    }
}

/// Apply declared input constraints to the resolved trigger inputs.
///
/// `defs` are the pipeline's `PipelineInputDef`s; `inputs` is the final map
/// (already merged with defaults). For `type: host`, `host_exists` decides
/// whether the referenced Buz Host is registered — it is a hook so the shared
/// layer stays DB-agnostic (Manager injects a query-backed closure).
///
/// Returns `Err` with one `InputFieldError` per offending field so the UI can
/// render field-level badges. Unknown type names are a YAML-definition problem,
/// reported as a validation error rather than silently ignored.
pub fn validate_input_values(
    defs: &[PipelineInputDef],
    inputs: &std::collections::HashMap<String, String>,
    host_exists: impl Fn(&str) -> bool,
) -> Result<(), Vec<InputFieldError>> {
    let mut errors: Vec<InputFieldError> = Vec::new();

    for def in defs {
        let kind = InputTypeKind::parse(def.input_type.as_deref());
        let Some(kind) = kind else {
            errors.push(InputFieldError {
                name: def.name.clone(),
                message: format!(
                    "unknown input type {:?}",
                    def.input_type.as_deref().unwrap_or("")
                ),
            });
            continue;
        };

        let Some(raw) = inputs.get(&def.name) else {
            if def.required.unwrap_or(false) {
                errors.push(InputFieldError {
                    name: def.name.clone(),
                    message: "required input is missing".into(),
                });
            }
            continue;
        };

        let value = raw.trim();
        if value.is_empty() {
            // Empty value == not provided: required fields are flagged, optional
            // fields skip type/pattern checks (defaults are merged upstream).
            if def.required.unwrap_or(false) {
                errors.push(InputFieldError {
                    name: def.name.clone(),
                    message: "required input is missing".into(),
                });
            }
            continue;
        }

        // `choice` needs its options declared; `options` without `choice` is ignored.
        if kind == InputTypeKind::Choice {
            match def.options.as_deref() {
                Some(opts) if !opts.is_empty() => {
                    if !opts.iter().any(|o| o == value) {
                        errors.push(InputFieldError {
                            name: def.name.clone(),
                            message: format!("value must be one of: {}", opts.join(", ")),
                        });
                    }
                }
                _ => {
                    errors.push(InputFieldError {
                        name: def.name.clone(),
                        message: "type: choice requires a non-empty options list".into(),
                    });
                }
            }
        } else if kind == InputTypeKind::Number {
            if value.parse::<i64>().is_err() {
                errors.push(InputFieldError {
                    name: def.name.clone(),
                    message: "must be an integer".into(),
                });
            }
        } else if kind == InputTypeKind::Boolean {
            let ok = matches!(value.to_ascii_lowercase().as_str(), "true" | "false");
            if !ok {
                errors.push(InputFieldError {
                    name: def.name.clone(),
                    message: "must be true or false".into(),
                });
            }
        } else if kind == InputTypeKind::Host && !host_exists(value) {
            errors.push(InputFieldError {
                name: def.name.clone(),
                message: format!("host '{value}' is not registered"),
            });
        }

        // `type: list` — one item per line; blank edge lines are tolerated,
        // but an empty item in the middle is a mistake. Declared `pattern`
        // applies to every element, not the joined text.
        if kind == InputTypeKind::List {
            let trimmed_block = value.trim_matches('\n');
            let items: Vec<&str> = trimmed_block.split('\n').map(str::trim).collect();
            if items.iter().any(|item| item.is_empty()) {
                errors.push(InputFieldError {
                    name: def.name.clone(),
                    message: "list contains an empty item — one entry per line".into(),
                });
            }
        }

        if let Some(pat) = def.pattern.as_deref() {
            if !pat.is_empty() {
                let re = regex::Regex::new(pat).map_err(|_| {
                    vec![InputFieldError {
                        name: def.name.clone(),
                        message: format!("invalid pattern '{pat}' in pipeline definition"),
                    }]
                })?;
                let matches = if kind == InputTypeKind::List {
                    value
                        .trim_matches('\n')
                        .split('\n')
                        .map(str::trim)
                        .filter(|item| !item.is_empty())
                        .all(|item| re.is_match(item))
                } else {
                    re.is_match(value)
                };
                if !matches {
                    errors.push(InputFieldError {
                        name: def.name.clone(),
                        message: if kind == InputTypeKind::List {
                            format!("every list item must match pattern '{pat}'")
                        } else {
                            format!("value does not match pattern '{pat}'")
                        },
                    });
                }
            }
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TriggerPipelineRequest {
    /// Optional key-value inputs (e.g. version, branch, environment).
    /// These are stored in the job snapshot and exported as INPUT_* env vars on the Runner.
    #[serde(default)]
    pub inputs: std::collections::HashMap<String, String>,
    /// Admin-only: when true, the Runner skips secret redaction in logs for this job.
    /// Debug-job logs are then only readable by platform admins. Ignored for non-admins.
    #[serde(default)]
    pub debug: Option<bool>,
    /// Any user: when true, the Runner emits extra execution detail (per-step
    /// duration, exit code, stage markers, full step config) while keeping
    /// secret redaction intact.
    #[serde(default)]
    pub verbose: Option<bool>,
    /// Optional platform Environment slug (or id). When set, Variable defaults
    /// are overlaid with that environment's overrides at claim time.
    /// Omit / empty = shared defaults only (legacy behavior).
    #[serde(default)]
    pub environment: Option<String>,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepDef {
    pub name: String,
    /// Step script/command. Empty for `uses:` steps — the resolver fills it
    /// from the referenced template before the task is stored.
    #[serde(default)]
    pub run: String,
    pub engine: Option<String>,
    /// Marketplace reference (`<namespace>/<name>@<pin>`). Set only by
    /// the step resolver when expanding templates — plain pipeline saves
    /// must not carry it (rejected in insert_steps).
    #[serde(default)]
    pub uses: Option<String>,
    /// Input values for a `uses:` step → `INPUT_<UPPER_SNAKE>` env at
    /// expansion. Only meaningful together with `uses`.
    #[serde(default)]
    pub with: Option<std::collections::BTreeMap<String, String>>,
    /// Container image when `engine` is `docker`.
    #[serde(default)]
    pub image: Option<String>,
    /// Extra host→container bind mounts ("host:container"), docker only.
    #[serde(default)]
    pub volumes: Option<Vec<String>>,
    /// Execution capabilities; the Runner must separately authorize them.
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// Host name(s) — single string or array. Only relevant for SSH steps.
    #[serde(default)]
    pub host: Option<OneOrMany>,
    /// Host labels to match — single string or array. SSH steps will run on all hosts matching any of these labels.
    #[serde(default, alias = "host_labels")]
    pub host_labels: Option<OneOrMany>,
    /// Multi-host SSH fan-out: `chain` (default) or `parallel`.
    #[serde(default)]
    pub hosts_strategy: Option<String>,
    pub dir: Option<String>,
    pub timeout: Option<String>,
    pub retry: Option<i32>,
    #[serde(rename = "continue-on-error", alias = "continue_on_error")]
    pub continue_on_error: Option<bool>,
    /// Step secret allowlist (optional narrowing). Does not auto-inject `$NAME`.
    #[serde(default)]
    pub secrets: Option<Vec<String>>,
    /// Step-level env map. Values may contain `${secrets.X}` / `${vars.X}`.
    #[serde(default)]
    pub env: Option<std::collections::BTreeMap<String, String>>,
    /// Run container in privileged mode (docker engine only).
    /// Required for Docker-in-Docker. Has host-escaping security implications.
    #[serde(default)]
    pub privileged: Option<bool>,
    /// Purpose slot: `${secrets.NAME}` or bare name for registry login JSON.
    #[serde(default)]
    pub registry_secret: Option<String>,
    /// Nested `config:` block (Phase 2b authoring shape).
    #[serde(default)]
    pub config: Option<crate::pipeline_yaml::ConfigBlock>,
    /// Purpose slot: `${secrets.NAME}` or bare OSS auth secret name.
    #[serde(default)]
    pub config_source: Option<String>,
    /// OSS object-key prefix (`config.key_path` / column `config_key_path`).
    #[serde(default)]
    pub config_key_path: Option<String>,
    /// Unpack directory (`config.dest` / column `config_dest`).
    #[serde(default)]
    pub config_dest: Option<String>,
    /// Render entries (`config.render` / column `config_render` JSON array).
    #[serde(default)]
    pub config_render: Option<Vec<crate::pipeline_yaml::ConfigRenderEntry>>,
}

/// Legacy name — prefer [`crate::pipeline_yaml::ConfigRenderEntry`].
pub type EnvRenderDef = crate::pipeline_yaml::ConfigRenderEntry;

// --- Secrets ---

#[derive(Debug, Serialize, Deserialize)]
pub struct CreateSecretRequest {
    pub name: String,
    pub value: String,
    #[serde(default)]
    pub owner_group: Option<String>,
    #[serde(default)]
    pub team_id: Option<String>,
    /// When true, same-team pipelines auto-allowlist this secret for
    /// `${secrets.NAME}` resolution (still not auto-injected as `$NAME`).
    /// Defaults to false (explicit bind / step reference / purpose slots).
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
