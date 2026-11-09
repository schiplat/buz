//! YAML generation for pipeline retirement of the form storage model.
//!
//! Two producers, one consumer shape: both assemble a
//! [`shared::pipeline_yaml::PipelineYaml`] and serialize it through
//! `serde_yaml`, then assert it passes `parse_and_validate` — the generator
//! cannot emit a file the schema would reject.
//!
//! - [`emit_from_request`] — `POST`/`PUT /pipelines` payloads. `uses:`/
//!   `with:` are preserved verbatim (the scanner re-resolves and locks).
//! - [`yaml_from_row_parts`] — the startup form→YAML migration. Steps come
//!   from the expanded `tasks` projection; `uses:` is reconstructed from
//!   `step_resolutions` (ref + resolved version + `INPUT_*` values) so
//!   template linkage and provenance survive the migration.

use std::collections::{BTreeMap, HashMap};

use shared::error::AppError;
use shared::pipeline_yaml::{
    parse_and_validate, CronTriggerYaml, InputDef, ManualTriggerYaml, NotifyChannelYaml,
    PipelineYaml, RunnerYaml, StepYaml, TriggerYaml, WebhookTriggerYaml,
};
use shared::protocol::CreatePipelineRequest;
use shared::protocol::OneOrMany;

/// Serialize + schema-validate. The double-check is deliberate: the generator
/// must never write a file the scanner would refuse (the migration would
/// otherwise leave the pipeline unmigrated forever).
pub(crate) fn emit_and_validate(pipeline: &PipelineYaml) -> Result<String, AppError> {
    let raw = serde_yaml::to_string(pipeline)
        .map_err(|error| AppError::Internal(format!("cannot serialize pipeline YAML: {error}")))?;
    let result = parse_and_validate(&raw);
    if !result.valid {
        let detail = result
            .errors
            .iter()
            .map(|error| format!("{}: {}", error.field, error.message))
            .collect::<Vec<_>>()
            .join("; ");
        return Err(AppError::ValidationFailed(format!(
            "generated pipeline YAML failed validation — {detail}"
        )));
    }
    Ok(raw)
}

fn seconds(raw: i64) -> String {
    format!("{raw}s")
}

fn description_field(description: &Option<String>) -> Option<String> {
    description
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

/// Emit YAML from a create/update request payload.
///
/// `secret_names` / `var_names` map binding IDs to their (same-team) names —
/// YAML references names, the request carries IDs. Missing IDs are skipped
/// with a warning (create validates bindings beforehand; migration may hit
/// rows whose binding was deleted).
pub fn emit_from_request(
    request: &CreatePipelineRequest,
    team_slug: &str,
    secret_names: &HashMap<String, String>,
    var_names: &HashMap<String, String>,
) -> Result<String, AppError> {
    // Inputs belong to the chosen trigger; emit them on manual / webhook / cron
    // alike so a YAML round-trip never drops declarations.
    let declared_inputs: Option<BTreeMap<String, InputDef>> = if request
        .input_defs
        .as_ref()
        .is_some_and(|defs| !defs.is_empty())
    {
        Some(
            request
                .input_defs
                .as_ref()
                .unwrap_or(&Vec::new())
                .iter()
                .map(|def| {
                    (
                        def.name.clone(),
                        InputDef {
                            description: def.description.clone(),
                            required: def.required,
                            default: def.default.clone(),
                            masking: def.masking.filter(|mask| *mask),
                            r#type: def.input_type.clone(),
                            pattern: def.pattern.clone(),
                            options: def.options.clone(),
                        },
                    )
                })
                .collect(),
        )
    } else {
        None
    };
    let manual_inputs = if request.trigger_type == "manual" {
        declared_inputs.clone()
    } else {
        None
    };

    let on = Some(match request.trigger_type.as_str() {
        "webhook" => TriggerYaml {
            manual: None,
            webhook: Some(WebhookTriggerYaml {
                path: request.webhook_path.clone(),
                inputs: declared_inputs.clone(),
                secret: request
                    .webhook_secret
                    .as_deref()
                    .map(str::trim)
                    .filter(|name| !name.is_empty())
                    .map(str::to_string),
                environment: request.webhook_environment.clone(),
            }),
            cron: None,
        },
        "cron" => TriggerYaml {
            manual: None,
            webhook: None,
            cron: Some(CronTriggerYaml {
                schedule: request.cron_expr.clone(),
                inputs: declared_inputs.clone(),
                environment: request.cron_environment.clone(),
            }),
        },
        _ => TriggerYaml {
            manual: Some(ManualTriggerYaml {
                inputs: manual_inputs,
            }),
            webhook: None,
            cron: None,
        },
    });

    let env: Option<BTreeMap<String, String>> = match request.env.as_ref() {
        Some(pairs) if !pairs.is_empty() => Some(
            pairs
                .iter()
                .map(|pair| (pair.name.clone(), pair.value.clone()))
                .collect(),
        ),
        _ => None,
    };

    let secret_names_list: Vec<String> = request
        .secret_ids
        .iter()
        .flatten()
        .filter_map(|id| secret_names.get(id).cloned())
        .collect();
    let var_names_list: Vec<String> = request
        .var_ids
        .iter()
        .flatten()
        .filter_map(|id| var_names.get(id).cloned())
        .collect();

    let steps = request
        .steps
        .iter()
        .map(|step| StepYaml {
            name: Some(step.name.clone()),
            run: Some(step.run.clone()),
            engine: step.engine.clone(),
            image: step.image.clone(),
            volumes: step.volumes.clone(),
            privileged: step.privileged,
            config: None,
            host: step.host.clone(),
            dir: step.dir.clone(),
            host_labels: step.host_labels.clone(),
            hosts_strategy: step.hosts_strategy.clone(),
            hosts_max_parallel: step.hosts_max_parallel,
            timeout: step.timeout.clone(),
            retry: step.retry,
            needs: if step.needs.is_empty() {
                None
            } else {
                Some(OneOrMany::Many(step.needs.clone()))
            },
            uses: step.uses.clone(),
            with: step.with.clone(),
            continue_on_error: step.continue_on_error,
            secrets: step.secrets.clone(),
            env: step.env.clone(),
        })
        .collect();

    let pipeline = PipelineYaml {
        name: request.name.trim().to_string(),
        description: description_field(&request.description),
        concurrency: concurrency_from_request(
            &request.concurrency_group,
            request.concurrency_mode.as_deref(),
            request.concurrency_max_queue,
            request.concurrency_queue_timeout_seconds,
        ),
        on,
        runner: Some(RunnerYaml {
            labels: if request.runner_labels.is_empty() {
                None
            } else {
                Some(request.runner_labels.clone())
            },
            timeout: Some(seconds(
                (request.timeout_seconds.unwrap_or(3600).max(1)) as i64,
            )),
            resources: None,
        }),
        env,
        variables: if var_names_list.is_empty() {
            None
        } else {
            Some(var_names_list)
        },
        secrets: if secret_names_list.is_empty() {
            None
        } else {
            Some(secret_names_list)
        },
        team: Some(team_slug.to_string()),
        environments: request.environments.clone(),
        notify: request.notify.clone(),
        steps,
    };

    emit_and_validate(&pipeline)
}

/// Concurrency block from the request's scalar columns. Only emitted when a
/// group is set (an empty group means "no concurrency control").
fn concurrency_from_request(
    group: &Option<String>,
    mode: Option<&str>,
    max_queue: Option<i32>,
    queue_timeout_seconds: Option<i32>,
) -> Option<shared::pipeline_yaml::ConcurrencyDef> {
    let group = group
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())?;
    let mode = mode
        .filter(|mode| !mode.is_empty())
        .unwrap_or("discard-new");
    let max_queue = max_queue.unwrap_or(3);
    let queue_timeout = queue_timeout_seconds.map(|secs| seconds(secs as i64));
    Some(shared::pipeline_yaml::ConcurrencyDef::Full(
        shared::pipeline_yaml::ConcurrencyYaml {
            group: group.to_string(),
            mode: mode.to_string(),
            max_queue: max_queue.max(0) as u32,
            queue_timeout,
        },
    ))
}

// ── migration assembly ──────────────────────────────────────────────────────

/// Row-level pieces for [`yaml_from_row_parts`] — assembled by the migration
/// from `pipelines` / `tasks` / `step_resolutions` / bindings.
pub(crate) struct RowSource {
    pub name: String,
    pub description: String,
    pub trigger_type: String,
    pub webhook_path: Option<String>,
    pub webhook_secret: String,
    pub webhook_environment: String,
    pub cron_expr: Option<String>,
    pub cron_environment: String,
    pub runner_labels: Vec<String>,
    pub timeout_seconds: i64,
    pub concurrency_group: Option<String>,
    pub concurrency_mode: String,
    pub concurrency_max_queue: i64,
    pub concurrency_queue_timeout_seconds: i64,
    pub owner_group: String,
    pub environments: Vec<String>,
    pub notify: Vec<NotifyChannelYaml>,
    pub input_defs: Vec<shared::protocol::PipelineInputDef>,
    pub env_pairs: Vec<(String, String)>,
    pub secret_names: Vec<String>,
    pub var_names: Vec<String>,
    /// `(step_order, uses_ref, resolved_version)` per marketplace step.
    pub resolutions: Vec<(i64, String, String)>,
    /// Expanded task projection, one entry per step in job order:
    /// `(name, exec_type, command_template, image, work_dir, timeout_seconds,
    /// retry_count, fail_strategy, step_secrets, step_env_json, volumes,
    /// privileged, hosts_strategy, hosts_max_parallel, needs_json,
    /// host_names, host_labels)`.
    pub steps: Vec<RowStep>,
}

pub(crate) struct RowStep {
    pub step_order: i64,
    pub name: String,
    pub exec_type: String,
    pub command_template: String,
    pub image: String,
    pub work_dir: String,
    pub timeout_seconds: i64,
    pub retry_count: i64,
    pub fail_strategy: String,
    pub step_secrets: String,
    pub step_env_json: String,
    pub volumes: Vec<String>,
    pub privileged: bool,
    pub hosts_strategy: String,
    pub hosts_max_parallel: Option<i64>,
    pub needs: Vec<String>,
    pub host_names: String,
    pub host_labels: String,
}

pub(crate) fn yaml_from_row_parts(source: &RowSource) -> Result<String, AppError> {
    let declared_inputs: Option<BTreeMap<String, InputDef>> = if source.input_defs.is_empty() {
        None
    } else {
        Some(
            source
                .input_defs
                .iter()
                .map(|def| {
                    (
                        def.name.clone(),
                        InputDef {
                            description: def.description.clone(),
                            required: def.required,
                            default: def.default.clone(),
                            masking: def.masking.filter(|mask| *mask),
                            r#type: def.input_type.clone(),
                            pattern: def.pattern.clone(),
                            options: def.options.clone(),
                        },
                    )
                })
                .collect(),
        )
    };
    let manual_inputs = if source.trigger_type == "manual" {
        declared_inputs.clone()
    } else {
        None
    };

    let on = Some(match source.trigger_type.as_str() {
        "webhook" => TriggerYaml {
            manual: None,
            webhook: Some(WebhookTriggerYaml {
                path: source.webhook_path.clone(),
                inputs: declared_inputs.clone(),
                secret: Some(source.webhook_secret.clone()).filter(|name| !name.is_empty()),
                environment: Some(source.webhook_environment.clone()).filter(|env| !env.is_empty()),
            }),
            cron: None,
        },
        "cron" => TriggerYaml {
            manual: None,
            webhook: None,
            cron: Some(CronTriggerYaml {
                schedule: source.cron_expr.clone(),
                inputs: declared_inputs.clone(),
                environment: Some(source.cron_environment.clone()).filter(|env| !env.is_empty()),
            }),
        },
        _ => TriggerYaml {
            manual: Some(ManualTriggerYaml {
                inputs: manual_inputs,
            }),
            webhook: None,
            cron: None,
        },
    });

    let resolutions_by_order: HashMap<i64, (String, String)> = source
        .resolutions
        .iter()
        .map(|(order, uses_ref, version)| (*order, (uses_ref.clone(), version.clone())))
        .collect();

    let mut steps = Vec::with_capacity(source.steps.len());
    for row in &source.steps {
        // Marketplace provenance: reconstruct `uses:` + `with:` from the
        // resolution row and the expanded INPUT_* env values, so the migrated
        // file stays template-linked (re-saved pipelines keep resolving).
        let (uses, with, run) = match resolutions_by_order.get(&row.step_order) {
            Some((uses_ref, version)) => {
                let with = with_from_step_env(&row.step_env_json, &source.input_defs);
                (
                    Some(format!("{uses_ref}@{version}")),
                    (!with.is_empty()).then_some(with),
                    None,
                )
            }
            None => (None, None, Some(row.command_template.clone())),
        };

        steps.push(StepYaml {
            name: Some(row.name.clone()),
            run,
            engine: Some(if row.exec_type == "local" {
                "shell".to_string()
            } else {
                row.exec_type.clone()
            }),
            image: (!row.image.is_empty()).then(|| row.image.clone()),
            volumes: (!row.volumes.is_empty()).then(|| row.volumes.clone()),
            privileged: row.privileged.then_some(true),
            config: None,
            host: hosts_from_names(&row.host_names),
            dir: (!row.work_dir.is_empty()).then(|| row.work_dir.clone()),
            host_labels: hosts_from_names(&row.host_labels),
            hosts_strategy: (!row.hosts_strategy.is_empty() && row.hosts_strategy != "chain")
                .then(|| row.hosts_strategy.clone()),
            hosts_max_parallel: row
                .hosts_max_parallel
                .filter(|width| *width > 0)
                .map(|width| width as u32),
            timeout: (row.timeout_seconds > 0).then(|| seconds(row.timeout_seconds)),
            retry: (row.retry_count > 0).then_some(row.retry_count as i32),
            needs: (!row.needs.is_empty()).then_some(OneOrMany::Many(row.needs.clone())),
            uses,
            with,
            continue_on_error: (row.fail_strategy == "continue").then_some(true),
            secrets: {
                let names: Vec<String> = row
                    .step_secrets
                    .split(',')
                    .map(str::trim)
                    .filter(|name| !name.is_empty())
                    .map(str::to_string)
                    .collect();
                (!names.is_empty()).then_some(names)
            },
            env: env_from_json(&row.step_env_json),
        });
    }

    let env: Option<BTreeMap<String, String>> = if source.env_pairs.is_empty() {
        None
    } else {
        Some(source.env_pairs.iter().cloned().collect())
    };

    let pipeline = PipelineYaml {
        name: source.name.trim().to_string(),
        description: description_field(&Some(source.description.clone())),
        concurrency: concurrency_from_request(
            &source.concurrency_group,
            Some(&source.concurrency_mode),
            Some(source.concurrency_max_queue as i32),
            Some(source.concurrency_queue_timeout_seconds as i32),
        ),
        on,
        runner: Some(RunnerYaml {
            labels: (!source.runner_labels.is_empty()).then(|| source.runner_labels.clone()),
            timeout: (source.timeout_seconds > 0).then(|| seconds(source.timeout_seconds)),
            resources: None,
        }),
        env,
        variables: (!source.var_names.is_empty()).then(|| source.var_names.clone()),
        secrets: (!source.secret_names.is_empty()).then(|| source.secret_names.clone()),
        team: Some(source.owner_group.clone()),
        environments: source.environments.clone(),
        notify: (!source.notify.is_empty()).then(|| source.notify.clone()),
        steps,
    };

    emit_and_validate(&pipeline)
}

/// Recover the template's `with:` values from the expanded step env: the
/// migration maps each declared input name through `INPUT_<UPPER>` back to
/// the original name. `${secrets.X}` / `${vars.X}` / `${env.X}` placeholders
/// survive verbatim (the claim path re-resolves them).
fn with_from_step_env(
    step_env_json: &str,
    input_defs: &[shared::protocol::PipelineInputDef],
) -> BTreeMap<String, String> {
    let Ok(map) = serde_json::from_str::<BTreeMap<String, String>>(step_env_json) else {
        return BTreeMap::new();
    };
    input_defs
        .iter()
        .filter_map(|def| {
            let key = shared::protocol::input_env_key(&def.name);
            map.get(&key).map(|value| (def.name.clone(), value.clone()))
        })
        .collect()
}

fn env_from_json(step_env_json: &str) -> Option<BTreeMap<String, String>> {
    match serde_json::from_str::<BTreeMap<String, String>>(step_env_json) {
        Ok(map) if !map.is_empty() => Some(map),
        _ => None,
    }
}

fn hosts_from_names(raw: &str) -> Option<OneOrMany> {
    let names: Vec<String> = raw
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .collect();
    match names.len() {
        0 => None,
        1 => Some(OneOrMany::One(names.into_iter().next().unwrap())),
        _ => Some(OneOrMany::Many(names)),
    }
}
