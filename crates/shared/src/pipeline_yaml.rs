use serde::{Deserialize, Serialize};

use crate::protocol::OneOrMany;

/// Buz pipeline definition in YAML format (simplified GitHub Actions style).
///
/// Unknown keys at every nesting level are rejected (`deny_unknown_fields`) so
/// typos like `ru:` / `runners:` / `grup:` fail Validate instead of being ignored.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PipelineYaml {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    /// Concurrency control: legacy string (backward compat) or full object.
    /// "build-lock" = concurrency group "build-lock" with discard-new (current behavior).
    #[serde(default)]
    pub concurrency: Option<ConcurrencyDef>,
    #[serde(default)]
    pub on: Option<TriggerYaml>,
    #[serde(default)]
    pub runner: Option<RunnerYaml>,
    #[serde(default)]
    pub env: Option<std::collections::BTreeMap<String, String>>,
    /// Platform variable names to bind (referenced from Manager UI).
    #[serde(default)]
    pub variables: Option<Vec<String>>,
    #[serde(default)]
    pub secrets: Option<Vec<String>>,
    /// Team slug this pipeline belongs to (Phase 2). Alias: `owner_group`.
    /// Resolved to `teams.id` by the Manager; defaults to `default` if omitted.
    /// Unknown slug fails Validate / create / update (no silent fallback).
    #[serde(default, alias = "owner_group")]
    pub team: Option<String>,
    /// Environments this pipeline may target (Phase 2 multi-env governance).
    /// Mandatory: every run must explicitly pick one of these at trigger time
    /// — a missing environment is rejected, so protection/approval/override
    /// gates can never be bypassed by omitting the field. Slugs are validated
    /// (charset + existence) by the Manager at save/scan time.
    #[serde(default)]
    pub environments: Vec<String>,
    /// End-of-build notification channels (`notify:`). Sent by Manager on
    /// job terminal status; webhook URLs must reference Secrets.
    #[serde(default)]
    pub notify: Option<Vec<NotifyChannelYaml>>,
    #[serde(default)]
    pub steps: Vec<StepYaml>,
}

/// One notification channel under top-level `notify:`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NotifyChannelYaml {
    #[serde(rename = "type")]
    pub channel_type: String,
    /// Webhook URL; must be `${secrets.NAME}` (or bare secret name allowlist).
    pub webhook: String,
    /// Events that trigger this channel. Default when omitted: `success`, `failure`.
    /// Allowed: `success` | `failure` | `cancelled`. YAML `failure` maps to job status `failed`.
    #[serde(default)]
    pub on: Option<Vec<String>>,
}

/// Default `on` events when a notify channel omits `on:`.
pub const DEFAULT_NOTIFY_ON: &[&str] = &["success", "failure"];

/// Map job status → notify event name (`failure` covers `failed` / `timeout`).
pub fn notify_event_for_job_status(status: &str) -> Option<&'static str> {
    match status {
        "success" => Some("success"),
        "failed" | "timeout" => Some("failure"),
        "cancelled" => Some("cancelled"),
        _ => None,
    }
}

/// Whether a channel should fire for the given job status.
///
/// Omitted / absent `on` → [`DEFAULT_NOTIFY_ON`]. Empty `on: []` → never.
pub fn notify_should_send(on: Option<&[String]>, job_status: &str) -> bool {
    let Some(event) = notify_event_for_job_status(job_status) else {
        return false;
    };
    match on {
        None => DEFAULT_NOTIFY_ON.contains(&event),
        Some([]) => false,
        Some(list) => list.iter().any(|e| e.trim() == event),
    }
}

/// Concurrency definition: string (legacy) or full object.
///
/// Custom deserialize (not `untagged`) so unknown object keys surface clearly
/// instead of falling through to "expected a string".
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum ConcurrencyDef {
    /// Legacy: `concurrency: "my-group"` — backward compat, equivalent to discard-new.
    Simple(String),
    /// New: `concurrency: { group: "my-group", mode: "queue", ... }`
    Full(ConcurrencyYaml),
}

impl<'de> Deserialize<'de> for ConcurrencyDef {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_yaml::Value::deserialize(deserializer)?;
        match value {
            serde_yaml::Value::String(s) => Ok(ConcurrencyDef::Simple(s)),
            serde_yaml::Value::Mapping(_) => {
                let full: ConcurrencyYaml =
                    serde_yaml::from_value(value).map_err(serde::de::Error::custom)?;
                Ok(ConcurrencyDef::Full(full))
            }
            other => Err(serde::de::Error::custom(format!(
                "concurrency must be a string or mapping, got {}",
                type_name_of_yaml(&other)
            ))),
        }
    }
}

fn type_name_of_yaml(v: &serde_yaml::Value) -> &'static str {
    match v {
        serde_yaml::Value::Null => "null",
        serde_yaml::Value::Bool(_) => "bool",
        serde_yaml::Value::Number(_) => "number",
        serde_yaml::Value::String(_) => "string",
        serde_yaml::Value::Sequence(_) => "sequence",
        serde_yaml::Value::Mapping(_) => "mapping",
        serde_yaml::Value::Tagged(_) => "tagged",
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConcurrencyYaml {
    pub group: String,
    #[serde(default = "default_concurrency_mode")]
    pub mode: String, // "discard-old" (default) | "queue" | "discard-new"
    #[serde(default = "default_max_queue", alias = "max-queue")]
    pub max_queue: u32,
    #[serde(default, alias = "queue-timeout")]
    pub queue_timeout: Option<String>,
}

pub fn default_concurrency_mode() -> String {
    "discard-old".into()
}
fn default_max_queue() -> u32 {
    3
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TriggerYaml {
    #[serde(default)]
    pub manual: Option<ManualTriggerYaml>,
    #[serde(default)]
    pub webhook: Option<WebhookTriggerYaml>,
    #[serde(default)]
    pub cron: Option<CronTriggerYaml>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManualTriggerYaml {
    #[serde(default)]
    pub inputs: Option<std::collections::BTreeMap<String, InputDef>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputDef {
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub required: Option<bool>,
    #[serde(default)]
    pub default: Option<String>,
    /// When true, values are redacted in logs, audit, and job snapshots (still injected as env at runtime).
    #[serde(default)]
    pub masking: Option<bool>,
    /// Optional value constraint. Supported kinds:
    ///   - absent / "string": free-form text
    ///   - "number": must parse as i64
    ///   - "boolean": "true"/"false" (case-insensitive)
    ///   - "host": must reference a registered Buz Host name (validated at trigger)
    ///   - "choice": value must be one of `options` (options are required then)
    #[serde(default)]
    pub r#type: Option<String>,
    /// Optional ECMAScript-compatible regex; the resolved value must match.
    #[serde(default)]
    pub pattern: Option<String>,
    /// Allowed values for `type: choice`.
    #[serde(default)]
    pub options: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WebhookTriggerYaml {
    #[serde(default)]
    pub path: Option<String>,
    /// Optional bare team secret name. When set, webhook requests must carry
    /// `X-Buz-Signature: sha256=<hex HMAC-SHA256 of the raw body>`.
    /// Only the name is stored; the value is resolved at trigger time.
    #[serde(default)]
    pub secret: Option<String>,
    /// Registered pipeline environment to use when the caller omits one.
    #[serde(default)]
    pub environment: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CronTriggerYaml {
    #[serde(default)]
    pub schedule: Option<String>,
    /// Registered pipeline environment to deploy to on each scheduled run.
    #[serde(default)]
    pub environment: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunnerYaml {
    #[serde(default)]
    pub labels: Option<Vec<String>>,
    #[serde(default)]
    pub timeout: Option<String>,
}

/// One `config.render` entry (template → output file).
///
/// Runs after delivery (OSS unpack / prior clone) places files under
/// `$BUZ_CONFIG_DIR`, and before the run block. References resolve against
/// job secrets (`${secrets.X}`) and vars (`${vars.X}`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ConfigRenderEntry {
    /// Template file relative to `$BUZ_CONFIG_DIR` (e.g. `env.example`).
    #[serde(default)]
    pub template: Option<String>,
    /// Output file relative to `$BUZ_CONFIG_DIR`. Defaults to
    /// `<template>.rendered` when unset at runtime.
    #[serde(default)]
    pub output: Option<String>,
    /// Output file mode (e.g. `0600`).
    #[serde(default)]
    pub mode: Option<String>,
    /// `fail` (default) aborts on unresolved refs; `warn` writes empty `KEY=`.
    #[serde(default)]
    pub on_missing: Option<String>,
    /// `recreate` (Phase 2d — parsed, not enforced).
    #[serde(default)]
    pub on_change: Option<String>,
    /// Compose services for `on_change` (Phase 2d — parsed, not enforced).
    #[serde(default)]
    pub services: Option<Vec<String>>,
    /// Number of timestamped `<output>.bak.<ts>` backups to keep when the
    /// render overwrites an existing output file. `0` disables backups.
    /// Defaults to `3` when unset.
    #[serde(default)]
    pub backup_keep: Option<u32>,
}

/// Legacy alias — prefer [`ConfigRenderEntry`].
pub type EnvRender = ConfigRenderEntry;

/// `config.render`: a single object or a list of objects.
///
/// Custom deserialize so unknown keys on a single object (e.g. `out:`) are not
/// swallowed by untagged fallback to the list variant.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum ConfigRender {
    One(ConfigRenderEntry),
    Many(Vec<ConfigRenderEntry>),
}

impl<'de> Deserialize<'de> for ConfigRender {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_yaml::Value::deserialize(deserializer)?;
        match value {
            serde_yaml::Value::Sequence(_) => {
                let many: Vec<ConfigRenderEntry> =
                    serde_yaml::from_value(value).map_err(serde::de::Error::custom)?;
                Ok(ConfigRender::Many(many))
            }
            serde_yaml::Value::Mapping(_) => {
                let one: ConfigRenderEntry =
                    serde_yaml::from_value(value).map_err(serde::de::Error::custom)?;
                Ok(ConfigRender::One(one))
            }
            other => Err(serde::de::Error::custom(format!(
                "config.render must be an object or a list, got {}",
                type_name_of_yaml(&other)
            ))),
        }
    }
}

impl ConfigRender {
    pub fn entries(&self) -> &[ConfigRenderEntry] {
        match self {
            Self::One(e) => std::slice::from_ref(e),
            Self::Many(v) => v.as_slice(),
        }
    }

    pub fn into_vec(self) -> Vec<ConfigRenderEntry> {
        match self {
            Self::One(e) => vec![e],
            Self::Many(v) => v,
        }
    }
}

/// Nested step `config:` block (Phase 2b).
///
/// Modes (see docs/01-architecture/config-block.md):
/// - **OSS deliver:** `source` + `key_path` + `dest`
/// - **render-only:** `dest` + `render`
/// - **sugar:** deliver fields + `render` in one step (allowed, not preferred)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ConfigBlock {
    /// OSS auth secret name (`kind = aliyun_oss_auth`).
    #[serde(default)]
    pub source: Option<String>,
    /// OSS object-key prefix; Manager appends `/configs.tar.gz`.
    #[serde(default)]
    pub key_path: Option<String>,
    /// Unpack / working directory → `$BUZ_CONFIG_DIR`.
    #[serde(default)]
    pub dest: Option<String>,
    /// One or more render entries (relative to `dest`).
    #[serde(default)]
    pub render: Option<ConfigRender>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StepYaml {
    pub name: Option<String>,
    #[serde(default)]
    pub run: Option<String>,
    #[serde(default)]
    pub engine: Option<String>,
    /// Container image when `engine: docker`.
    #[serde(default)]
    pub image: Option<String>,
    /// Extra host→container bind mounts (only for engine: docker). Each entry is "host_path:container_path".
    #[serde(default)]
    pub volumes: Option<Vec<String>>,
    /// Run the container in privileged mode (only for engine: docker).
    /// Required for true Docker-in-Docker (docker:dind) to support nested containers.
    /// WARNING: privileged containers can escape to the host via nsenter or kernel exploits.
    /// Only use on disposable runner VMs.
    #[serde(default)]
    pub privileged: Option<bool>,
    /// Name of a Manager Secret whose value is JSON registry credentials
    /// (`registry` optional when `image` has a host, `username`, `password`).
    /// Used for `docker login` on docker / shell / SSH steps.
    #[serde(default)]
    pub registry_secret: Option<String>,
    /// Nested config delivery + render (Phase 2b). Required shape for YAML.
    #[serde(default)]
    pub config: Option<ConfigBlock>,
    /// Legacy flat fields — if present, validation fails (use `config:` instead).
    #[serde(default)]
    pub config_source: Option<String>,
    #[serde(default)]
    pub config_path: Option<String>,
    #[serde(default)]
    pub config_dir: Option<String>,
    #[serde(default)]
    pub env_render: Option<serde_yaml::Value>,
    #[serde(default)]
    pub host: Option<OneOrMany>,
    #[serde(default, alias = "work_dir")]
    pub dir: Option<String>,
    #[serde(default)]
    pub host_labels: Option<OneOrMany>,
    /// Multi-host SSH fan-out: `chain` (default) or `parallel`.
    #[serde(default)]
    pub hosts_strategy: Option<String>,
    #[serde(default)]
    pub timeout: Option<String>,
    #[serde(default)]
    pub retry: Option<i32>,
    /// Marketplace step reference: `<namespace>/<name>@<pin>` (pin is
    /// `X`, `X.Y`, `X.Y.Z`, or `sha256:<hex>`; ranges/`latest` rejected).
    /// Mutually exclusive with `engine:` / `run:` / `image:`.
    #[serde(default)]
    pub uses: Option<String>,
    /// Input values for a `uses:` step, mapped to `INPUT_<UPPER_SNAKE>` env
    /// at resolution time. Values may use `${secrets.X}` / `${vars.X}`.
    #[serde(default)]
    pub with: Option<std::collections::BTreeMap<String, String>>,
    /// Prefer kebab-case `continue-on-error`; snake_case accepted as alias.
    #[serde(default, rename = "continue-on-error", alias = "continue_on_error")]
    pub continue_on_error: Option<bool>,
    /// Step secret allowlist (optional narrowing). Does **not** inject `$NAME`;
    /// map via `env:` or purpose slots (`registry_secret`, `config.source`).
    #[serde(default)]
    pub secrets: Option<Vec<String>>,
    /// Step-level env map. Values may use `${secrets.X}` / `${vars.X}`;
    /// expanded at claim time onto the process environment.
    #[serde(default)]
    pub env: Option<std::collections::BTreeMap<String, String>>,
}

impl From<&StepYaml> for crate::protocol::StepDef {
    fn from(step: &StepYaml) -> Self {
        let (config_source, config_key_path, config_dest, config_render) =
            flatten_config(step.config.as_ref());
        Self {
            name: step.name.clone().unwrap_or_else(|| "unnamed".into()),
            run: step.run.clone().unwrap_or_default(),
            engine: step.engine.clone(),
            uses: step.uses.clone(),
            with: step.with.clone(),
            image: step.image.clone(),
            volumes: step.volumes.clone(),
            capabilities: Vec::new(),
            host: step.host.clone(),
            host_labels: step.host_labels.clone(),
            hosts_strategy: step.hosts_strategy.clone(),
            dir: step.dir.clone(),
            timeout: step.timeout.clone(),
            retry: step.retry,
            continue_on_error: step.continue_on_error,
            secrets: step.secrets.clone(),
            env: step.env.clone(),
            privileged: step.privileged,
            registry_secret: step.registry_secret.clone(),
            config: step.config.clone(),
            config_source,
            config_key_path,
            config_dest,
            config_render,
        }
    }
}

/// Result of YAML validation.
#[derive(Debug, Clone, Serialize)]
pub struct YamlValidationResult {
    pub valid: bool,
    pub name: Option<String>,
    pub errors: Vec<YamlValidationError>,
    pub warnings: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parsed: Option<PipelineYamlData>,
}

/// Serializable subset of PipelineYaml for the frontend to import into the form.
#[derive(Debug, Clone, Serialize)]
pub struct PipelineYamlData {
    /// Environments this pipeline may target at trigger time.
    pub environments: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trigger_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trigger_config: Option<TriggerConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runner: Option<RunnerConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub concurrency: Option<String>,
    /// Full concurrency settings (new block format).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub concurrency_config: Option<ConcurrencyConfig>,
    /// Pipeline-level user-defined env vars (key-value).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<std::collections::BTreeMap<String, String>>,
    /// Platform variable names to bind (referenced from Manager UI).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub variables: Option<Vec<String>>,
    /// Pipeline-level secret names referenced from the YAML.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secrets: Option<Vec<String>>,
    /// End-of-build notification channels (expressions kept; not resolved).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notify: Option<Vec<NotifyChannelData>>,
    pub steps: Vec<StepData>,
}

/// Serializable notify channel for Validate → frontend / Definition.
#[derive(Debug, Clone, Serialize)]
pub struct NotifyChannelData {
    #[serde(rename = "type")]
    pub channel_type: String,
    pub webhook: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TriggerConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub webhook_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub webhook_environment: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cron_expr: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cron_environment: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inputs: Option<Vec<InputData>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunnerConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub labels: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConcurrencyConfig {
    pub group: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_queue: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub queue_timeout: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct InputData {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub masking: Option<bool>,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub input_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct StepData {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub engine: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    /// Comma-separated host:container volume mounts (for frontend import).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub volumes: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host_labels: Option<String>,
    /// `chain` (default) or `parallel` — SSH multi-host fan-out.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hosts_strategy: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub continue_on_error: Option<bool>,
    /// Step secret allowlist (optional narrowing).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secrets: Option<Vec<String>>,
    /// Step-level env map (`env:` under the step).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<std::collections::BTreeMap<String, String>>,
    /// Privileged mode flag (only for docker steps).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub privileged: Option<bool>,
    /// Purpose slot: `${secrets.NAME}` or bare secret name for registry login.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub registry_secret: Option<String>,
    /// Nested `config:` block (authoring / import shape).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<ConfigBlock>,
    /// Flattened from `config` for Manager task columns / API.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config_source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config_key_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config_dest: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config_render: Option<Vec<ConfigRenderEntry>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct YamlValidationError {
    pub field: String,
    pub message: String,
}

fn non_empty(s: Option<&str>) -> Option<&str> {
    s.map(str::trim).filter(|v| !v.is_empty())
}

/// Normalize SSH multi-host strategy. Empty / omitted → `chain` (one host after another).
///
/// Accepts legacy alias `serial` → `chain` (pre-release name).
pub fn normalize_hosts_strategy(raw: Option<&str>) -> Result<&'static str, String> {
    match raw
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("chain")
    {
        "chain" | "serial" => Ok("chain"),
        "parallel" => Ok("parallel"),
        other => Err(format!(
            "invalid hosts_strategy '{other}' — must be chain or parallel"
        )),
    }
}

/// Parse pipeline duration strings: `30`, `30s`, `10m`, `1h` (case-insensitive unit).
///
/// - Trims surrounding whitespace only — **internal spaces are rejected** (`10x m` → error).
/// - Bare digits mean seconds.
/// - Returns seconds as `i64` (> 0).
pub fn parse_duration_seconds(raw: &str) -> Result<i64, String> {
    let s = raw.trim();
    if s.is_empty() {
        return Err("duration is empty".into());
    }
    if s.chars().any(|c| c.is_whitespace()) {
        return Err(format!(
            "invalid duration '{raw}' — no spaces allowed (use 30s, 10m, or 1h)"
        ));
    }
    let lower = s.to_ascii_lowercase();
    let (num, mult): (&str, i64) = if let Some(n) = lower.strip_suffix('s') {
        (n, 1)
    } else if let Some(n) = lower.strip_suffix('m') {
        (n, 60)
    } else if let Some(n) = lower.strip_suffix('h') {
        (n, 3600)
    } else {
        (lower.as_str(), 1)
    };
    if num.is_empty() || !num.chars().all(|c| c.is_ascii_digit()) {
        return Err(format!(
            "invalid duration '{raw}' — expected <number> with optional unit s|m|h (e.g. 30s, 10m, 1h)"
        ));
    }
    let n: i64 = num
        .parse()
        .map_err(|_| format!("invalid duration '{raw}' — number out of range"))?;
    if n <= 0 {
        return Err(format!("invalid duration '{raw}' — must be > 0"));
    }
    n.checked_mul(mult)
        .ok_or_else(|| format!("invalid duration '{raw}' — overflow when converting to seconds"))
}

fn validate_duration_field(field: &str, raw: Option<&str>, errors: &mut Vec<YamlValidationError>) {
    let Some(raw) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return;
    };
    if let Err(msg) = parse_duration_seconds(raw) {
        errors.push(YamlValidationError {
            field: field.into(),
            message: msg,
        });
    }
}

/// Validate a nested `config:` block (deliver / render-only / sugar).
fn validate_config_block(
    cfg: &ConfigBlock,
    engine: &str,
    field: &str,
    errors: &mut Vec<YamlValidationError>,
) {
    let source = non_empty(cfg.source.as_deref());
    let key_path = non_empty(cfg.key_path.as_deref());
    let dest = non_empty(cfg.dest.as_deref());
    let render_entries = cfg.render.as_ref().map(|r| r.entries()).unwrap_or(&[]);
    let has_render = !render_entries.is_empty();

    if source.is_none() && key_path.is_none() && dest.is_none() && !has_render {
        errors.push(YamlValidationError {
            field: field.into(),
            message: "'config:' is empty — set deliver (source+key_path+dest) and/or render-only (dest+render)".into(),
        });
        return;
    }

    if source.is_some() != key_path.is_some() {
        errors.push(YamlValidationError {
            field: field.into(),
            message: "'config.source' and 'config.key_path' must be set together (OSS deliver)"
                .into(),
        });
    }

    let is_deliver = source.is_some() && key_path.is_some();
    let is_render_only = !is_deliver && has_render;
    let is_sugar = is_deliver && has_render;

    if is_deliver || is_sugar {
        let engine = if engine.is_empty() { "shell" } else { engine };
        if !matches!(engine, "shell" | "local" | "ssh" | "docker") {
            errors.push(YamlValidationError {
                field: field.into(),
                message:
                    "'config' OSS deliver is only valid for shell, local, ssh, or docker steps"
                        .into(),
            });
        }
        if dest.is_none() {
            errors.push(YamlValidationError {
                field: field.into(),
                message: "'config.dest' is required for OSS deliver".into(),
            });
        }
        if let Some(p) = key_path {
            // Manager always builds `{key_path.trim('/')}/configs.tar.gz`. Trailing
            // '/' is optional. Including the archive filename would double-append.
            let trimmed = p.trim().trim_matches('/');
            if trimmed.ends_with("configs.tar.gz") {
                errors.push(YamlValidationError {
                    field: format!("{field}.key_path"),
                    message: "'config.key_path' must be a directory prefix — Manager appends '/configs.tar.gz' (do not include the archive filename)".into(),
                });
            }
        }
    }

    if is_render_only || is_sugar {
        let engine = if engine.is_empty() { "shell" } else { engine };
        if !matches!(engine, "shell" | "local" | "ssh") {
            errors.push(YamlValidationError {
                field: field.into(),
                message: "'config.render' is only valid for shell, local, or ssh steps".into(),
            });
        }
        if dest.is_none() {
            errors.push(YamlValidationError {
                field: field.into(),
                message: "'config.dest' is required when 'config.render' is set".into(),
            });
        }
    }

    // Partial junk: render without deliver source but also missing dest already handled;
    // key_path/source alone handled; dest-only without render/deliver is useless.
    if !is_deliver && !has_render && dest.is_some() {
        errors.push(YamlValidationError {
            field: field.into(),
            message:
                "'config.dest' alone is not enough — add OSS deliver (source+key_path) or render"
                    .into(),
        });
    }

    for (i, er) in render_entries.iter().enumerate() {
        let entry_field = if render_entries.len() == 1 {
            format!("{field}.config.render")
        } else {
            format!("{field}.config.render[{i}]")
        };
        let template = non_empty(er.template.as_deref()).unwrap_or("");
        if template.is_empty() {
            errors.push(YamlValidationError {
                field: entry_field.clone(),
                message: "'template' is required (relative to $BUZ_CONFIG_DIR, e.g. env.example)"
                    .into(),
            });
        }
        if let Some(om) = non_empty(er.on_missing.as_deref()) {
            if !matches!(om, "fail" | "warn") {
                errors.push(YamlValidationError {
                    field: entry_field.clone(),
                    message: "'on_missing' must be 'fail' or 'warn'".into(),
                });
            }
        }
        if let Some(oc) = non_empty(er.on_change.as_deref()) {
            if !matches!(oc, "recreate") {
                errors.push(YamlValidationError {
                    field: entry_field,
                    message: "'on_change' must be 'recreate'".into(),
                });
            }
        }
    }
}

/// Flatten nested `config:` into task/API column fields.
pub fn flatten_config(
    cfg: Option<&ConfigBlock>,
) -> (
    Option<String>,
    Option<String>,
    Option<String>,
    Option<Vec<ConfigRenderEntry>>,
) {
    let Some(cfg) = cfg else {
        return (None, None, None, None);
    };
    let source = non_empty(cfg.source.as_deref()).map(|s| s.to_string());
    let key_path = non_empty(cfg.key_path.as_deref()).map(|s| s.to_string());
    let dest = non_empty(cfg.dest.as_deref()).map(|s| s.to_string());
    let render = cfg
        .render
        .as_ref()
        .map(|r| r.entries().to_vec())
        .filter(|v| !v.is_empty());
    (source, key_path, dest, render)
}

/// Collect secret names that this pipeline effectively allowlists.
///
/// Includes optional top-level `secrets:`, step `secrets:`, purpose slots,
/// `${secrets.NAME}` inside pipeline/step `env:`, and `notify[].webhook` —
/// so production YAML may omit the top-level list (GHA-style) while Manager
/// still binds `pipeline_secrets`.
pub fn collect_effective_secret_names(parsed: &PipelineYaml) -> Vec<String> {
    use std::collections::BTreeSet;
    let mut names = BTreeSet::new();

    if let Some(list) = &parsed.secrets {
        for n in list {
            let t = n.trim();
            if !t.is_empty() {
                names.insert(t.to_string());
            }
        }
    }
    if let Some(env) = &parsed.env {
        for v in env.values() {
            for n in crate::secret_ref::collect_secrets_names(v) {
                names.insert(n);
            }
        }
    }
    for step in &parsed.steps {
        if let Some(list) = &step.secrets {
            for n in list {
                let t = n.trim();
                if !t.is_empty() {
                    names.insert(t.to_string());
                }
            }
        }
        if let Some(env) = &step.env {
            for v in env.values() {
                for n in crate::secret_ref::collect_secrets_names(v) {
                    names.insert(n);
                }
            }
        }
        if let Some(raw) = step.registry_secret.as_deref() {
            if let Some(n) = crate::secret_ref::resolve_secret_slot(raw) {
                names.insert(n);
            }
        }
        if let Some(cfg) = &step.config {
            if let Some(raw) = cfg.source.as_deref() {
                if let Some(n) = crate::secret_ref::resolve_secret_slot(raw) {
                    names.insert(n);
                }
            }
        }
    }
    if let Some(channels) = &parsed.notify {
        for ch in channels {
            if let Some(n) = crate::secret_ref::resolve_secret_slot(&ch.webhook) {
                names.insert(n);
            }
        }
    }
    names.into_iter().collect()
}

/// Map a serde_yaml parse failure to a validation error with a navigable `field`.
///
/// Unknown keys become `steps[0].ru` (path + key) when the message includes a
/// path prefix, so the editor can highlight the bad keyword.
fn yaml_parse_error_to_validation(e: &serde_yaml::Error) -> YamlValidationError {
    let msg = e.to_string();
    let field = extract_field_from_yaml_parse_error(&msg).unwrap_or_else(|| {
        if msg.contains("unknown field") {
            "yaml.keys".into()
        } else {
            "yaml".into()
        }
    });
    // Prefer precise line:column when serde provides it (helps FE fallback).
    let message = if let Some(loc) = e.location() {
        format!(
            "YAML error: {msg} (at line {} column {})",
            loc.line(),
            loc.column()
        )
    } else {
        format!("YAML error: {msg}")
    };
    // Avoid duplicating "at line N column M" if serde already included it.
    let message = if msg.contains("at line") && message.matches("at line").count() > 1 {
        format!("YAML error: {msg}")
    } else {
        message
    };
    YamlValidationError { field, message }
}

/// Extract `steps[0].ru` from messages like `steps[0]: unknown field \`ru\`, expected …`.
fn extract_field_from_yaml_parse_error(msg: &str) -> Option<String> {
    let key = extract_unknown_field_name(msg)?;
    // Take the path immediately before ": unknown field"
    let marker = ": unknown field";
    let path = msg.find(marker).and_then(|idx| {
        let before = msg[..idx].trim_end();
        // Drop leading "YAML error:" if present (usually not yet wrapped).
        let before = before
            .strip_prefix("YAML error:")
            .map(str::trim)
            .unwrap_or(before);
        // Last path-like token (handles ", steps[0]" noise).
        let token = before.split(',').next_back()?.trim();
        if token.is_empty() {
            None
        } else {
            Some(token.to_string())
        }
    });
    match path {
        Some(p)
            if p.contains('[')
                || p.contains('.')
                || p.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') =>
        {
            Some(format!("{p}.{key}"))
        }
        _ => Some(key),
    }
}

fn extract_unknown_field_name(msg: &str) -> Option<String> {
    for (open, close) in [("`", "`"), ("'", "'"), ("\"", "\"")] {
        let needle = format!("unknown field {open}");
        if let Some(idx) = msg.find(&needle) {
            let rest = &msg[idx + needle.len()..];
            if let Some(end) = rest.find(close) {
                let key = rest[..end].trim();
                if !key.is_empty() {
                    return Some(key.to_string());
                }
            }
        }
    }
    None
}

/// Parsed pin portion of a `uses:` reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UsesPin {
    /// `@1`, `@1.2`, `@1.2.0` — exact or major pin.
    Version(String),
    /// `@sha256:<64 hex>` — strict content pin.
    Sha256(String),
}

/// Parse a `uses:` reference (`<namespace>/<name>@<pin>`).
///
/// IDs use the same canonical two-segment grammar as step specs. Pins follow
/// §4 of the marketplace ADR: `@1` major, `@1.2` / `@1.2.0` exact,
/// `@sha256:<hex>` strict. Ranges and `latest` are deliberately not supported
/// — resolution must be reproducible ("解析即锁定").
pub fn parse_uses_reference(raw: &str) -> Result<(String, UsesPin), String> {
    let raw = raw.trim();
    let Some((reference, pin)) = raw.rsplit_once('@') else {
        return Err("missing '@pin' — expected '<namespace>/<name>@<pin>' \
             (e.g. buz/feishu@1.2)"
            .into());
    };
    let reference = reference.trim();
    let segments: Vec<&str> = reference.split('/').collect();
    let canonical_segment = |segment: &str| {
        !segment.is_empty()
            && segment
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    };
    if segments.len() != 2 || !segments.iter().all(|segment| canonical_segment(segment)) {
        return Err(
            "uses reference must be '<namespace>/<name>@<pin>' with lowercase [a-z0-9-] segments"
                .into(),
        );
    }

    let pin = pin.trim();
    if let Some(hex) = pin.strip_prefix("sha256:") {
        if hex.len() == 64 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return Ok((
                reference.to_string(),
                UsesPin::Sha256(hex.to_ascii_lowercase()),
            ));
        }
        return Err("sha256 pin must be 'sha256:<64 hex chars>'".into());
    }

    let parts: Vec<&str> = pin.split('.').collect();
    let version_like = parts.len() <= 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()));
    if version_like {
        Ok((reference.to_string(), UsesPin::Version(pin.to_string())))
    } else {
        Err(format!(
            "unsupported pin '{pin}' — use @1, @1.2, @1.2.0 or @sha256:<64 hex> \
             (ranges and 'latest' are not supported)"
        ))
    }
}

/// Parse a pipeline YAML string and run validation checks.
pub fn parse_and_validate(raw: &str) -> YamlValidationResult {
    let mut errors: Vec<YamlValidationError> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();

    let parsed: PipelineYaml = match serde_yaml::from_str(raw) {
        Ok(p) => p,
        Err(e) => {
            let err = yaml_parse_error_to_validation(&e);
            return YamlValidationResult {
                valid: false,
                name: None,
                errors: vec![err],
                warnings: vec![],
                parsed: None,
            };
        }
    };

    // The typed pipeline schema intentionally has no author-controlled capability
    // field. Inspect the original YAML so attempts to add one get a clear error.
    let raw_steps = serde_yaml::from_str::<serde_yaml::Value>(raw)
        .ok()
        .and_then(|value| {
            value
                .get("steps")
                .and_then(serde_yaml::Value::as_sequence)
                .cloned()
        })
        .unwrap_or_default();

    // --- Validation rules ---

    // 1. name is required
    let name = parsed.name.trim();
    if name.is_empty() {
        errors.push(YamlValidationError {
            field: "name".into(),
            message: "Pipeline name is required".into(),
        });
    }

    // 2. steps array must not be empty
    if parsed.steps.is_empty() {
        errors.push(YamlValidationError {
            field: "steps".into(),
            message: "At least one step is required".into(),
        });
    }

    // 2b. environments — mandatory target binding (multi-env governance).
    // Every run must explicitly pick one of the declared environments so the
    // protection/approval/override gates can never be bypassed by omission.
    {
        let envs = &parsed.environments;
        if envs.is_empty() {
            errors.push(YamlValidationError {
                field: "environments".into(),
                message: "environments is required — declare which environments this \
                          pipeline may target (e.g. environments: [staging, production])"
                    .into(),
            });
        }
        if let Some(on) = parsed.on.as_ref() {
            if let Some(webhook) = on.webhook.as_ref() {
                if let Some(target) = webhook.environment.as_deref() {
                    if target.trim().is_empty() {
                        errors.push(YamlValidationError {
                            field: "on.webhook.environment".into(),
                            message: "webhook environment must not be empty or whitespace".into(),
                        });
                    } else if target != target.trim() {
                        errors.push(YamlValidationError {
                            field: "on.webhook.environment".into(),
                            message: "webhook environment must not contain surrounding whitespace"
                                .into(),
                        });
                    } else if !envs.iter().any(|declared| declared == target) {
                        errors.push(YamlValidationError {
                            field: "on.webhook.environment".into(),
                            message: format!("webhook environment '{target}' must appear in the top-level environments allowlist"),
                        });
                    }
                }
            }
            if let Some(cron) = on.cron.as_ref() {
                match cron.environment.as_deref() {
                    Some(target) if target.trim().is_empty() => errors.push(YamlValidationError {
                        field: "on.cron.environment".into(),
                        message: "cron environment must not be empty or whitespace".into(),
                    }),
                    Some(target) if target != target.trim() => errors.push(YamlValidationError {
                        field: "on.cron.environment".into(),
                        message: "cron environment must not contain surrounding whitespace".into(),
                    }),
                    Some(target) if envs.iter().any(|declared| declared == target) => {}
                    Some(target) => errors.push(YamlValidationError {
                        field: "on.cron.environment".into(),
                        message: format!("cron environment '{target}' must appear in the top-level environments allowlist"),
                    }),
                    None => errors.push(YamlValidationError {
                        field: "on.cron.environment".into(),
                        message: "cron triggers require an explicit registered environment".into(),
                    }),
                }
            }
        }

        let mut seen = std::collections::HashSet::new();
        for raw_slug in envs {
            let slug = raw_slug.trim();
            let valid_slug = !slug.is_empty()
                && raw_slug == slug
                && slug.len() <= 64
                && !slug.starts_with('-')
                && !slug.ends_with('-')
                && slug.chars().all(|c| {
                    c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_')
                });
            if !valid_slug {
                errors.push(YamlValidationError {
                    field: "environments".into(),
                    message: format!(
                        "environment slug {slug:?} is invalid — use lowercase letters, \
                         numbers, '-' or '_'"
                    ),
                });
            }
            if !seen.insert(slug.to_string()) {
                errors.push(YamlValidationError {
                    field: "environments".into(),
                    message: format!("environment '{slug}' is declared more than once"),
                });
            }
        }
    }

    // 3. Each step must have `run`; `engine` must be a known value when set.
    const VALID_ENGINES: &[&str] = &["shell", "local", "ssh", "docker"];
    for (i, step) in parsed.steps.iter().enumerate() {
        if raw_steps
            .get(i)
            .and_then(serde_yaml::Value::as_mapping)
            .is_some_and(|mapping| {
                mapping.contains_key(serde_yaml::Value::String("capabilities".into()))
            })
        {
            errors.push(YamlValidationError {
                field: format!("steps[{i}].capabilities"),
                message: "capabilities are granted only by a locked plugin template; raw steps cannot request them".into(),
            });
        }

        let uses_ref = match step
            .uses
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            Some(raw) => match parse_uses_reference(raw) {
                Ok(parsed) => Some(parsed),
                Err(msg) => {
                    errors.push(YamlValidationError {
                        field: format!("steps[{i}].uses"),
                        message: msg,
                    });
                    None
                }
            },
            None => None,
        };

        let run = step.run.as_deref().unwrap_or("").trim();

        if uses_ref.is_some() {
            // Marketplace step: `image:` comes from the resolved template and
            // has no call-site override — statically rejected. `engine:` is
            // allowed here as a deployment-context override; whether the
            // template supports the requested engine is enforced at resolution
            // time against its declared `engines` ("shell/ssh unsupported
            // override" errors). `run:` is appended after the template run
            // block (template = environment, call site = commands).
            for (field, value) in [("image", step.image.as_deref().unwrap_or("").trim())] {
                if !value.is_empty() {
                    errors.push(YamlValidationError {
                        field: format!("steps[{i}].{field}"),
                        message: format!(
                            "'uses:' steps cannot declare '{field}:' — it comes from the \
                             referenced step template"
                        ),
                    });
                }
            }
        } else if run.is_empty() {
            errors.push(YamlValidationError {
                field: format!("steps[{i}].run"),
                message: "Step requires 'run' (script/command to execute)".into(),
            });
        }

        if step.with.is_some() && uses_ref.is_none() {
            errors.push(YamlValidationError {
                field: format!("steps[{i}].with"),
                message: "'with:' requires 'uses:' — it provides inputs for a referenced \
                          step template"
                    .into(),
            });
        }

        let engine = step.engine.as_deref().unwrap_or("").trim();
        if !engine.is_empty() && !VALID_ENGINES.contains(&engine) {
            errors.push(YamlValidationError {
                field: format!("steps[{i}].engine"),
                message: format!(
                    "invalid engine '{engine}' — must be one of: {}",
                    VALID_ENGINES.join(", ")
                ),
            });
        }

        if raw_steps
            .get(i)
            .and_then(serde_yaml::Value::as_mapping)
            .is_some_and(|mapping| {
                mapping.contains_key(serde_yaml::Value::String("capabilities".into()))
            })
        {
            errors.push(YamlValidationError {
                field: format!("steps[{i}].capabilities"),
                message: "capabilities are granted only by a locked plugin template; raw steps cannot request them".into(),
            });
        }

        if step.name.as_deref().unwrap_or("").trim().is_empty() {
            warnings.push(format!(
                "steps[{}] has no 'name' — consider adding one for readability",
                i
            ));
        }

        // Deprecation warnings (2.0 removal plan): registry login and OSS
        // config delivery have plugin/core replacements. Non-blocking so
        // existing pipelines keep validating.
        if step.registry_secret.is_some() {
            warnings.push(format!(
                "steps[{i}].registry_secret: deprecated — use the buz/docker-login \
                 template for registry login; removal planned in 2.0"
            ));
        }
        if let Some(cfg) = &step.config {
            if cfg.source.is_some() || cfg.key_path.is_some() {
                warnings.push(format!(
                    "steps[{i}].config.source: deprecated — deliver with the buz/config-fetch \
                     template and render via config.render; removal planned in 2.0"
                ));
            }
        }

        // Honest-schema warnings: fields the schema accepts but the runner does
        // not act on yet. Non-blocking so existing pipelines keep validating.
        if step.retry.unwrap_or(0) > 0 {
            warnings.push(format!(
                "steps[{i}].retry: automatic retries are not executed yet — the step runs once"
            ));
        }
        if let Some(render_entries) = step
            .config
            .as_ref()
            .and_then(|c| c.render.as_ref())
            .map(|r| r.entries())
        {
            for (j, entry) in render_entries.iter().enumerate() {
                if entry.on_change.is_some() || entry.services.is_some() {
                    warnings.push(format!(
                        "steps[{i}].config.render[{j}]: 'on_change'/'services' are parsed \
                         but not enforced yet — output is written without service recreation"
                    ));
                }
            }
        }
    }

    // 3b. Manual trigger inputs — remind to declare an explicit `type` so the
    // trigger dialog renders a dedicated control and constraints are enforced.
    // `options` only takes effect with `type: choice`; flag it specifically.
    if let Some(ref on) = parsed.on {
        if let Some(ref manual) = on.manual {
            if let Some(ref inputs) = manual.inputs {
                for (name, def) in inputs {
                    let ty = def.r#type.as_deref().unwrap_or("").trim();
                    let has_options = def.options.as_deref().is_some_and(|o| !o.is_empty());
                    let field = format!("on.manual.inputs.{name}");
                    if has_options && ty != "choice" {
                        warnings.push(format!(
                            "{field}: 'options' requires 'type: choice' to take effect — currently ignored"
                        ));
                    } else if ty.is_empty() {
                        warnings.push(format!(
                            "{field}: no 'type' declared — defaults to 'string'; declare 'type:' to enable number/boolean/host/choice validation and matching UI control"
                        ));
                    }
                }
            }
        }
    }

    // 4. Detect trigger type (single source of truth for UI auto-fill)
    if let Some(ref on) = parsed.on {
        let trigger_count = [on.manual.is_some(), on.webhook.is_some(), on.cron.is_some()]
            .iter()
            .filter(|&&b| b)
            .count();

        if trigger_count > 1 {
            warnings.push("Multiple trigger types defined — only the first will be used".into());
        }

        // 4b. on.webhook.secret — bare team secret name enabling HMAC verification
        if let Some(secret) = on
            .webhook
            .as_ref()
            .and_then(|w| w.secret.as_deref())
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            if let Err(msg) = crate::secret_kind::validate_secret_name(secret) {
                errors.push(YamlValidationError {
                    field: "on.webhook.secret".into(),
                    message: msg,
                });
            }
        }
    }

    // 5. Concurrency mode + queue_timeout validation
    if let Some(ConcurrencyDef::Full(ref fc)) = parsed.concurrency {
        let valid_modes = ["discard-old", "queue", "discard-new"];
        if !valid_modes.contains(&fc.mode.as_str()) {
            errors.push(YamlValidationError {
                field: "concurrency.mode".into(),
                message: format!(
                    "invalid concurrency mode '{}' — must be one of: {}",
                    fc.mode,
                    valid_modes.join(", ")
                ),
            });
        }
        validate_duration_field(
            "concurrency.queue_timeout",
            fc.queue_timeout.as_deref(),
            &mut errors,
        );
    }

    // 5b. runner.timeout / step.timeout — reject garbage like `10x m`
    validate_duration_field(
        "runner.timeout",
        parsed.runner.as_ref().and_then(|r| r.timeout.as_deref()),
        &mut errors,
    );
    for (i, step) in parsed.steps.iter().enumerate() {
        validate_duration_field(
            &format!("steps[{i}].timeout"),
            step.timeout.as_deref(),
            &mut errors,
        );
    }

    // 6. SSH step requires host; validate hosts_strategy
    for (i, step) in parsed.steps.iter().enumerate() {
        let engine = step.engine.as_deref().unwrap_or("").trim();
        if engine == "ssh"
            && step.host.as_ref().is_none_or(|h| h.is_empty())
            && step.host_labels.as_ref().is_none_or(|h| h.is_empty())
        {
            errors.push(YamlValidationError {
                field: format!("steps[{}]", i),
                message: "SSH execution requires 'host' or 'host_labels' field".into(),
            });
        }
        if let Some(raw) = step.hosts_strategy.as_deref() {
            let trimmed = raw.trim();
            if !trimmed.is_empty() {
                if engine != "ssh" {
                    errors.push(YamlValidationError {
                        field: format!("steps[{i}].hosts_strategy"),
                        message: "hosts_strategy is only valid for engine: ssh".into(),
                    });
                } else if let Err(msg) = normalize_hosts_strategy(Some(trimmed)) {
                    errors.push(YamlValidationError {
                        field: format!("steps[{i}].hosts_strategy"),
                        message: msg,
                    });
                }
            }
        }
    }

    // 6c. notify channels — type whitelist, webhook secret slot, on events
    const VALID_NOTIFY_TYPES: &[&str] = &["feishu"];
    const VALID_NOTIFY_ON: &[&str] = &["success", "failure", "cancelled"];
    if let Some(channels) = &parsed.notify {
        for (i, ch) in channels.iter().enumerate() {
            let field = format!("notify[{i}]");
            let ty = ch.channel_type.trim();
            if ty.is_empty() || !VALID_NOTIFY_TYPES.contains(&ty) {
                errors.push(YamlValidationError {
                    field: format!("{field}.type"),
                    message: format!(
                        "invalid notify type '{}' — must be one of: {}",
                        ch.channel_type,
                        VALID_NOTIFY_TYPES.join(", ")
                    ),
                });
            }
            let webhook = ch.webhook.trim();
            if webhook.is_empty() {
                errors.push(YamlValidationError {
                    field: format!("{field}.webhook"),
                    message: "notify webhook is required".into(),
                });
            } else if crate::secret_ref::resolve_secret_slot(webhook).is_none() {
                errors.push(YamlValidationError {
                    field: format!("{field}.webhook"),
                    message: "notify webhook must be '${secrets.NAME}' (or bare secret name)"
                        .into(),
                });
            }
            if let Some(on) = &ch.on {
                for (j, ev) in on.iter().enumerate() {
                    let e = ev.trim();
                    if e.is_empty() || !VALID_NOTIFY_ON.contains(&e) {
                        errors.push(YamlValidationError {
                            field: format!("{field}.on[{j}]"),
                            message: format!(
                                "invalid notify event '{ev}' — must be one of: {}",
                                VALID_NOTIFY_ON.join(", ")
                            ),
                        });
                    }
                }
            }
        }
    }

    // 6. Docker engine rules
    for (i, step) in parsed.steps.iter().enumerate() {
        let engine = step.engine.as_deref().unwrap_or("").trim();
        let field = format!("steps[{i}]");

        // privileged is only meaningful for docker
        if engine != "docker" && step.privileged.unwrap_or(false) {
            errors.push(YamlValidationError {
                field: field.clone(),
                message: "'privileged' is only valid for docker engine steps".into(),
            });
        }

        let reg_secret = step
            .registry_secret
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        if reg_secret.is_some() {
            let engine = if engine.is_empty() { "shell" } else { engine };
            if !matches!(engine, "shell" | "local" | "ssh" | "docker") {
                errors.push(YamlValidationError {
                    field: field.clone(),
                    message: "'registry_secret' is only valid for shell, ssh, or docker steps"
                        .into(),
                });
            }
        }

        // Reject legacy flat config fields — nested `config:` only.
        let legacy_flat = step
            .config_source
            .as_deref()
            .is_some_and(|s| !s.trim().is_empty())
            || step
                .config_path
                .as_deref()
                .is_some_and(|s| !s.trim().is_empty())
            || step
                .config_dir
                .as_deref()
                .is_some_and(|s| !s.trim().is_empty())
            || step.env_render.as_ref().is_some_and(|v| !v.is_null());
        if legacy_flat {
            errors.push(YamlValidationError {
                field: field.clone(),
                message: "flat 'config_source'/'config_path'/'config_dir'/'env_render' are no longer supported — use nested 'config:' (see docs: Config Block DSL v2)".into(),
            });
        }

        if let Some(cfg) = &step.config {
            validate_config_block(cfg, engine, &field, &mut errors);
        }

        // Scripts do not expand `${secrets.…}` — require explicit `env:` mapping.
        if let Some(run) = step.run.as_deref() {
            if crate::secret_ref::run_contains_secrets_expr(run) {
                errors.push(YamlValidationError {
                    field: field.clone(),
                    message: "'run' must not contain '${secrets.…}' — map via env: \
                              (e.g. FOO: ${secrets.NAME}) then use $FOO in the script"
                        .into(),
                });
            }
        }

        if engine != "docker" {
            continue;
        }
        let image = step.image.as_deref().unwrap_or("").trim();
        if image.is_empty() {
            errors.push(YamlValidationError {
                field: field.clone(),
                message: "Docker step requires non-empty 'image'".into(),
            });
        }
        if step.host.as_ref().is_some_and(|h| !h.is_empty())
            || step.host_labels.as_ref().is_some_and(|h| !h.is_empty())
        {
            errors.push(YamlValidationError {
                field,
                message: "Docker step must not set 'host' or 'host_labels' (SSH-only)".into(),
            });
        }
    }

    let valid = errors.is_empty();

    // Resolve the concurrency string for UI auto-fill
    let (concurrency_group, concurrency_config) = match &parsed.concurrency {
        Some(ConcurrencyDef::Simple(g)) => (Some(g.clone()), None),
        Some(ConcurrencyDef::Full(fc)) => {
            let group = fc.group.clone();
            let config = ConcurrencyConfig {
                group: fc.group.clone(),
                mode: Some(fc.mode.clone()),
                max_queue: Some(fc.max_queue),
                queue_timeout: fc.queue_timeout.clone(),
            };
            (Some(group), Some(config))
        }
        None => (None, None),
    };

    // Build the parsed data snapshot for frontend import-to-form
    let notify_data = parsed.notify.as_ref().map(|channels| {
        channels
            .iter()
            .map(|ch| NotifyChannelData {
                channel_type: ch.channel_type.clone(),
                webhook: ch.webhook.clone(),
                on: ch.on.clone(),
            })
            .collect::<Vec<_>>()
    });

    let parsed_data = PipelineYamlData {
        environments: parsed.environments.clone(),
        description: parsed.description.clone(),
        concurrency: concurrency_group,
        concurrency_config,
        env: parsed.env.clone(),
        variables: parsed.variables.clone(),
        secrets: parsed.secrets.clone(),
        notify: notify_data,
        runner: parsed.runner.as_ref().map(|r| RunnerConfig {
            labels: r.labels.clone(),
            timeout: r.timeout.clone(),
        }),
        trigger_type: Some(
            parsed
                .on
                .as_ref()
                .map(|on| {
                    if on.webhook.is_some() {
                        "webhook"
                    } else if on.cron.is_some() {
                        "cron"
                    } else {
                        "manual"
                    }
                })
                .unwrap_or("manual")
                .to_string(),
        ),
        trigger_config: parsed.on.as_ref().map(|on| TriggerConfig {
            webhook_path: on.webhook.as_ref().and_then(|w| w.path.clone()),
            webhook_environment: on.webhook.as_ref().and_then(|w| w.environment.clone()),
            cron_expr: on.cron.as_ref().and_then(|c| c.schedule.clone()),
            cron_environment: on.cron.as_ref().and_then(|c| c.environment.clone()),
            inputs: on.manual.as_ref().and_then(|m| {
                m.inputs.as_ref().map(|inputs| {
                    inputs
                        .iter()
                        .map(|(k, v)| InputData {
                            name: k.clone(),
                            description: v.description.clone(),
                            required: v.required,
                            default: v.default.clone(),
                            masking: v.masking.filter(|m| *m),
                            input_type: v.r#type.clone(),
                            pattern: v.pattern.clone(),
                            options: v.options.clone(),
                        })
                        .collect()
                })
            }),
        }),
        steps: parsed
            .steps
            .iter()
            .map(|s| {
                let (config_source, config_key_path, config_dest, config_render) =
                    flatten_config(s.config.as_ref());
                StepData {
                    name: s.name.clone(),
                    run: s.run.clone(),
                    engine: s.engine.clone(),
                    image: s.image.clone(),
                    volumes: s.volumes.as_ref().map(|v| v.join(",")),
                    host: s.host.as_ref().map(|h| match h {
                        OneOrMany::One(s) => s.clone(),
                        OneOrMany::Many(v) => v.join(","),
                    }),
                    host_labels: s.host_labels.as_ref().map(|h| match h {
                        OneOrMany::One(s) => s.clone(),
                        OneOrMany::Many(v) => v.join(","),
                    }),
                    hosts_strategy: s
                        .hosts_strategy
                        .as_deref()
                        .map(str::trim)
                        .filter(|v| !v.is_empty())
                        .map(|v| {
                            normalize_hosts_strategy(Some(v))
                                .unwrap_or("chain")
                                .to_string()
                        }),
                    dir: s.dir.clone(),
                    timeout: s.timeout.clone(),
                    retry: s.retry,
                    continue_on_error: s.continue_on_error,
                    secrets: s.secrets.clone(),
                    env: s.env.clone(),
                    privileged: s.privileged,
                    registry_secret: s
                        .registry_secret
                        .as_deref()
                        .map(str::trim)
                        .filter(|v| !v.is_empty())
                        .map(str::to_string),
                    config: s.config.clone(),
                    config_source,
                    config_key_path,
                    config_dest,
                    config_render,
                }
            })
            .collect(),
    };

    YamlValidationResult {
        valid,
        name: Some(name.to_string()),
        errors,
        warnings,
        parsed: Some(parsed_data),
    }
}

#[cfg(test)]
mod step_yaml_conversion_tests {
    use super::StepYaml;
    use crate::protocol::StepDef;

    #[test]
    fn conversion_preserves_step_fields_and_flattens_config() {
        let parsed: StepYaml = serde_yaml::from_str(
            r#"
name: Upload artifact
run: echo ok
engine: docker
image: example/image:1
volumes: ["/tmp:/tmp"]
privileged: true
registry_secret: ${secrets.REGISTRY_AUTH}
dir: /workspace
timeout: 5m
retry: 2
continue-on-error: true
secrets: [UPLOAD_TOKEN]
env:
  REGION: cn-hangzhou
config:
  source: ${secrets.OSS_AUTH}
  key_path: releases/app
  dest: /opt/app
  render:
    template: app.env.tpl
    output: app.env
"#,
        )
        .unwrap();

        let step = StepDef::from(&parsed);
        assert_eq!(step.name, "Upload artifact");
        assert_eq!(step.run, "echo ok");
        assert_eq!(step.engine.as_deref(), Some("docker"));
        assert_eq!(step.image.as_deref(), Some("example/image:1"));
        assert_eq!(
            step.volumes.as_deref(),
            Some(&["/tmp:/tmp".to_string()][..])
        );
        assert_eq!(step.privileged, Some(true));
        assert_eq!(
            step.registry_secret.as_deref(),
            Some("${secrets.REGISTRY_AUTH}")
        );
        assert_eq!(step.dir.as_deref(), Some("/workspace"));
        assert_eq!(step.timeout.as_deref(), Some("5m"));
        assert_eq!(step.retry, Some(2));
        assert_eq!(step.continue_on_error, Some(true));
        assert_eq!(
            step.secrets.as_deref(),
            Some(&["UPLOAD_TOKEN".to_string()][..])
        );
        assert_eq!(
            step.env.as_ref().unwrap().get("REGION").unwrap(),
            "cn-hangzhou"
        );
        assert_eq!(step.config_source.as_deref(), Some("${secrets.OSS_AUTH}"));
        assert_eq!(step.config_key_path.as_deref(), Some("releases/app"));
        assert_eq!(step.config_dest.as_deref(), Some("/opt/app"));
        let render = step.config_render.as_ref().unwrap();
        assert_eq!(render.len(), 1);
        assert_eq!(render[0].template.as_deref(), Some("app.env.tpl"));
        assert_eq!(render[0].output.as_deref(), Some("app.env"));
    }
}
