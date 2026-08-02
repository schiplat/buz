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

/// Declarative `.env` generation for `config_source` steps.
///
/// Runs after the config bundle is unpacked (into `$BUZ_CONFIG_DIR`) and before
/// the run block. The template is resolved against the job's dispatched
/// secrets (`${secrets.X}`, by logical name) and vars (`${vars.X}`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct EnvRender {
    /// Template file relative to `$BUZ_CONFIG_DIR` (e.g. `env.example`).
    #[serde(default)]
    pub template: Option<String>,
    /// Output file relative to `$BUZ_CONFIG_DIR`. Defaults to
    /// `<template>.rendered` when unset.
    #[serde(default)]
    pub output: Option<String>,
    /// Output file mode (e.g. `0600`).
    #[serde(default)]
    pub mode: Option<String>,
    /// `fail` (default) aborts the render on any unresolved reference;
    /// `warn` writes `KEY=` with a `# WARNING` comment (rollback deploys).
    #[serde(default)]
    pub on_missing: Option<String>,
    /// `recreate` force-recreates the listed `services` when a template-defined
    /// key changed (Phase 2; parsed but not yet enforced).
    #[serde(default)]
    pub on_change: Option<String>,
    /// Compose services affected by this `.env` (Phase 2).
    #[serde(default)]
    pub services: Option<Vec<String>>,
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
    /// Name of a Manager Secret (kind=`oss`) holding Aliyun OSS read-only AK/SK.
    /// Manager signs a short-lived GET URL for `config_path` and injects it
    /// into the step env as `$BUZ_CONFIG_URL` (shell / ssh steps).
    #[serde(default)]
    pub config_source: Option<String>,
    /// OSS object key (directory) to download, e.g. `config_tpl/<project>/<tag>`.
    /// Manager appends `configs.tar.gz` when signing the URL.
    #[serde(default)]
    pub config_path: Option<String>,
    /// Target directory where Buz unpacks the config bundle (exposed to the run
    /// block as `$BUZ_CONFIG_DIR`). SSH steps: on the target host; shell steps:
    /// on the runner. Defaults to a per-job temp dir when unset.
    #[serde(default)]
    pub config_dir: Option<String>,
    /// Declarative `.env` rendering after unpack, before the run block.
    #[serde(default)]
    pub env_render: Option<EnvRender>,
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
    /// Manager Secret name (kind=`oss`) for signed OSS config downloads.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config_source: Option<String>,
    /// OSS object key (directory) — Manager appends `configs.tar.gz`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config_path: Option<String>,
    /// Target unpack directory for `config_source`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config_dir: Option<String>,
    /// Declarative `.env` rendering after unpack.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env_render: Option<EnvRender>,
}

#[derive(Debug, Clone, Serialize)]
pub struct YamlValidationError {
    pub field: String,
    pub message: String,
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

        let cfg_source = step
            .config_source
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let cfg_path = step
            .config_path
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        if cfg_source.is_some() || cfg_path.is_some() {
            let engine = if uses.is_empty() { "shell" } else { uses };
            if !matches!(engine, "shell" | "local" | "ssh" | "docker") {
                errors.push(YamlValidationError {
                    field: field.clone(),
                    message: "'config_source'/'config_path' are only valid for shell, ssh, or docker steps"
                        .into(),
                });
            }
            if cfg_source.is_some() != cfg_path.is_some() {
                errors.push(YamlValidationError {
                    field: field.clone(),
                    message: "'config_source' and 'config_path' must be set together".into(),
                });
            }
            if let Some(p) = cfg_path {
                if !p.ends_with('/') && !p.ends_with("configs.tar.gz") {
                    warnings.push(format!(
                        "{field}: 'config_path' should be a directory (Manager appends 'configs.tar.gz')"
                    ));
                }
            }
        }

        // env_render / config_dir build on config_source: the template lives in
        // the unpacked bundle, so both require the OSS delivery to be present.
        if step.env_render.is_some() || step.config_dir.is_some() {
            let engine = if uses.is_empty() { "shell" } else { uses };
            if !matches!(engine, "shell" | "local" | "ssh") {
                errors.push(YamlValidationError {
                    field: field.clone(),
                    message:
                        "'env_render'/'config_dir' are only valid for shell, local, or ssh steps"
                            .into(),
                });
            }
            if cfg_source.is_none() {
                errors.push(YamlValidationError {
                    field: field.clone(),
                    message: "'env_render'/'config_dir' require 'config_source' (the template lives in the unpacked config bundle)"
                        .into(),
                });
            }
        }
        if let Some(er) = &step.env_render {
            let template = er.template.as_deref().map(str::trim).unwrap_or("");
            if template.is_empty() {
                errors.push(YamlValidationError {
                    field: field.clone(),
                    message: "'env_render.template' is required (relative to $BUZ_CONFIG_DIR, e.g. env.example)"
                        .into(),
                });
            }
            if let Some(om) = er
                .on_missing
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                if !matches!(om, "fail" | "warn") {
                    errors.push(YamlValidationError {
                        field: field.clone(),
                        message: "'env_render.on_missing' must be 'fail' or 'warn'".into(),
                    });
                }
            }
            if let Some(oc) = er
                .on_change
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                if !matches!(oc, "recreate") {
                    errors.push(YamlValidationError {
                        field: field.clone(),
                        message: "'env_render.on_change' must be 'recreate'".into(),
                    });
                }
            }
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
            .map(|s| StepData {
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
                config_source: s
                    .config_source
                    .as_deref()
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
                    .map(str::to_string),
                config_path: s
                    .config_path
                    .as_deref()
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
                    .map(str::to_string),
                config_dir: s
                    .config_dir
                    .as_deref()
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
                    .map(str::to_string),
                env_render: s.env_render.clone(),
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
