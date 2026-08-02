use serde::{Deserialize, Serialize};

use crate::protocol::OneOrMany;

/// Buz pipeline definition in YAML format (simplified GitHub Actions style).
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    /// Resolved to `teams.id` by the YAML scanner; defaults to `default` if omitted.
    #[serde(default, alias = "owner_group")]
    pub team: Option<String>,
    #[serde(default)]
    pub steps: Vec<StepYaml>,
}

/// Concurrency definition: string (legacy) or full object.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ConcurrencyDef {
    /// Legacy: `concurrency: "my-group"` — backward compat, equivalent to discard-new.
    Simple(String),
    /// New: `concurrency: { group: "my-group", mode: "queue", ... }`
    Full(ConcurrencyYaml),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConcurrencyYaml {
    pub group: String,
    #[serde(default = "default_concurrency_mode")]
    pub mode: String, // "discard-old" (default) | "queue" | "discard-new"
    #[serde(default = "default_max_queue")]
    pub max_queue: u32,
    #[serde(default)]
    pub queue_timeout: Option<String>,
}

pub fn default_concurrency_mode() -> String {
    "discard-old".into()
}
fn default_max_queue() -> u32 {
    3
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TriggerYaml {
    #[serde(default)]
    pub manual: Option<ManualTriggerYaml>,
    #[serde(default)]
    pub webhook: Option<WebhookTriggerYaml>,
    #[serde(default)]
    pub cron: Option<CronTriggerYaml>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManualTriggerYaml {
    #[serde(default)]
    pub inputs: Option<std::collections::BTreeMap<String, InputDef>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputDef {
    pub description: Option<String>,
    pub required: Option<bool>,
    #[serde(default)]
    pub default: Option<String>,
    /// When true, values are redacted in logs, audit, and job snapshots (still injected as env at runtime).
    #[serde(default)]
    pub masking: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookTriggerYaml {
    #[serde(default)]
    pub path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CronTriggerYaml {
    #[serde(default)]
    pub schedule: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
#[serde(rename_all = "snake_case")]
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
}

/// Legacy alias — prefer [`ConfigRenderEntry`].
pub type EnvRender = ConfigRenderEntry;

/// `config.render`: a single object or a list of objects.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum ConfigRender {
    One(ConfigRenderEntry),
    Many(Vec<ConfigRenderEntry>),
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
#[serde(rename_all = "snake_case")]
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
pub struct StepYaml {
    pub name: Option<String>,
    #[serde(default)]
    pub run: Option<String>,
    #[serde(default)]
    pub uses: Option<String>,
    /// Container image when `uses: docker`.
    #[serde(default)]
    pub image: Option<String>,
    /// Extra host→container bind mounts (only for uses: docker). Each entry is "host_path:container_path".
    #[serde(default)]
    pub volumes: Option<Vec<String>>,
    /// Run the container in privileged mode (only for uses: docker).
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
    #[serde(default, alias = "host_labels")]
    pub host_labels: Option<OneOrMany>,
    #[serde(default)]
    pub timeout: Option<String>,
    #[serde(default)]
    pub retry: Option<i32>,
    #[serde(default, rename = "continue-on-error")]
    pub continue_on_error: Option<bool>,
    /// Secrets referenced by this step — resolved at runtime from the Manager.
    /// These are IN ADDITION to any pipeline-level secrets.
    #[serde(default)]
    pub secrets: Option<Vec<String>>,
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
    pub steps: Vec<StepData>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TriggerConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub webhook_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cron_expr: Option<String>,
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
}

#[derive(Debug, Clone, Serialize)]
pub struct StepData {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uses: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    /// Comma-separated host:container volume mounts (for frontend import).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub volumes: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host_labels: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub continue_on_error: Option<bool>,
    /// Step-level secret names (in addition to pipeline-level secrets).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secrets: Option<Vec<String>>,
    /// Privileged mode flag (only for docker steps).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub privileged: Option<bool>,
    /// Manager Secret name for private registry login (docker / shell / SSH).
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

/// Validate a nested `config:` block (deliver / render-only / sugar).
fn validate_config_block(
    cfg: &ConfigBlock,
    uses: &str,
    field: &str,
    errors: &mut Vec<YamlValidationError>,
    warnings: &mut Vec<String>,
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
            message: "'config.source' and 'config.key_path' must be set together (OSS deliver)".into(),
        });
    }

    let is_deliver = source.is_some() && key_path.is_some();
    let is_render_only = !is_deliver && has_render;
    let is_sugar = is_deliver && has_render;

    if is_deliver || is_sugar {
        let engine = if uses.is_empty() { "shell" } else { uses };
        if !matches!(engine, "shell" | "local" | "ssh" | "docker") {
            errors.push(YamlValidationError {
                field: field.into(),
                message: "'config' OSS deliver is only valid for shell, local, ssh, or docker steps"
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
            if !p.ends_with('/') && !p.ends_with("configs.tar.gz") {
                warnings.push(format!(
                    "{field}: 'config.key_path' should be a directory prefix (Manager appends 'configs.tar.gz')"
                ));
            }
        }
    }

    if is_render_only || is_sugar {
        let engine = if uses.is_empty() { "shell" } else { uses };
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
    if !is_deliver && !has_render {
        if dest.is_some() {
            errors.push(YamlValidationError {
                field: field.into(),
                message: "'config.dest' alone is not enough — add OSS deliver (source+key_path) or render".into(),
            });
        }
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
pub fn flatten_config(cfg: Option<&ConfigBlock>) -> (
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

/// Parse a pipeline YAML string and run validation checks.
pub fn parse_and_validate(raw: &str) -> YamlValidationResult {
    let mut errors: Vec<YamlValidationError> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();

    let parsed: PipelineYaml = match serde_yaml::from_str(raw) {
        Ok(p) => p,
        Err(e) => {
            return YamlValidationResult {
                valid: false,
                name: None,
                errors: vec![YamlValidationError {
                    field: "yaml".into(),
                    message: format!("YAML syntax error: {e}"),
                }],
                warnings: vec![],
                parsed: None,
            };
        }
    };

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

    // 3. Each step must have either `run` or `uses`
    for (i, step) in parsed.steps.iter().enumerate() {
        let run = step.run.as_deref().unwrap_or("").trim();
        let uses = step.uses.as_deref().unwrap_or("").trim();

        if run.is_empty() && uses.is_empty() {
            errors.push(YamlValidationError {
                field: format!("steps[{}]", i),
                message: "Step must define 'run' or 'uses'".into(),
            });
        }

        if step.name.as_deref().unwrap_or("").trim().is_empty() {
            warnings.push(format!(
                "steps[{}] has no 'name' — consider adding one for readability",
                i
            ));
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
    }

    // 5. Concurrency mode validation
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
    }

    // 6. SSH step requires host
    for (i, step) in parsed.steps.iter().enumerate() {
        let uses = step.uses.as_deref().unwrap_or("").trim();
        if uses == "ssh"
            && step.host.as_ref().is_none_or(|h| h.is_empty())
            && step.host_labels.as_ref().is_none_or(|h| h.is_empty())
        {
            errors.push(YamlValidationError {
                field: format!("steps[{}]", i),
                message: "SSH execution requires 'host' or 'host_labels' field".into(),
            });
        }
    }

    // 6. Docker engine rules
    for (i, step) in parsed.steps.iter().enumerate() {
        let uses = step.uses.as_deref().unwrap_or("").trim();
        let field = format!("steps[{i}]");

        // privileged is only meaningful for docker
        if uses != "docker" && step.privileged.unwrap_or(false) {
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
            let engine = if uses.is_empty() { "shell" } else { uses };
            if !matches!(engine, "shell" | "local" | "ssh" | "docker") {
                errors.push(YamlValidationError {
                    field: field.clone(),
                    message: "'registry_secret' is only valid for shell, ssh, or docker steps"
                        .into(),
                });
            }
        }

        // Reject legacy flat config fields — nested `config:` only.
        let legacy_flat = step.config_source.as_deref().is_some_and(|s| !s.trim().is_empty())
            || step.config_path.as_deref().is_some_and(|s| !s.trim().is_empty())
            || step.config_dir.as_deref().is_some_and(|s| !s.trim().is_empty())
            || step.env_render.as_ref().is_some_and(|v| !v.is_null());
        if legacy_flat {
            errors.push(YamlValidationError {
                field: field.clone(),
                message: "flat 'config_source'/'config_path'/'config_dir'/'env_render' are no longer supported — use nested 'config:' (see docs: Config Block DSL v2)".into(),
            });
        }

        if let Some(cfg) = &step.config {
            validate_config_block(cfg, uses, &field, &mut errors, &mut warnings);
        }

        if uses != "docker" {
            continue;
        }
        let run = step.run.as_deref().unwrap_or("").trim();
        if run.is_empty() {
            errors.push(YamlValidationError {
                field: field.clone(),
                message: "Docker step requires 'run'".into(),
            });
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

    if valid {
        warnings.push("YAML syntax and structure looks good".into());
    }

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
    let parsed_data = PipelineYamlData {
        description: parsed.description.clone(),
        concurrency: concurrency_group,
        concurrency_config,
        env: parsed.env.clone(),
        variables: parsed.variables.clone(),
        secrets: parsed.secrets.clone(),
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
            cron_expr: on.cron.as_ref().and_then(|c| c.schedule.clone()),
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
                    uses: s.uses.clone(),
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
                    dir: s.dir.clone(),
                    timeout: s.timeout.clone(),
                    retry: s.retry,
                    continue_on_error: s.continue_on_error,
                    secrets: s.secrets.clone(),
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
