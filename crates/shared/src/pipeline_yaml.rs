use serde::{Deserialize, Serialize};

use crate::protocol::OneOrMany;

/// Buz pipeline definition in YAML format (simplified GitHub Actions style).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineYaml {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub concurrency: Option<String>,
    #[serde(default)]
    pub on: Option<TriggerYaml>,
    #[serde(default)]
    pub runner: Option<RunnerYaml>,
    #[serde(default)]
    pub env: Option<std::collections::BTreeMap<String, String>>,
    #[serde(default)]
    pub secrets: Option<Vec<String>>,
    pub steps: Vec<StepYaml>,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepYaml {
    pub name: Option<String>,
    #[serde(default)]
    pub run: Option<String>,
    #[serde(default)]
    pub uses: Option<String>,
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
pub struct InputData {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
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
            warnings.push(format!("steps[{}] has no 'name' — consider adding one for readability", i));
        }
    }

    // 4. Detect trigger type (single source of truth for UI auto-fill)
    if let Some(ref on) = parsed.on {
        let trigger_count = [
            on.manual.is_some(),
            on.webhook.is_some(),
            on.cron.is_some(),
        ]
        .iter()
        .filter(|&&b| b)
        .count();

        if trigger_count > 1 {
            warnings.push("Multiple trigger types defined — only the first will be used".into());
        }
    }

    // 5. SSH step requires host
    for (i, step) in parsed.steps.iter().enumerate() {
        let uses = step.uses.as_deref().unwrap_or("").trim();
        if uses == "ssh" && step.host.as_ref().map_or(true, |h| h.is_empty()) {
            errors.push(YamlValidationError {
                field: format!("steps[{}]", i),
                message: "SSH execution requires 'host' field".into(),
            });
        }
    }

    let valid = errors.is_empty();

    if valid {
        warnings.push("YAML syntax and structure looks good".into());
    }

    // Build the parsed data snapshot for frontend import-to-form
    let parsed_data = PipelineYamlData {
        description: parsed.description.clone(),
        concurrency: parsed.concurrency.clone(),
        runner: parsed.runner.as_ref().map(|r| RunnerConfig {
            labels: r.labels.clone(),
            timeout: r.timeout.clone(),
        }),
        trigger_type: parsed.on.as_ref().map(|on| {
            if on.manual.is_some() { "manual" }
            else if on.webhook.is_some() { "webhook" }
            else if on.cron.is_some() { "cron" }
            else { "manual" }
        }).map(|s| s.to_string()),
        trigger_config: parsed.on.as_ref().map(|on| TriggerConfig {
            webhook_path: on.webhook.as_ref().and_then(|w| w.path.clone()),
            cron_expr: on.cron.as_ref().and_then(|c| c.schedule.clone()),
            inputs: on.manual.as_ref().and_then(|m| {
                m.inputs.as_ref().map(|inputs| {
                    inputs.iter().map(|(k, v)| InputData {
                        name: k.clone(),
                        description: v.description.clone(),
                        required: v.required,
                        default: v.default.clone(),
                    }).collect()
                })
            }),
        }),
        steps: parsed.steps.iter().map(|s| StepData {
            name: s.name.clone(),
            run: s.run.clone(),
            uses: s.uses.clone(),
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
        }).collect(),
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
mod tests {
    use super::*;

    #[test]
    fn valid_minimal_yaml() {
        let yaml = r#"
name: hello-world
steps:
  - name: greet
    run: echo hello
"#;
        let r = parse_and_validate(yaml);
        assert!(r.valid);
        assert_eq!(r.errors.len(), 0);
    }

    #[test]
    fn missing_name() {
        let yaml = r#"
name: ""
steps:
  - run: echo hi
"#;
        let r = parse_and_validate(yaml);
        assert!(!r.valid);
        assert!(r.errors.iter().any(|e| e.field == "name"));
    }

    #[test]
    fn empty_steps() {
        let yaml = "name: test\nsteps: []\n";
        let r = parse_and_validate(yaml);
        assert!(!r.valid);
    }

    #[test]
    fn step_missing_command() {
        let yaml = r#"
name: test
steps:
  - name: noop
"#;
        let r = parse_and_validate(yaml);
        assert!(!r.valid);
    }

    #[test]
    fn ssh_without_host() {
        let yaml = r#"
name: deploy
steps:
  - name: push
    uses: ssh
    run: docker push
"#;
        let r = parse_and_validate(yaml);
        assert!(!r.valid);
    }

    #[test]
    fn ssh_multi_host() {
        let yaml = r#"
name: multi-deploy
steps:
  - name: push
    uses: ssh
    host:
      - prod-01
      - prod-02
    run: docker push
"#;
        let r = parse_and_validate(yaml);
        assert!(r.valid, "Errors: {:?}", r.errors);
    }

    #[test]
    fn full_example() {
        let yaml = r#"
name: Deploy Production
description: Build and deploy to production
on:
  manual:
    inputs:
      version:
        description: Version to deploy
        required: true
        default: latest
concurrency: deploy-prod
runner:
  labels: [prod, linux-x64]
  timeout: 60m
env:
  APP_NAME: myapp
secrets:
  - DOCKER_PASSWORD
steps:
  - name: checkout
    run: git pull origin main
  - name: build
    run: docker build -t ${APP_NAME} .
    timeout: 10m
  - name: deploy
    uses: ssh
    host: prod-01
    run: docker-compose up -d
    retry: 1
"#;
        let r = parse_and_validate(yaml);
        assert!(r.valid, "Errors: {:?}", r.errors);
    }
}
