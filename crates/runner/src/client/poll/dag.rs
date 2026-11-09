//! `needs:` DAG execution.
//!
//! Steps that declare `needs:` form a DAG: a step runs once every transitive
//! dependency has finished; ready steps run concurrently. A failed step
//! (`fail_strategy: stop`) or an engine/setup error skips its transitive
//! dependents (status `skipped`); independent steps still run and the job
//! ends failed. `continue-on-error` failures do not skip dependents — the
//! step's failure is recorded and the job still ends failed.
//!
//! Pipelines without any `needs:` never enter this module (the sequential
//! loop is untouched); cancellation propagates through the same watch channel
//! every engine already races against.
//!
//! `$BUZ_ENV` under a DAG: each step gets its own fragment file
//! (`.buz/env-{idx}`) so concurrent writers cannot interleave; a step's
//! environment is the job base env merged with the fragment maps of its
//! dependency closure in topological order (later dependencies win on
//! duplicate keys).

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::watch;
use tokio::task::JoinSet;

use super::step_log::{build_retry_marker, LOG_STEP_RETRY, LOG_STREAM_START};
use super::{
    announce_step, build_step_secrets, finalize_step, open_ssh_sessions, parse_step_spec,
    run_docker_step, run_shell_step, run_ssh_step, validate_step_constraints, StepExecution,
    StepLogger, StepRun,
};

/// Resolved DAG for one job's steps: topological order plus adjacency.
pub(super) struct DagPlan {
    /// Steps in a deterministic topological order (job order among ties).
    pub(super) order: Vec<usize>,
    /// Per step: indices it declared in `needs` (resolved, deduplicated).
    pub(super) needs_of: Vec<Vec<usize>>,
    /// Per step: indices that declare it (reverse edges).
    pub(super) dependents_of: Vec<Vec<usize>>,
}

impl DagPlan {
    /// `Some(plan)` when any step declares `needs`; `None` → the sequential
    /// loop handles the job unchanged.
    pub(super) fn build(tasks: &[serde_json::Value]) -> anyhow::Result<Option<Self>> {
        let uses_needs = tasks.iter().any(|step| {
            step["needs"]
                .as_array()
                .is_some_and(|list| !list.is_empty())
        });
        if !uses_needs {
            return Ok(None);
        }

        // Name → first index. Duplicate names are rejected at save when needs
        // is used; defend here too with an actionable message.
        let mut by_name: HashMap<&str, usize> = HashMap::new();
        for (index, step) in tasks.iter().enumerate() {
            let name = step["name"].as_str().unwrap_or("").trim();
            if name.is_empty() {
                continue;
            }
            if let Some(previous) = by_name.insert(name, index) {
                anyhow::bail!(
                    "step name '{name}' is declared twice (steps {previous} and {index}) — \
                     names must be unique when 'needs' is used"
                );
            }
        }

        let mut needs_of: Vec<Vec<usize>> = Vec::with_capacity(tasks.len());
        for (index, step) in tasks.iter().enumerate() {
            let own_name = step["name"].as_str().unwrap_or("").trim();
            let mut resolved: Vec<usize> = Vec::new();
            for target in step["needs"].as_array().unwrap_or(&vec![]) {
                let target = target.as_str().unwrap_or("").trim();
                if target.is_empty() {
                    anyhow::bail!("step {index}: needs entries must not be empty");
                }
                if target == own_name {
                    anyhow::bail!("step '{target}' cannot need itself");
                }
                match by_name.get(target) {
                    Some(&target_index) => {
                        if !resolved.contains(&target_index) {
                            resolved.push(target_index);
                        }
                    }
                    None => anyhow::bail!(
                        "step {index}: needs target '{target}' does not match any step name"
                    ),
                }
            }
            needs_of.push(resolved);
        }

        // Topological order (Kahn), ties broken by job order. A leftover node
        // means a cycle — the save-time layer rejects those; fail the job with
        // the same message instead of silently serializing.
        let mut dependents_of: Vec<Vec<usize>> = vec![Vec::new(); tasks.len()];
        let mut indegree: Vec<usize> = needs_of.iter().map(|needs| needs.len()).collect();
        let mut order = Vec::with_capacity(tasks.len());
        loop {
            let mut progressed = false;
            for index in 0..tasks.len() {
                if indegree[index] == 0 && !order.contains(&index) {
                    order.push(index);
                    for &dependent in &dependents_of[index] {
                        // Populated below on the first pass — see edge insert.
                        let _ = dependent;
                    }
                    progressed = true;
                }
            }
            if !progressed {
                break;
            }
            // Recompute: keep the loop simple by draining in waves.
            let mut changed = false;
            for &done in order.clone().iter() {
                for (index, needs) in needs_of.iter().enumerate() {
                    if indegree[index] > 0 && needs.iter().all(|need| order.contains(need)) {
                        indegree[index] = 0;
                        changed = true;
                    }
                }
                let _ = changed;
                let _ = done;
            }
            if !changed && order.len() < tasks.len() {
                anyhow::bail!("'needs' forms a dependency cycle — fix the pipeline definition");
            }
            if order.len() == tasks.len() {
                break;
            }
        }

        for (index, needs) in needs_of.iter().enumerate() {
            for &need in needs {
                dependents_of[need].push(index);
            }
        }

        Ok(Some(Self {
            order,
            needs_of,
            dependents_of,
        }))
    }
}

/// Outcome of one finished step (cancel/abort errors return as `Err`).
struct StepOutcome {
    index: usize,
    /// `true` when the step failed under `fail_strategy: continue` — the job
    /// ends failed but dependents still run.
    soft_failed: bool,
    /// `$BUZ_ENV` fragment parsed from this step's writes.
    env_fragment: Vec<(String, String)>,
}

/// Execute the whole job as a DAG.
///
/// Wave scheduling: spawn every step whose dependencies are done, wait for
/// one completion, repeat. Returns the fatal error after all runnable steps
/// have finished — a cancel sentinel aborts immediately; any other step error
/// skips its dependents, lets independent steps finish, and is returned at
/// the end (the job ends failed either way, matching the sequential
/// stop semantics).
#[allow(clippy::too_many_arguments)] // mirrors execute_job_inner's context
pub(super) async fn execute(
    client: &reqwest::Client,
    config: &crate::config::Config,
    token: &str,
    task: &serde_json::Value,
    tasks: &[serde_json::Value],
    plan: DagPlan,
    cancel_rx: &mut watch::Receiver<bool>,
    sandbox_handle: &Option<std::sync::Arc<crate::client::sandbox_client::SandboxHandle>>,
) -> anyhow::Result<()> {
    use std::sync::Arc;
    use tokio::sync::Mutex;

    let job_id = task["job_id"].as_str().unwrap_or("unknown").to_string();
    let debug_unredact = task
        .get("debug_unredact")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let verbose = task
        .get("verbose")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let docker_config_dir: Option<String> = match config.engine.docker_config_isolation {
        crate::config::DockerConfigIsolation::PerJob => {
            Some(super::job_docker_config_dir(&config.runner.workspace_dir))
        }
        crate::config::DockerConfigIsolation::Shared => None,
    };

    let pipeline_secret_keys: std::collections::HashSet<String> = task
        .get("secrets")
        .and_then(|v| v.as_object())
        .map(|obj| obj.keys().cloned().collect())
        .unwrap_or_default();
    let secret_mask_values: Vec<String> = task
        .get("secrets")
        .and_then(|v| v.as_object())
        .map(|obj| {
            obj.values()
                .filter_map(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();

    // Job base env: trigger inputs + expanded pipeline env + BUZ_* metadata
    // (same construction as the sequential loop).
    let base_env = super::build_base_env(task, &job_id, &config.runner.manager_url);

    let buz_dir = format!("{}/.buz", config.runner.workspace_dir.trim_end_matches('/'));
    let _ = std::fs::create_dir_all(&buz_dir);

    // The shared image prefetch stays owned by the caller; concurrent docker
    // steps take the lock only for the (cheap) wait, not for the whole run.
    let shared_prefetch: Arc<Mutex<Option<crate::client::image_prefetch::ImagePrefetch>>> =
        Arc::new(Mutex::new(None));

    let mut fragment_maps: Vec<Option<HashMap<String, String>>> = vec![None; tasks.len()];
    let mut finished: Vec<bool> = vec![false; tasks.len()];
    let mut skipped: Vec<bool> = vec![false; tasks.len()];
    let mut unmet: Vec<usize> = plan.needs_of.iter().map(|needs| needs.len()).collect();
    let mut any_step_failed = false;
    let mut first_error: Option<anyhow::Error> = None;
    let mut running: JoinSet<Result<StepOutcome, (usize, anyhow::Error)>> = JoinSet::new();
    let mut assigned: Vec<bool> = vec![false; tasks.len()];

    loop {
        if *cancel_rx.borrow() {
            running.abort_all();
            while running.join_next().await.is_some() {}
            return Err(anyhow::anyhow!("job cancelled by user"));
        }

        // Spawn every ready step.
        for &index in &plan.order {
            if assigned[index] || skipped[index] || unmet[index] > 0 {
                continue;
            }
            assigned[index] = true;

            // Closure env: base + dependency fragments in topological order
            // (a later dependency overwrites earlier ones on duplicate keys).
            let mut step_base = base_env.clone();
            for dependency in plan_topological_closure(&plan, index) {
                if let Some(map) = &fragment_maps[dependency] {
                    for (key, value) in map {
                        step_base.insert(key.clone(), value.clone());
                    }
                }
            }

            let client = client.clone();
            let config = config.clone();
            let token = token.to_string();
            let step_job_id = job_id.clone();
            let step = tasks[index].clone();
            let cancel_rx = cancel_rx.clone();
            let shared_prefetch = Arc::clone(&shared_prefetch);
            let sandbox_handle = sandbox_handle.clone();
            let docker_config_dir = docker_config_dir.clone();
            let secret_mask_values = secret_mask_values.clone();
            let pipeline_secret_keys = pipeline_secret_keys.clone();

            running.spawn(async move {
                let sandbox_ref = sandbox_handle.as_deref();
                run_one_step(RunOneStep {
                    client: &client,
                    config: &config,
                    token: &token,
                    job_id: &step_job_id,
                    index,
                    step: &step,
                    base_env: step_base,
                    pipeline_secret_keys: &pipeline_secret_keys,
                    secret_mask_values: &secret_mask_values,
                    debug_unredact,
                    verbose,
                    docker_config_dir: docker_config_dir.as_deref(),
                    sandbox_handle: sandbox_ref,
                    cancel_rx: &mut cancel_rx.clone(),
                    prefetch: &shared_prefetch,
                })
                .await
            });
        }

        if running.is_empty() {
            break; // nothing in flight: all steps finished or skipped
        }

        let joined = running.join_next().await;
        let Some(joined) = joined else {
            break;
        };
        let outcome: Result<StepOutcome, (usize, anyhow::Error)> = match joined {
            Ok(result) => result,
            Err(join_error) => {
                // A panicked step: the index is lost with the panic — treat
                // the oldest unfinished assigned step as failed (its
                // dependents must not hang).
                let index = (0..tasks.len())
                    .find(|&index| assigned[index] && !finished[index] && !skipped[index])
                    .unwrap_or(0);
                Err((index, anyhow::anyhow!("step task failed: {join_error}")))
            }
        };

        match outcome {
            Ok(step) => {
                let index = step.index;
                finished[index] = true;
                fragment_maps[index] = Some(step.env_fragment.into_iter().collect());
                any_step_failed |= step.soft_failed;
                if step.soft_failed {
                    tracing::warn!(
                        step = index,
                        "step failed under fail_strategy: continue — dependents still run"
                    );
                }
                for &dependent in &plan.dependents_of[index] {
                    unmet[dependent] = unmet[dependent].saturating_sub(1);
                }
            }
            Err((index, error)) => {
                if error.to_string().contains("job cancelled by user") {
                    running.abort_all();
                    while running.join_next().await.is_some() {}
                    return Err(error);
                }
                // The failing step already reported itself failed; skip its
                // transitive dependents and let independent steps finish.
                if first_error.is_none() {
                    first_error = Some(error);
                }
                finished[index] = true;
                for &dependent in &plan.dependents_of[index] {
                    unmet[dependent] = unmet[dependent].saturating_sub(1);
                }
                let newly_skipped = skip_dependents(&plan, index, &mut skipped);
                for dependent in newly_skipped {
                    super::update_job_status(
                        client,
                        config,
                        token,
                        &job_id,
                        "skipped",
                        Some(dependent as i32),
                    )
                    .await?;
                }
            }
        }
    }

    if let Some(error) = first_error {
        return Err(error);
    }
    if any_step_failed {
        tracing::warn!("Job {job_id} finished with failed step(s)");
        super::update_job_status(client, config, token, &job_id, "failed", None).await?;
        return Ok(());
    }

    super::update_job_status(client, config, token, &job_id, "success", None).await?;
    tracing::info!("Job {job_id} completed successfully (DAG)");
    Ok(())
}

/// Mark the transitive dependents of `index` skipped (never scheduled);
/// returns the newly skipped indices so the caller records their status.
fn skip_dependents(plan: &DagPlan, index: usize, skipped: &mut [bool]) -> Vec<usize> {
    let mut newly_skipped = Vec::new();
    let mut stack = vec![index];
    let mut seen = vec![false; plan.dependents_of.len()];
    seen[index] = true;
    while let Some(node) = stack.pop() {
        for &dependent in &plan.dependents_of[node] {
            if seen[dependent] {
                continue;
            }
            seen[dependent] = true;
            if !skipped[dependent] {
                skipped[dependent] = true;
                tracing::info!(step = dependent, "step skipped — dependency failed");
                newly_skipped.push(dependent);
            }
            stack.push(dependent);
        }
    }
    newly_skipped
}

fn plan_topological_closure(plan: &DagPlan, index: usize) -> impl Iterator<Item = usize> + '_ {
    plan.order
        .iter()
        .copied()
        .filter(move |&candidate| dependency_reachable(plan, index, candidate))
}

fn dependency_reachable(plan: &DagPlan, from: usize, target: usize) -> bool {
    let mut stack = vec![from];
    let mut seen = vec![false; plan.needs_of.len()];
    while let Some(node) = stack.pop() {
        if node == target {
            return true;
        }
        for &need in &plan.needs_of[node] {
            if !seen[need] {
                seen[need] = true;
                stack.push(need);
            }
        }
    }
    false
}

struct RunOneStep<'a> {
    client: &'a reqwest::Client,
    config: &'a crate::config::Config,
    token: &'a str,
    job_id: &'a str,
    index: usize,
    step: &'a serde_json::Value,
    base_env: HashMap<String, String>,
    pipeline_secret_keys: &'a std::collections::HashSet<String>,
    secret_mask_values: &'a [String],
    debug_unredact: bool,
    verbose: bool,
    docker_config_dir: Option<&'a str>,
    sandbox_handle: Option<&'a crate::client::sandbox_client::SandboxHandle>,
    cancel_rx: &'a mut watch::Receiver<bool>,
    prefetch: &'a Arc<tokio::sync::Mutex<Option<crate::client::image_prefetch::ImagePrefetch>>>,
}

async fn run_one_step(ctx: RunOneStep<'_>) -> Result<StepOutcome, (usize, anyhow::Error)> {
    let index = ctx.index;
    let log = StepLogger::new(ctx.client, ctx.config, ctx.token, ctx.job_id, index);
    let spec = parse_step_spec(index, ctx.step);
    let docker_socket_path = validate_step_constraints(&log, ctx.config, ctx.step, &spec)
        .await
        .map_err(|error| (index, error))?;
    let secrets = build_step_secrets(
        &log,
        ctx.step,
        &ctx.base_env,
        ctx.pipeline_secret_keys,
        ctx.secret_mask_values,
        ctx.debug_unredact,
    )
    .await
    .map_err(|error| (index, error))?;

    // Per-step $BUZ_ENV fragment: concurrent writers cannot interleave.
    let fragment_name = format!("env-{index}");
    let buz_env_path = format!(
        "{}/.buz/{fragment_name}",
        ctx.config.runner.workspace_dir.trim_end_matches('/')
    );
    let _ = std::fs::write(&buz_env_path, "");

    let step_started = std::time::Instant::now();
    announce_step(&log, ctx.config, ctx.step, &spec, &secrets.env, ctx.verbose)
        .await
        .map_err(|error| (index, error))?;

    let max_attempts = spec.retry.saturating_add(1);
    let mut stream_start = log.log_base + LOG_STREAM_START;
    let mut attempt: u32 = 0;

    let execution = loop {
        attempt += 1;
        let attempt_result: anyhow::Result<StepExecution> = async {
            let mut ssh_sessions =
                open_ssh_sessions(&log, ctx.config, ctx.step, &spec, ctx.cancel_rx).await?;
            if ctx.verbose {
                log.emit(
                    super::step_log::LOG_STEP_VERBOSE_EXEC,
                    super::step_log::build_verbose_stage("exec"),
                )
                .await;
            }
            let run = StepRun {
                log: &log,
                config: ctx.config,
                spec: spec.clone(),
                env: secrets.env.clone(),
                catalog: &secrets.catalog,
                secret_values: &secrets.secret_values,
                forced_values: &secrets.forced_values,
                docker_config_dir: ctx.docker_config_dir,
                buz_env_path: &buz_env_path,
                sandbox_handle: ctx.sandbox_handle,
                stream_start,
            };
            match spec.exec_type {
                "local" | "shell" => run_shell_step(run, ctx.cancel_rx).await,
                "ssh" => run_ssh_step(run, &mut ssh_sessions, ctx.cancel_rx).await,
                "docker" => {
                    // Shared prefetch: hold the lock only for the wait, then
                    // release so another docker step can start its own wait.
                    let image = ctx.step["image"].as_str().unwrap_or("").trim();
                    if !image.is_empty() {
                        let wait = {
                            let mut guard = ctx.prefetch.lock().await;
                            match guard.as_mut() {
                                Some(prefetch) => prefetch.wait_for(image).await,
                                None => None,
                            }
                        };
                        if let Some(Err(error)) = wait {
                            tracing::warn!("Docker image prefetch failed for {image:?}: {error}");
                        }
                    }
                    run_docker_step(run, &mut None, docker_socket_path.as_deref(), ctx.cancel_rx)
                        .await
                }
                "wasm" => anyhow::bail!(
                    "wasm engine is not implemented in this release — step '{}' cannot run",
                    spec.name
                ),
                other => anyhow::bail!("unsupported engine type: {other}"),
            }
        }
        .await;

        match attempt_result {
            Ok(execution) => {
                stream_start = execution.next_stream;
                let failed = execution.output.exit_code != 0 && !execution.was_cancelled;
                if failed && attempt < max_attempts {
                    let detail = format!("exit {}", execution.output.exit_code);
                    log.emit(
                        LOG_STEP_RETRY,
                        build_retry_marker(spec.name, attempt, max_attempts, &detail),
                    )
                    .await;
                    continue;
                }
                break execution;
            }
            Err(error) => {
                let cancelled = *ctx.cancel_rx.borrow();
                if attempt < max_attempts && !cancelled {
                    let detail = super::mask_output(
                        &format!("{error:#}"),
                        &secrets.secret_values,
                        &secrets.forced_values,
                    );
                    log.emit(
                        LOG_STEP_RETRY,
                        build_retry_marker(spec.name, attempt, max_attempts, detail.trim()),
                    )
                    .await;
                    continue;
                }
                return Err((index, error));
            }
        }
    };

    let soft_failed = finalize_step(
        &log,
        &spec,
        execution,
        &secrets.forced_values,
        ctx.sandbox_handle,
        ctx.verbose,
        step_started,
    )
    .await
    .map_err(|error| (index, error))?;

    // Collect the step's $BUZ_ENV fragment (VM mode reads it back through the
    // supervisor; host modes read the workspace file directly).
    let env_fragment = match ctx.sandbox_handle {
        Some(handle) if handle.mode() == shared::protocol::RunnerExecutionMode::Vm => {
            crate::client::sandbox_client::read_job_file(
                &ctx.config.sandbox,
                handle,
                &format!(".buz/{fragment_name}"),
            )
            .await
            .and_then(|content| super::parse_buz_env_content(&content))
            .unwrap_or_default()
        }
        _ => super::parse_buz_env_file(&buz_env_path).unwrap_or_default(),
    };

    Ok(StepOutcome {
        index,
        soft_failed,
        env_fragment,
    })
}
