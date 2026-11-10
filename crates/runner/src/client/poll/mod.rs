use reqwest::Client;
use shared::protocol::{LogChunk, LogLevel, UpdateJobStatusRequest};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::watch;

use crate::config::{Config, DockerConfigIsolation};
use crate::engine;
use crate::engine::ssh::{SshConnectionParams, SshSession};
use crate::masker::SecretCatalog;
use crate::policy::{self, SharedPolicy};
use std::collections::{HashMap, HashSet};

pub(crate) mod dag;
pub mod step_log;

use step_log::{
    build_retry_marker, build_runner_banner, build_ssh_banner, build_step_echo,
    build_step_failure_message, build_verbose_stage, build_verbose_step_config,
    build_verbose_step_result, mask_output, step_failure_hint, step_timeout_content, StepLogger,
    StreamBudget, LOG_ECHO, LOG_LOGIN, LOG_RUNNER_BANNER, LOG_STEP_RETRY, LOG_STEP_SUMMARY,
    LOG_STEP_TRUNCATED, LOG_STEP_VERBOSE_CONFIG, LOG_STEP_VERBOSE_EXEC, LOG_STEP_VERBOSE_PREPARE,
    LOG_STEP_VERBOSE_RESULT, LOG_STREAM_START,
};

/// Poll loop with concurrent job slots and exponential backoff on failures.
///
/// Capacity and strategy (engines, cleanup) come from Manager UI via
/// `shared_policy` (refreshed by heartbeat).
pub async fn run_poll_loop(
    client: &Client,
    config: &Config,
    token: &str,
    shared_policy: SharedPolicy,
    restart_requested: Arc<AtomicBool>,
) -> anyhow::Result<()> {
    use tokio::task::JoinSet;

    let mut consecutive_failures: u32 = 0;
    let mut jobs: JoinSet<anyhow::Result<()>> = JoinSet::new();
    // Cancel signals for live jobs; notices arrive on the claim channel.
    let cancels: CancelRegistry = Arc::new(Mutex::new(HashMap::new()));
    // Manager keeps cancel notices pending until the next poll carries this
    // acknowledgement; retain them across transport errors.
    let mut pending_cancel_acks: Vec<String> = Vec::new();

    loop {
        // Reap finished jobs
        while let Some(joined) = jobs.try_join_next() {
            match joined {
                Ok(Ok(())) => {}
                Ok(Err(e)) => tracing::error!("Job task error: {e}"),
                Err(e) => tracing::error!("Job task panicked: {e}"),
            }
        }

        // Graceful restart: stop claiming new work, let in-flight jobs drain,
        // then exit cleanly (exit 0) so the supervisor restarts us. The Manager
        // already blocks new dispatch once `restart_requested` is set, so the
        // remaining in-flight set is bounded by the current running jobs.
        if restart_requested.load(Ordering::SeqCst) {
            if jobs.is_empty() {
                tracing::info!(
                    "Restart requested and no active jobs — exiting for supervisor restart"
                );
                return Ok(());
            }
            tracing::info!(
                "Restart requested — draining {} active job(s) before exit",
                jobs.len()
            );
            tokio::time::sleep(Duration::from_secs(2)).await;
            continue;
        }

        let local_max = policy::max_concurrent(&shared_policy);
        let active = jobs.len() as u32;
        // No runner-side capacity gate: the Manager rejects claims at capacity
        // and still delivers cancel notices, which a busy runner must keep
        // receiving (that channel replaces the old per-job status probes).

        let poll_result = poll_for_task(client, config, token, &pending_cancel_acks, active).await;
        match poll_result {
            Ok(outcome) => {
                // Successful response proves Manager received the prior batch
                // of acknowledgements. Queue newly observed ids before
                // signalling so even an already-finished local job is acked.
                pending_cancel_acks.clear();
                for job_id in &outcome.cancelled_jobs {
                    if !pending_cancel_acks.contains(job_id) {
                        pending_cancel_acks.push(job_id.clone());
                    }
                    tracing::info!("Manager reported job {job_id} cancelled — signalling");
                    signal_cancel(&cancels, job_id);
                }
                let Some(task) = outcome.task else {
                    if consecutive_failures > 0 {
                        tracing::info!("Poll recovered after {consecutive_failures} failure(s)");
                    }
                    consecutive_failures = 0;
                    if let Some(delay_ms) = outcome.retry_after_ms {
                        tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                    }
                    continue;
                };

                consecutive_failures = 0;

                let job_id = task["job_id"].as_str().unwrap_or("unknown").to_string();
                tracing::info!("Claimed job {job_id} (active {}/{})", active + 1, local_max);

                let c = client.clone();
                let mut job_config = config.clone();
                let snap = policy::snapshot(&shared_policy);
                policy::apply_to_config(&mut job_config, &snap);
                // Isolate workspace per job so parallel jobs do not clash
                let job_ws = format!(
                    "{}/{}",
                    config.runner.workspace_dir.trim_end_matches('/'),
                    job_id
                );
                if let Err(e) = std::fs::create_dir_all(&job_ws) {
                    tracing::error!("Failed to create job workspace {job_ws}: {e}");
                    continue;
                }
                restrict_job_workspace(&job_ws, config.engine.run_as_user.as_deref());
                if let Err(e) = prepare_job_docker_config_dir(
                    &job_ws,
                    config.engine.docker_config_isolation,
                    config.engine.run_as_user.as_deref(),
                ) {
                    // Best-effort: `docker login` recreates the dir if missing.
                    tracing::warn!("Failed to prepare job docker config dir for {job_id}: {e}");
                }
                job_config.runner.workspace_dir = job_ws.clone();
                let t = token.to_string();
                let clean = snap.clean_workspace_after_job;

                // Cancel signal: registered before spawn so a notice arriving
                // between claim and first step cannot be missed.
                let (cancel_tx, cancel_rx) = watch::channel(false);
                cancels
                    .lock()
                    .expect("cancel registry")
                    .insert(job_id.clone(), cancel_tx);
                let cancels_for_job = Arc::clone(&cancels);
                let job_id_for_cleanup = job_id.clone();

                jobs.spawn(async move {
                    let result = run_one_job(&c, &job_config, &t, &task, cancel_rx).await;
                    // Unconditional: job-scoped Docker credentials never outlive
                    // the job — independent of clean_workspace_after_job (which
                    // defaults to false).
                    clean_job_docker_config(&job_ws);
                    if clean {
                        tracing::info!("Cleaning job workspace: {job_ws}");
                        clean_workspace(&job_ws);
                    }
                    cancels_for_job
                        .lock()
                        .expect("cancel registry")
                        .remove(&job_id_for_cleanup);
                    result
                });
            }
            Err(e) => {
                consecutive_failures += 1;

                let delay_secs = match consecutive_failures {
                    0 => 0,
                    n if n <= 5 => 2u64 << (n - 1),
                    _ => 60,
                };
                let delay = Duration::from_secs(delay_secs);

                tracing::warn!(
                    "Poll failed (attempt #{consecutive_failures}): {e}.  Retrying in {delay:?}..."
                );

                tokio::time::sleep(delay).await;
            }
        }
    }
}

/// Execute one claimed job and report terminal status to Manager.
async fn run_one_job(
    client: &Client,
    config: &Config,
    token: &str,
    task: &serde_json::Value,
    mut cancel_rx: watch::Receiver<bool>,
) -> anyhow::Result<()> {
    let job_id = task["job_id"].as_str().unwrap_or("unknown");

    if let Err(e) = execute_job(client, config, token, task, &mut cancel_rx).await {
        let msg = format!("{e:#}");
        if msg.contains("job cancelled by user") {
            tracing::warn!("Job {job_id} cancelled by user — skipping error log");
            if let Err(se) =
                update_job_status(client, config, token, job_id, "cancelled", None).await
            {
                tracing::error!(
                    "CRITICAL: Failed to report cancelled status for job {job_id}: {se}"
                );
            }
        } else {
            tracing::error!("Job {job_id} failed: {e:#}");
            if let Err(le) = super::logs::upload_chunk(
                client,
                config,
                token,
                job_id,
                LogChunk {
                    chunk_index: -1,
                    step_order: -1,
                    content: format!("==> ERROR\n{e:#}\n"),
                    level: Some(LogLevel::Error),
                },
            )
            .await
            {
                tracing::warn!("Job {job_id}: failed to upload error log chunk: {le}");
            }
            if let Err(se) = update_job_status(client, config, token, job_id, "failed", None).await
            {
                tracing::error!("CRITICAL: Failed to report failed status for job {job_id}: {se}");
            }
        }
    }
    Ok(())
}

/// One claim poll's outcome: an optional job plus cancel notices the Manager
/// delivered on the same response (replacing the old per-job 2s probes).
#[derive(Default)]
pub struct PollOutcome {
    pub task: Option<serde_json::Value>,
    pub cancelled_jobs: Vec<String>,
    pub retry_after_ms: Option<u64>,
}

/// Parse a poll response body (`data` object) into task + cancel notices.
/// `job_id` presence distinguishes a job payload from a cancel-only payload.
pub fn parse_poll_data(data: serde_json::Value) -> PollOutcome {
    if data.is_null() {
        return PollOutcome::default();
    }
    let retry_after_ms = data.get("retry_after_ms").and_then(|v| v.as_u64());
    let cancelled_jobs = data
        .get("cancelled_jobs")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let task = if data.get("job_id").is_some() {
        Some(data)
    } else {
        None
    };
    PollOutcome {
        task,
        cancelled_jobs,
        retry_after_ms,
    }
}

/// Live jobs' cancel signals, shared between the claim loop (which receives
/// Manager notices) and the job tasks (which race steps against them).
pub type CancelRegistry = Arc<Mutex<HashMap<String, watch::Sender<bool>>>>;

/// Fire (and consume) a job's cancel signal. Unknown ids — already finished
/// locally, or reported after the task exited — are dropped: the Manager's
/// terminal-state protection settles the verdict either way.
pub fn signal_cancel(registry: &CancelRegistry, job_id: &str) {
    if let Some(tx) = registry.lock().expect("cancel registry").remove(job_id) {
        let _ = tx.send(true);
    }
}

/// Resolves once the Manager reports this job cancelled via the claim channel.
pub async fn cancel_triggered(rx: &mut watch::Receiver<bool>) -> bool {
    loop {
        if *rx.borrow() {
            return true;
        }
        if rx.changed().await.is_err() {
            return std::future::pending().await;
        }
    }
}

/// Idle runners can hold one long poll for the full client timeout budget.
/// Active runners refresh their capacity snapshot frequently so a completed
/// local job makes the next claim eligible without cancelling an in-flight poll.
pub fn poll_wait_seconds(active_jobs: u32) -> u64 {
    if active_jobs == 0 {
        45
    } else {
        2
    }
}

async fn poll_for_task(
    client: &Client,
    config: &Config,
    token: &str,
    cancel_acks: &[String],
    active_jobs: u32,
) -> anyhow::Result<PollOutcome> {
    let wait_seconds = poll_wait_seconds(active_jobs);
    let mut request = client
        .get(format!("{}/api/v1/runners/poll", config.runner.manager_url))
        .header("Authorization", format!("Bearer {token}"))
        .header("prefer", format!("wait={wait_seconds}"))
        .header("x-buz-active-jobs", active_jobs.to_string())
        .timeout(Duration::from_secs(50));
    if !cancel_acks.is_empty() {
        request = request.header("x-buz-cancel-ack", cancel_acks.join(","));
    }
    let resp = request
        .send()
        .await
        .map_err(|e| super::friendly_reqwest_error(e, &config.runner.manager_url, "Poll failed"))?;

    if resp.status() == 204 {
        return Ok(PollOutcome::default());
    }

    if !resp.status().is_success() {
        // Distinguish auth failures from transient errors
        let status = resp.status();
        if status.as_u16() == 401 || status.as_u16() == 403 {
            anyhow::bail!("auth error ({status}) — token may have expired, re‑register needed");
        }
        anyhow::bail!("poll returned {status}");
    }

    let body = resp.json::<serde_json::Value>().await?;
    Ok(parse_poll_data(
        body.get("data").cloned().unwrap_or_default(),
    ))
}

pub async fn execute_job(
    client: &Client,
    config: &Config,
    token: &str,
    task: &serde_json::Value,
    cancel_rx: &mut watch::Receiver<bool>,
) -> anyhow::Result<()> {
    let tasks = task
        .get("tasks")
        .and_then(|tasks| tasks.as_array())
        .ok_or_else(|| anyhow::anyhow!("missing tasks array"))?;
    let job_id = task
        .get("job_id")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("missing job_id"))?;
    let sandbox_handle = if config.execution_mode == shared::protocol::RunnerExecutionMode::Host {
        None
    } else {
        let unsupported: Vec<String> = tasks
            .iter()
            .enumerate()
            .filter_map(|(index, step)| {
                let engine = step
                    .get("exec_type")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("shell");
                if matches!(engine, "ssh" | "docker") {
                    Some(format!("step {} ({engine})", index + 1))
                } else {
                    None
                }
            })
            .collect();
        anyhow::ensure!(
            unsupported.is_empty(),
            "job mode '{}' does not support SSH, Docker-engine, or host-side config preparation: {}",
            config.execution_mode.as_str(),
            unsupported.join(", ")
        );
        // The Manager froze `runner.resources` into the claim payload after the
        // team-grant check; sandboxd resolves the envelope from its applied
        // document (empty = default; unknown = error, never a downgrade).
        let resource_profile = task
            .get("resource_profile")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        Some(std::sync::Arc::new(
            crate::client::sandbox_client::create_job(
                config,
                job_id,
                &config.runner.workspace_dir,
                resource_profile,
            )
            .await?,
        ))
    };
    let mut prefetch = crate::client::image_prefetch::ImagePrefetch::start(tasks, config, job_id);

    // `needs:` DAG: when any step declares dependencies, the whole job is
    // scheduled as a graph (ready steps run concurrently). Pipelines without
    // `needs` keep the sequential loop — unchanged, byte for byte.
    if let Some(plan) = dag::DagPlan::build(tasks)? {
        let result = dag::execute(
            client,
            config,
            token,
            task,
            tasks,
            plan,
            cancel_rx,
            &sandbox_handle,
        )
        .await;
        if let Some(prefetch) = prefetch.as_mut() {
            prefetch.abort_and_drain().await;
        }
        if let Some(handle) = sandbox_handle.as_deref() {
            if let Err(error) =
                crate::client::sandbox_client::destroy_job(&config.sandbox, handle).await
            {
                tracing::error!(job_id, error = %error, "failed to destroy job sandbox");
            }
        }
        return result;
    }

    let result = execute_job_inner(
        client,
        config,
        token,
        task,
        cancel_rx,
        &mut prefetch,
        sandbox_handle.as_deref(),
    )
    .await;
    if let Some(prefetch) = prefetch.as_mut() {
        prefetch.abort_and_drain().await;
    }
    if let Some(handle) = sandbox_handle.as_ref() {
        if let Err(error) =
            crate::client::sandbox_client::destroy_job(&config.sandbox, handle).await
        {
            tracing::error!(job_id, error = %error, "failed to destroy job sandbox");
        }
    }
    result
}

/// Base process env for every step: trigger inputs + pipeline `env:` (already
/// expanded at claim — may contain secret values via mapping) + `BUZ_*` job
/// metadata, forced non-interactive. Secrets are NOT auto-injected as `$NAME`
/// — Manager expands `env: KEY: ${secrets.NAME}` before claim; scripts use
/// `$KEY`.
fn build_base_env(
    task: &serde_json::Value,
    job_id: &str,
    manager_url: &str,
) -> std::collections::HashMap<String, String> {
    let mut base_env: std::collections::HashMap<String, String> = task
        .get("inputs")
        .and_then(|v| v.as_object())
        .map(|obj| {
            obj.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default();

    if let Some(env_obj) = task.get("env").and_then(|v| v.as_object()) {
        for (k, v) in env_obj {
            if let Some(s) = v.as_str() {
                base_env.insert(k.clone(), s.to_string());
            }
        }
    }

    // BUZ_* job metadata (ADR §5.2) — plugins render "build #<job> passed"
    // style messages from these. Never secret values, safe to echo.
    for (key, value) in buz_metadata_env(task, job_id, manager_url) {
        base_env.insert(key, value);
    }

    // Force non-interactive mode for common tools. When a step tries to read
    // from stdin (e.g. `git clone` prompting for credentials), these env vars
    // make the tool fail immediately with a clear error instead of hanging.
    base_env
        .entry("GIT_TERMINAL_PROMPT".to_string())
        .or_insert_with(|| "0".to_string());
    base_env
        .entry("DEBIAN_FRONTEND".to_string())
        .or_insert_with(|| "noninteractive".to_string());
    base_env
}

/// Secret catalog keys + raw mask values for the whole job (log redaction
/// only — never process env injection).
fn job_secret_mask_inputs(task: &serde_json::Value) -> (HashSet<String>, Vec<String>) {
    let mut pipeline_secret_keys: HashSet<String> = HashSet::new();
    let mut secret_mask_values: Vec<String> = Vec::new();
    if let Some(secrets_obj) = task.get("secrets").and_then(|v| v.as_object()) {
        for (k, v) in secrets_obj {
            if let Some(s) = v.as_str() {
                pipeline_secret_keys.insert(k.clone());
                if !s.is_empty() {
                    secret_mask_values.push(s.to_string());
                }
            }
        }
    }
    // Env keys that Manager marked as sourced from `${secrets.…}` mappings.
    if let Some(keys) = task.get("secret_env_keys").and_then(|v| v.as_array()) {
        for key in keys.iter().filter_map(|v| v.as_str()) {
            if !key.is_empty() {
                pipeline_secret_keys.insert(key.to_string());
            }
        }
    }
    // Trigger inputs marked masking: true — same redaction as secrets in logs/echo.
    if let Some(keys) = task.get("masking_input_keys").and_then(|v| v.as_array()) {
        for key in keys.iter().filter_map(|v| v.as_str()) {
            if key.is_empty() {
                continue;
            }
            pipeline_secret_keys.insert(key.to_string());
            // Engines also export INPUT_<NAME>; cover both key forms in env echo.
            pipeline_secret_keys.insert(shared::protocol::input_env_key(key));
        }
    }
    (pipeline_secret_keys, secret_mask_values)
}

async fn execute_job_inner(
    client: &Client,
    config: &Config,
    token: &str,
    task: &serde_json::Value,
    cancel_rx: &mut watch::Receiver<bool>,
    prefetch: &mut Option<crate::client::image_prefetch::ImagePrefetch>,
    sandbox_handle: Option<&crate::client::sandbox_client::SandboxHandle>,
) -> anyhow::Result<()> {
    let job_id = task["job_id"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("missing job_id"))?;

    // Per-job Docker CLI config isolation (runner.toml `[engine]`, machine-local).
    // `Some(dir)` → shell/docker steps get DOCKER_CONFIG inside the job workspace;
    // ssh steps get a job-named /tmp dir on each target host. `None` = legacy
    // host-global `~/.docker` behavior.
    let docker_config_dir: Option<String> = match config.engine.docker_config_isolation {
        DockerConfigIsolation::PerJob => Some(job_docker_config_dir(&config.runner.workspace_dir)),
        DockerConfigIsolation::Shared => None,
    };

    // Admin-only debug flag: when set, skip all log redaction for this job.
    // The Manager only sets it for platform-admin-triggered runs; debug-job
    // logs are additionally gated to admins on the read side.
    let debug_unredact = task
        .get("debug_unredact")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    // Verbose execution mode: available to any user — adds per-step duration,
    // exit code, stage markers and full step config to the logs while keeping
    // secret redaction fully intact.
    let verbose = task
        .get("verbose")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let tasks = task["tasks"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("missing tasks array"))?;

    // Host-mode isolation: `[engine] run_as_user` executes shell steps as a
    // separate OS account. Fail once here (clear sudoers message) instead of
    // surfacing a sudo failure per step. Sandbox modes have their own boundary.
    if config.execution_mode == shared::protocol::RunnerExecutionMode::Host {
        if config.engine.netns_isolation {
            engine::shell::preflight_netns().await?;
        }
        if let Some(user) = config
            .engine
            .run_as_user
            .as_deref()
            .map(str::trim)
            .filter(|user| !user.is_empty())
        {
            engine::shell::preflight_run_as_user(user).await?;
        }
    }

    // Job-start runner identity (version + OS) — makes every job's logs
    // self-describing for troubleshooting. Non-fatal if the upload fails.
    let _ = super::logs::upload_chunk(
        client,
        config,
        token,
        job_id,
        LogChunk {
            chunk_index: LOG_RUNNER_BANNER,
            step_order: 0,
            content: build_runner_banner(),
            level: Some(LogLevel::Info),
        },
    )
    .await;

    // Build base env from trigger inputs + expanded pipeline `env:` only.
    // Secrets are NOT auto-injected as `$NAME` — Manager expands
    // `env: KEY: ${secrets.NAME}` before claim; scripts use `$KEY`.
    let mut base_env = build_base_env(task, job_id, &config.runner.manager_url);

    // Job-scoped dynamic env file ($BUZ_ENV, GHA $GITHUB_ENV style): steps
    // append KEY=value lines; parsed after each step and merged into the base
    // env of subsequent steps. Lives inside the job workspace so Docker
    // containers reach it via the workspace mount (/buz/workspace/.buz/env).
    let buz_env_path = format!(
        "{}/.buz/env",
        config.runner.workspace_dir.trim_end_matches('/')
    );
    if let Some(parent) = std::path::Path::new(&buz_env_path).parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            tracing::warn!(
                "Failed to create {}: {e} — $BUZ_ENV disabled for this job",
                parent.display()
            );
        }
    }
    let _ = std::fs::remove_file(&buz_env_path);
    base_env.insert("BUZ_ENV".into(), buz_env_path.clone());

    // Secret catalog keys/values for log redaction (not process env injection).
    let (pipeline_secret_keys, secret_mask_values) = job_secret_mask_inputs(task);
    // Tracks whether any step failed under fail_strategy = "continue". The job
    // must end as failed even though execution continued past the failing step.
    let mut any_step_failed = false;

    for (idx, step) in tasks.iter().enumerate() {
        let log = StepLogger::new(client, config, token, job_id, idx);
        let spec = parse_step_spec(idx, step);
        let docker_socket_path = validate_step_constraints(&log, config, step, &spec).await?;
        let secrets = build_step_secrets(
            &log,
            step,
            &base_env,
            &pipeline_secret_keys,
            &secret_mask_values,
            debug_unredact,
        )
        .await?;

        let step_started = std::time::Instant::now();
        announce_step(&log, config, step, &spec, &secrets.env, verbose).await?;

        let exec_type = spec.exec_type;
        let step_name = spec.name;
        let max_attempts = spec.retry.saturating_add(1);
        // A retry continues after the failed attempt's streamed output: the
        // chunk index is unique per job, so restarting at LOG_STREAM_START
        // would overwrite the earlier attempt's lines.
        let mut stream_start = log.log_base + LOG_STREAM_START;
        let mut attempt: u32 = 0;

        loop {
            attempt += 1;

            // ── One attempt: fresh SSH sessions, engine run, no status verdict ──
            let attempt_result: anyhow::Result<StepExecution> = async {
                let mut ssh_sessions =
                    open_ssh_sessions(&log, config, step, &spec, cancel_rx).await?;

                // Verbose: exec-stage marker only (STEP CONFIG already uploaded).
                if verbose {
                    log.emit(LOG_STEP_VERBOSE_EXEC, build_verbose_stage("exec"))
                        .await;
                }

                let run = StepRun {
                    log: &log,
                    config,
                    spec: spec.clone(),
                    env: secrets.env.clone(),
                    catalog: &secrets.catalog,
                    secret_values: &secrets.secret_values,
                    forced_values: &secrets.forced_values,
                    docker_config_dir: docker_config_dir.as_deref(),
                    buz_env_path: &buz_env_path,
                    sandbox_handle,
                    stream_start,
                };
                match exec_type {
                    "local" | "shell" => run_shell_step(run, cancel_rx).await,
                    "ssh" => run_ssh_step(run, &mut ssh_sessions, cancel_rx).await,
                    "docker" => {
                        run_docker_step(run, prefetch, docker_socket_path.as_deref(), cancel_rx)
                            .await
                    }
                    "wasm" => anyhow::bail!(
                        "wasm engine is not implemented in this release — step '{step_name}' cannot run"
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
                        let marker = build_retry_marker(step_name, attempt, max_attempts, &detail);
                        tracing::warn!(
                            step = step_name,
                            attempt,
                            max_attempts,
                            exit_code = execution.output.exit_code,
                            "step failed — retrying"
                        );
                        log.emit(LOG_STEP_RETRY, marker).await;
                        continue;
                    }
                    any_step_failed |= finalize_step(
                        &log,
                        &spec,
                        execution,
                        &secrets.forced_values,
                        sandbox_handle,
                        verbose,
                        step_started,
                    )
                    .await?;
                    break;
                }
                Err(error) => {
                    // Cancellation is not retryable. `cancel_rx` is only read,
                    // never consumed, so the next attempt still observes it.
                    let cancelled = *cancel_rx.borrow();
                    if attempt < max_attempts && !cancelled {
                        let detail = mask_output(
                            &format!("{error:#}"),
                            &secrets.secret_values,
                            &secrets.forced_values,
                        );
                        let marker =
                            build_retry_marker(step_name, attempt, max_attempts, detail.trim());
                        tracing::warn!(
                            step = step_name,
                            attempt,
                            max_attempts,
                            error = %error,
                            "step attempt failed — retrying"
                        );
                        log.emit(LOG_STEP_RETRY, marker).await;
                        continue;
                    }
                    return Err(error);
                }
            }
        }

        // ── $BUZ_ENV merge: what this step wrote becomes env for later steps ──
        // Shell and Docker steps share the same physical file (the Docker
        // container sees it via the workspace mount). Values are not
        // auto-masked — route sensitive data through Secrets instead.
        let dynamic_env = match sandbox_handle {
            Some(handle) if handle.mode() == shared::protocol::RunnerExecutionMode::Vm => {
                crate::client::sandbox_client::read_job_file(&config.sandbox, handle, ".buz/env")
                    .await
                    .and_then(|content| parse_buz_env_content(&content))
            }
            _ => parse_buz_env_file(&buz_env_path),
        };
        match dynamic_env {
            Ok(dynamic) => {
                for (key, value) in dynamic {
                    base_env.insert(key, value);
                }
            }
            Err(e) => {
                tracing::warn!("Failed to parse $BUZ_ENV after step '{step_name}': {e}");
            }
        }
    }

    // fail_strategy = "continue" may leave failed steps behind; the job must
    // still end as failed rather than silently succeeding.
    if any_step_failed {
        tracing::warn!("Job {job_id} finished with failed step(s)");
        update_job_status(client, config, token, job_id, "failed", None).await?;
        return Ok(());
    }

    // All steps completed successfully
    update_job_status(client, config, token, job_id, "success", None).await?;
    tracing::info!("Job {job_id} completed successfully");

    Ok(())
}

/// Parsed per-step fields from the claim payload (`tasks[i]`). Strings borrow
/// from the step JSON; collections are owned. Cloned into `StepRun` so the
/// loop keeps `spec` for `finalize_step`.
#[derive(Clone)]
pub(super) struct PluginSpec<'a> {
    pub(super) idx: usize,
    pub(super) step: &'a serde_json::Value,
    pub(super) name: &'a str,
    pub(super) command: &'a str,
    pub(super) work_dir: Option<&'a str>,
    pub(super) exec_type: &'a str,
    pub(super) fail_strategy: &'a str,
    pub(super) timeout_secs: u64,
    pub(super) timeout: Option<Duration>,
    /// Automatic retries after a non-zero exit (`retry` in the step YAML).
    pub(super) retry: u32,
    pub(super) privileged: bool,
    pub(super) volumes: Vec<String>,
    pub(super) capabilities: Vec<String>,
}

pub(super) fn parse_step_spec(idx: usize, step: &serde_json::Value) -> PluginSpec<'_> {
    let timeout_secs = step["timeout_seconds"].as_u64().unwrap_or(0);
    PluginSpec {
        idx,
        step,
        name: step["name"].as_str().unwrap_or("unnamed"),
        command: step["command_template"].as_str().unwrap_or(""),
        work_dir: step["work_dir"].as_str().filter(|s| !s.is_empty()),
        exec_type: step["exec_type"].as_str().unwrap_or("shell"),
        fail_strategy: step["fail_strategy"].as_str().unwrap_or("stop"),
        timeout_secs,
        timeout: (timeout_secs > 0).then(|| Duration::from_secs(timeout_secs)),
        retry: step["retry_count"]
            .as_u64()
            .unwrap_or(0)
            .min(u32::MAX as u64) as u32,
        privileged: step
            .get("privileged")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        volumes: string_list(step, "volumes"),
        capabilities: string_list(step, "capabilities"),
    }
}

fn string_list(step: &serde_json::Value, key: &str) -> Vec<String> {
    step.get(key)
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Enforce engine policy before anything runs: Docker mount/socket policy,
/// image preflight, and work_dir containment. Returns the Docker socket path
/// for docker steps. Policy errors report the step failed via `fail_step`;
/// the work_dir check keeps its historical status-only failure path.
pub(super) async fn validate_step_constraints(
    log: &StepLogger,
    config: &Config,
    step: &serde_json::Value,
    spec: &PluginSpec<'_>,
) -> anyhow::Result<Option<std::path::PathBuf>> {
    let docker_socket_path = if spec.exec_type == "docker" {
        match engine::docker::validate_step_policy(
            &config.runner,
            &spec.capabilities,
            &spec.volumes,
            spec.privileged,
        ) {
            Ok(value) => value,
            Err(e) => return Err(log.fail_step(format!("{e:#}")).await),
        }
    } else {
        anyhow::ensure!(
            spec.capabilities.is_empty(),
            "execution capabilities are only valid for Docker engine steps"
        );
        None
    };
    if spec.exec_type == "docker" {
        let image = step.get("image").and_then(|v| v.as_str()).unwrap_or("");
        if let Err(e) = engine::docker::preflight(config.engine.docker_enabled, image) {
            return Err(log.fail_step(format!("{e:#}")).await);
        }
    }
    if spec
        .work_dir
        .is_some_and(|dir| !is_safe_workspace_subdir(dir))
    {
        let msg = format!(
            "step work_dir must be a relative path inside the job workspace: {:?}",
            spec.work_dir
        );
        update_job_status(
            &log.client,
            &log.config,
            &log.token,
            &log.job_id,
            "failed",
            Some(log.step_order),
        )
        .await?;
        anyhow::bail!(msg);
    }
    Ok(docker_socket_path)
}

/// Per-step env + redaction state: base env plus the step `env:` map, the
/// secret catalog (keys + forced values), and the resolved mask lists the
/// engine runners and `finalize_step` mask with.
pub(super) struct StepSecrets {
    env: std::collections::HashMap<String, String>,
    catalog: SecretCatalog,
    secret_values: Vec<String>,
    forced_values: Vec<String>,
}

/// ── Step env: base + expanded step `env:` map (no auto-inject from secrets:) ──
pub(super) async fn build_step_secrets(
    log: &StepLogger,
    step: &serde_json::Value,
    base_env: &std::collections::HashMap<String, String>,
    pipeline_secret_keys: &HashSet<String>,
    secret_mask_values: &[String],
    debug_unredact: bool,
) -> anyhow::Result<StepSecrets> {
    let mut env = base_env.clone();
    let mut catalog = SecretCatalog::new();
    catalog.add_keys(pipeline_secret_keys.iter().cloned());
    for v in secret_mask_values {
        catalog.add_value(v.clone());
    }
    if let Some(se_obj) = step.get("step_env").and_then(|v| v.as_object()) {
        for (k, v) in se_obj {
            if let Some(s) = v.as_str() {
                // `with:` values may reference dynamic env written by
                // earlier steps ($BUZ_ENV) via `${env.NAME}` — resolve
                // now, when those values exist. Fail closed on unknown.
                if k.starts_with("INPUT_") && s.contains("${env.") {
                    match shared::secret_ref::expand_env_refs(s, &env) {
                        Ok(expanded) => {
                            env.insert(k.clone(), expanded);
                        }
                        Err(msg) => {
                            let error =
                                format!("with input {}: {msg}", k.trim_start_matches("INPUT_"));
                            return Err(log.fail_step(error).await);
                        }
                    }
                } else {
                    env.insert(k.clone(), s.to_string());
                }
            }
        }
    }
    if let Some(keys) = step.get("secret_env_keys").and_then(|v| v.as_array()) {
        for key in keys.iter().filter_map(|v| v.as_str()) {
            if !key.is_empty() {
                catalog.add_key(key.to_string());
            }
        }
    }
    if let Some(keys) = step.get("masking_env_keys").and_then(|v| v.as_array()) {
        for key in keys.iter().filter_map(|v| v.as_str()) {
            if !key.is_empty() {
                catalog.add_key(key.to_string());
            }
        }
    }
    // step_secrets values are for render/mask only — do not inject as `$NAME`.
    if let Some(ss_obj) = step.get("step_secrets").and_then(|v| v.as_object()) {
        for (k, v) in ss_obj {
            if let Some(s) = v.as_str() {
                catalog.add_key(k.clone());
                if !s.is_empty() {
                    catalog.add_value(s.to_string());
                }
            }
        }
    }

    // Sensitive values are registered by the Manager per step in
    // `log_mask_values` (registry passwords, tokens, …).
    if let Some(values) = step.get("log_mask_values").and_then(|v| v.as_array()) {
        for v in values.iter().filter_map(|x| x.as_str()) {
            catalog.add_value(v.to_string());
        }
    }

    // Debug mode: RUN echo + streams show real values, so the catalog is emptied.
    if debug_unredact {
        catalog = SecretCatalog::new();
    }

    // Collect values for log masking — secrets and masking inputs.
    let secret_values = catalog.secret_values(&env);
    // Forced values: rendered `.env` secret values that must be masked
    // regardless of length (plaintext `.env` content may be echoed by the
    // run block, e.g. `cat .env`).
    let forced_values = catalog.forced_secret_values();
    Ok(StepSecrets {
        env,
        catalog,
        secret_values,
        forced_values,
    })
}

/// Verbose stage markers, engine enablement, the "running" status transition,
/// and the Manager-attached fail-fast errors (input validation, env
/// expansion, host resolution, config render). Every error here has
/// already been reported via StepLogger before returning.
#[allow(clippy::too_many_arguments)] // job execution context: step, sandbox, secrets, logging handles
pub(super) async fn announce_step(
    log: &StepLogger,
    config: &Config,
    step: &serde_json::Value,
    spec: &PluginSpec<'_>,
    env: &std::collections::HashMap<String, String>,
    verbose: bool,
) -> anyhow::Result<()> {
    // Verbose mode: stage marker + STEP CONFIG up front (before prepare /
    // fail-fast) so a prepare-stage failure still shows the step shape.
    // Debug alone does not emit these — it only unmasks values.
    if verbose {
        log.emit(LOG_STEP_VERBOSE_PREPARE, build_verbose_stage("prepare"))
            .await;
        let cfg = build_verbose_step_config(step, spec.exec_type);
        log.emit(LOG_STEP_VERBOSE_CONFIG, cfg).await;
    }

    if let Err(e) = config.engine.ensure_enabled(spec.exec_type) {
        return Err(log.fail_step(format!("{e:#}")).await);
    }

    let env_count = env.len();
    tracing::info!(
        "Step {idx}: {name} ({exec_type}){}",
        if env_count > 0 {
            format!(" [with {env_count} env vars]")
        } else {
            String::new()
        },
        idx = spec.idx,
        name = spec.name,
        exec_type = spec.exec_type,
    );

    update_job_status(
        &log.client,
        &log.config,
        &log.token,
        &log.job_id,
        "running",
        Some(log.step_order),
    )
    .await?;

    if let Some(err) = step
        .get("input_validation_error")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
    {
        return Err(log
            .fail_step(format!("marketplace input validation: {err}"))
            .await);
    }

    // Fail fast if Manager could not resolve step `env:` (`${secrets.X}`).
    if let Some(err) = step
        .get("env_expand_error")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
    {
        return Err(log.fail_step(format!("step env: {err}")).await);
    }

    if let Some(err) = step
        .get("host_resolution_error")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
    {
        return Err(log.fail_step(format!("SSH host resolution: {err}")).await);
    }

    Ok(())
}

/// Open the step's SSH sessions (one TCP connect per host, shared by config
/// prepare and the run block to avoid reconnect storms / fail2ban /
/// MaxStartups). Non-SSH steps get an empty vector. Cancel aborts in-flight
/// connects and reports the step cancelled.
pub(super) async fn open_ssh_sessions(
    log: &StepLogger,
    config: &Config,
    step: &serde_json::Value,
    spec: &PluginSpec<'_>,
    cancel_rx: &mut watch::Receiver<bool>,
) -> anyhow::Result<Vec<SshSession>> {
    let mut ssh_sessions: Vec<SshSession> = Vec::new();
    if spec.exec_type != "ssh" {
        return Ok(ssh_sessions);
    }
    let conns = resolve_ssh_conns(step, config);
    if conns.is_empty() {
        return Ok(ssh_sessions);
    }
    // Multi-host: connect in parallel; cancel aborts all in-flight connects.
    let connect_result = tokio::select! {
        result = engine::ssh::connect_all(&conns, ssh_hosts_parallel(step), ssh_hosts_max_parallel(step)) => Some(result),
        _ = cancel_triggered(cancel_rx) => None,
    };
    match connect_result {
        None => {
            return Err(log.cancel_step().await);
        }
        Some(Err(e)) => {
            return Err(log.fail_step(format!("{e:#}")).await);
        }
        Some(Ok(mut sessions)) => {
            tracing::info!(
                hosts = sessions.len(),
                "SSH step '{}': opened step session(s)",
                spec.name
            );
            let targets = {
                let probe = engine::ssh::probe_workspaces(&mut sessions, spec.timeout);
                tokio::select! {
                    t = probe => Some(t),
                    _ = cancel_triggered(cancel_rx) => None,
                }
            };
            match targets {
                None => {
                    drop(sessions);
                    return Err(log.cancel_step().await);
                }
                Some(targets) => {
                    let banner = build_ssh_banner(&targets);
                    if !banner.is_empty() {
                        log.emit(LOG_LOGIN, banner).await;
                    }
                    ssh_sessions = sessions;
                }
            }
        }
    }
    Ok(ssh_sessions)
}

/// Config prepare: config_render (render-only).
/// Templates already at dest (shell in place / SSH fetch → render on Runner
/// → upload). See config-block.md.
///
/// Prepare failures MUST be reported with this step's `step_order` (not
/// the job-level -1 catch in `run_one_job`) so Job Detail attaches the
/// ERROR to the correct terminal without frontend guessing.
/// Shared inputs for one step's engine execution. Each engine mutates its own
/// copy of `env`; cancel racing happens on the separately passed receiver.
pub(super) struct StepRun<'a> {
    pub(super) log: &'a StepLogger,
    pub(super) config: &'a Config,
    pub(super) spec: PluginSpec<'a>,
    pub(super) env: std::collections::HashMap<String, String>,
    pub(super) catalog: &'a SecretCatalog,
    pub(super) secret_values: &'a [String],
    pub(super) forced_values: &'a [String],
    pub(super) docker_config_dir: Option<&'a str>,
    pub(super) buz_env_path: &'a str,
    pub(super) sandbox_handle: Option<&'a crate::client::sandbox_client::SandboxHandle>,
    /// First stream chunk index for this attempt. Carried across retries so a
    /// later attempt appends after the failed one (the chunk index is unique
    /// per job, so reusing it would overwrite the earlier output).
    stream_start: i32,
}

/// What one engine produced for the step. `output_secret_values` carries the
/// mask list for the summary chunk — SSH swaps in its own catalog view so
/// remote-exported step env values are covered.
pub(super) struct StepExecution {
    pub(super) output: engine::shell::ShellOutput,
    pub(super) was_cancelled: bool,
    pub(super) output_secret_values: Vec<String>,
    /// Index the next attempt's stream should start from.
    pub(super) next_stream: i32,
    /// `Some(count)` when the streamed output overflowed the per-step log
    /// window — surfaced on the step's summary and failure message.
    pub(super) omitted: Option<u64>,
}

/// `local`/`shell` engine: inside the job sandbox when one exists, otherwise
/// streamed on the runner host.
pub(super) async fn run_shell_step(
    run: StepRun<'_>,
    cancel_rx: &mut watch::Receiver<bool>,
) -> anyhow::Result<StepExecution> {
    let exec_type = run.spec.exec_type;
    if let Some(handle) = run.sandbox_handle {
        let mut sandbox_env = run.env.clone();
        sandbox_env.insert("BUZ_STEP_ENGINE".into(), exec_type.to_string());
        // Let build scripts size themselves (e.g. make -j) by the effective
        // envelope the supervisor applied, not the one the pipeline asked for.
        sandbox_env.insert("BUZ_RESOURCE_PROFILE".into(), handle.profile().to_string());
        let buz_file = std::path::Path::new(&run.buz_env_path)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "env".into());
        sandbox_env.insert(
            "BUZ_ENV".into(),
            format!("{}/.buz/{buz_file}", engine::docker::CONTAINER_WORKSPACE),
        );
        sandbox_env.insert(
            "DOCKER_CONFIG".into(),
            format!("{}/.docker", engine::docker::CONTAINER_WORKSPACE),
        );
        let echo_line = build_step_echo(
            exec_type,
            run.spec.command,
            &sandbox_env,
            run.catalog,
            run.spec.work_dir,
        );
        run.log.emit(LOG_ECHO, echo_line).await;
        let environment = serde_json::to_value(&sandbox_env)?;
        let timeout_seconds = if run.spec.timeout.is_some() {
            run.spec.timeout_secs
        } else {
            0
        };
        let (output, was_cancelled, next_stream, omitted) = run_sandbox_step(
            run.log,
            run.config,
            run.spec.command,
            run.spec.work_dir.unwrap_or(""),
            &environment,
            timeout_seconds,
            run.secret_values,
            run.forced_values,
            cancel_rx,
            handle,
            run.stream_start,
        )
        .await?;
        return Ok(StepExecution {
            output,
            was_cancelled,
            output_secret_values: run.secret_values.to_vec(),
            next_stream,
            omitted,
        });
    }

    let mut env = run.env.clone();
    if let Some(dir) = run.docker_config_dir {
        env.insert("DOCKER_CONFIG".into(), dir.to_string());
    }
    env.insert("BUZ_STEP_ENGINE".into(), exec_type.to_string());
    // The host runner reads $BUZ_ENV at its real workspace path (the sandbox
    // branch above swaps in the container mount path instead).
    env.insert("BUZ_ENV".into(), run.buz_env_path.to_string());

    // Debug echo — show the command before execution (like bash -x)
    {
        let echo_line = build_step_echo(
            exec_type,
            run.spec.command,
            &env,
            run.catalog,
            run.spec.work_dir,
        );
        run.log.emit(LOG_ECHO, echo_line).await;
    }

    let mut cancel_rx_step = cancel_rx.clone();
    let jid = run.log.job_id.clone();
    let sname = run.spec.name.to_string();

    let exec_result = engine::shell::execute_streaming(
        run.spec.command,
        run.spec.work_dir,
        Some(&env),
        run.config.engine.run_as_user.as_deref(),
        run.config.engine.netns_isolation,
    )
    .await;
    let (mut child, mut output_rx, read_guard) = match exec_result {
        Ok(v) => v,
        Err(e) => return Err(e),
    };

    // Background task: stream output chunks to the manager in real time.
    // It returns the next free chunk index so a retry can append after this
    // attempt instead of overwriting it.
    let upload_task = {
        let logger = run.log.clone();
        let secret_values = run.secret_values.to_vec();
        let forced_values = run.forced_values.to_vec();
        let start = run.stream_start;
        tokio::spawn(async move {
            logger
                .stream_chunks(&mut output_rx, &secret_values, &forced_values, start)
                .await
        })
    };

    // Race: child completion vs cancellation vs timeout
    let (exit_code, was_cancelled_local, was_timeout, stream_outcome) = tokio::select! {
        status = child.wait() => {
            let code = status?.code().unwrap_or(-1);
            // Ensure all pipe data is read before proceeding
            let _ = read_guard.join().await;
            let outcome = upload_task
                .await
                .unwrap_or(step_log::StreamOutcome { next_index: run.stream_start, omitted: None });
            (code, false, false, outcome)
        }
        _ = cancel_triggered(&mut cancel_rx_step) => {
            tracing::warn!("Job {jid} cancelled during step '{sname}' — killing");
            let _ = child.kill().await;
            let _ = child.wait().await;
            upload_task.abort();
            (-1, true, false, step_log::StreamOutcome { next_index: run.stream_start, omitted: None })
        }
        _ = tokio::time::sleep(run.spec.timeout.unwrap_or(Duration::from_secs(3600))), if run.spec.timeout.is_some() => {
            tracing::warn!("Step '{sname}' timed out after {}s — killing process", run.spec.timeout_secs);
            let _ = child.kill().await;
            let _ = child.wait().await;
            let _ = read_guard.join().await;
            upload_task.abort();
            (-1, false, true, step_log::StreamOutcome { next_index: run.stream_start, omitted: None })
        }
    };

    Ok(StepExecution {
        output: engine::shell::ShellOutput {
            stdout: String::new(),
            stderr: if was_cancelled_local {
                "==> CANCELLED\n".into()
            } else if was_timeout {
                step_timeout_content(run.spec.timeout_secs)
            } else {
                String::new()
            },
            exit_code: if was_cancelled_local || was_timeout {
                -1
            } else {
                exit_code
            },
        },
        was_cancelled: was_cancelled_local,
        output_secret_values: run.secret_values.to_vec(),
        next_stream: stream_outcome.next_index,
        omitted: stream_outcome.omitted,
    })
}

/// SSH engine: one combined output upload (masked with the SSH catalog);
/// parallel fan-out per host inside `execute_ssh_step`.
pub(super) async fn run_ssh_step(
    run: StepRun<'_>,
    ssh_sessions: &mut Vec<SshSession>,
    cancel_rx: &mut watch::Receiver<bool>,
) -> anyhow::Result<StepExecution> {
    if ssh_sessions.is_empty() {
        let msg = if run.spec.step["host"].is_null() {
            format!(
                "SSH step '{}' missing host connection details",
                run.spec.name
            )
        } else {
            format!("SSH step '{}': no hosts resolved from the database. Register your hosts in the Manager UI first.", run.spec.name)
        };
        return Err(run.log.fail_step(msg).await);
    }

    let mut ssh_env = run.env.clone();
    let cmd = run.spec.command.to_string();
    let mut cancel_rx_step = cancel_rx.clone();

    // Debug echo — show the command before execution (like bash -x)
    {
        let echo_line = build_step_echo(
            run.spec.exec_type,
            &cmd,
            &ssh_env,
            run.catalog,
            run.spec.work_dir,
        );
        run.log.emit(LOG_ECHO, echo_line).await;
    }

    // Remote output is uploaded combined — mask with the step catalog
    // (covers step env values exported into the remote shell).
    let output_secret_values = run.catalog.secret_values(&ssh_env);

    let jid = run.log.job_id.clone();
    let sname = run.spec.name.to_string();
    let sname2 = sname.clone();
    // Job-scoped remote Docker config (per-host copy of the same
    // path): plugin/step logins on the target host stay inside the
    // job; inserted after masking setup — a path, not a secret.
    if run.docker_config_dir.is_some() {
        ssh_env.insert(
            "DOCKER_CONFIG".into(),
            format!("/tmp/buz-docker-{}", run.log.job_id),
        );
    }
    ssh_env.insert("BUZ_STEP_ENGINE".into(), "ssh".into());
    // Parallel fan-out inside execute_ssh_step; cancelling this
    // select arm drops join_all and aborts every in-flight host.
    let sessions_for_run = std::mem::take(ssh_sessions);

    let (output, was_cancelled) = tokio::select! {
        result = execute_ssh_step(
            sessions_for_run,
            &cmd,
            run.spec.timeout,
            &sname,
            &ssh_env,
            ssh_hosts_parallel(run.spec.step),
            ssh_hosts_max_parallel(run.spec.step),
        ) => {
            (result?, false)
        }
        _ = cancel_triggered(&mut cancel_rx_step) => {
            tracing::warn!("Job {jid} cancelled during SSH step '{sname2}' — aborting all hosts");
            (engine::shell::ShellOutput {
                stdout: String::new(),
                stderr: "==> CANCELLED\n".to_string(),
                exit_code: -1,
            }, true)
        }
    };
    Ok(StepExecution {
        output,
        was_cancelled,
        output_secret_values,
        // SSH output is uploaded as one summary chunk, not streamed.
        next_stream: run.stream_start,
        omitted: None,
    })
}

/// Docker engine: image prefetch, streaming run with the step-scoped
/// container name; cancel/timeout also stops the container.
pub(super) async fn run_docker_step(
    run: StepRun<'_>,
    prefetch: &mut Option<crate::client::image_prefetch::ImagePrefetch>,
    docker_socket_path: Option<&Path>,
    cancel_rx: &mut watch::Receiver<bool>,
) -> anyhow::Result<StepExecution> {
    let image = run.spec.step["image"].as_str().unwrap_or("").trim();
    if let Err(e) = engine::docker::preflight(run.config.engine.docker_enabled, image) {
        return Err(run.log.fail_step(format!("{e:#}")).await);
    }

    let prefetch_result = if let Some(prefetch) = prefetch.as_mut() {
        tokio::select! {
            result = prefetch.wait_for(image) => result,
            _ = cancel_triggered(cancel_rx) => {
                prefetch.abort_and_drain().await;
                return Err(run.log.cancel_step().await);
            }
        }
    } else {
        None
    };
    if let Some(Err(error)) = prefetch_result {
        tracing::warn!("Docker image prefetch failed for {image:?}: {error}");
    }

    // Debug echo — show the command before execution (like bash -x)
    {
        let echo_line = build_step_echo(
            run.spec.exec_type,
            run.spec.command,
            &run.env,
            run.catalog,
            run.spec.work_dir,
        );
        run.log.emit(LOG_ECHO, echo_line).await;
    }

    let mut cancel_rx_step = cancel_rx.clone();
    let jid = run.log.job_id.clone();
    let sname = run.spec.name.to_string();

    let container_name = format!("buz-{}-{}", run.log.job_id, run.spec.idx);
    let exec_result = engine::docker::execute_streaming_with_socket(
        image,
        Path::new(&run.config.runner.workspace_dir),
        run.spec.command,
        &run.env,
        &run.spec.volumes,
        run.spec.work_dir,
        run.spec.privileged,
        run.docker_config_dir,
        docker_socket_path,
        Some(&container_name),
    )
    .await;

    let (mut child, mut output_rx, read_guard) = match exec_result {
        Ok(v) => v,
        Err(e) => return Err(e),
    };

    let upload_task = {
        let logger = run.log.clone();
        let secret_values = run.secret_values.to_vec();
        let forced_values = run.forced_values.to_vec();
        let start = run.stream_start;
        tokio::spawn(async move {
            logger
                .stream_chunks(&mut output_rx, &secret_values, &forced_values, start)
                .await
        })
    };

    let (exit_code, was_cancelled_docker, was_timeout_docker, stream_outcome) = tokio::select! {
        status = child.wait() => {
            let code = status?.code().unwrap_or(-1);
            let _ = read_guard.join().await;
            let outcome = upload_task
                .await
                .unwrap_or(step_log::StreamOutcome { next_index: run.stream_start, omitted: None });
            (code, false, false, outcome)
        }
        _ = cancel_triggered(&mut cancel_rx_step) => {
            tracing::warn!("Job {jid} cancelled during docker step '{sname}' — killing container process");
            let _ = child.kill().await;
            let _ = child.wait().await;
            engine::docker::stop_container(&container_name, run.docker_config_dir).await;
            upload_task.abort();
            (-1, true, false, step_log::StreamOutcome { next_index: run.stream_start, omitted: None })
        }
        _ = tokio::time::sleep(run.spec.timeout.unwrap_or(Duration::from_secs(3600))), if run.spec.timeout.is_some() => {
            tracing::warn!("Docker step '{sname}' timed out after {}s — killing container", run.spec.timeout_secs);
            let _ = child.kill().await;
            let _ = child.wait().await;
            engine::docker::stop_container(&container_name, run.docker_config_dir).await;
            let _ = read_guard.join().await;
            upload_task.abort();
            (-1, false, true, step_log::StreamOutcome { next_index: run.stream_start, omitted: None })
        }
    };

    Ok(StepExecution {
        output: engine::shell::ShellOutput {
            stdout: String::new(),
            stderr: if was_cancelled_docker {
                "==> CANCELLED\n".into()
            } else if was_timeout_docker {
                step_timeout_content(run.spec.timeout_secs)
            } else {
                String::new()
            },
            exit_code: if was_cancelled_docker || was_timeout_docker {
                -1
            } else {
                exit_code
            },
        },
        was_cancelled: was_cancelled_docker,
        output_secret_values: run.secret_values.to_vec(),
        next_stream: stream_outcome.next_index,
        omitted: stream_outcome.omitted,
    })
}

/// Post-execution bookkeeping: cancel short-circuit, SSH/sandbox summary
/// chunk, Verbose result, and the exit-code verdict. Returns `true` when the
/// step failed under `fail_strategy = "continue"` (the job must still end as
/// failed); stop-strategy failures and cancels return an already-reported
/// error.
pub(super) async fn finalize_step(
    log: &StepLogger,
    spec: &PluginSpec<'_>,
    execution: StepExecution,
    forced_values: &[String],
    sandbox_handle: Option<&crate::client::sandbox_client::SandboxHandle>,
    verbose: bool,
    step_started: std::time::Instant,
) -> anyhow::Result<bool> {
    let StepExecution {
        output,
        was_cancelled,
        output_secret_values,
        next_stream: _,
        omitted,
    } = execution;
    if was_cancelled {
        return Err(log.cancel_step().await);
    }

    // Truncation must be stated on the step-end record, not only in the live
    // stream marker: the summary chunk (SSH/sandbox) and any failure message
    // both note that the output above is incomplete.
    let truncation_note = omitted.map(step_log::build_truncation_note);

    // For SSH steps, upload the accumulated output as a summary chunk.
    // Local steps are fully streamed so we skip this redundant upload.
    if spec.exec_type == "ssh" || sandbox_handle.is_some() {
        let combined = format!("==> {}\n{}\n{}", spec.name, output.stdout, output.stderr);
        let mut masked = mask_output(&combined, &output_secret_values, forced_values);
        if let Some(note) = &truncation_note {
            masked.push('\n');
            masked.push_str(note);
        }
        log.try_emit(LOG_STEP_SUMMARY, masked).await?;
    } else if let Some(note) = &truncation_note {
        // Streamed steps already carry the 9991 marker; on failure the ERROR
        // terminal is the last thing read — state the truncation there too.
        log.emit(LOG_STEP_SUMMARY, format!("==> {}\n{}", spec.name, note))
            .await;
    }

    // Verbose mode: result summary (duration + exit code). Uploaded even on
    // failure so troubleshooting users see exactly how long the step ran.
    if verbose && !was_cancelled {
        log.emit(
            LOG_STEP_VERBOSE_RESULT,
            build_verbose_step_result(step_started.elapsed(), output.exit_code),
        )
        .await;
    }

    if output.exit_code != 0 && spec.fail_strategy == "stop" {
        let mut hint = step_failure_hint(&output.stderr, output.exit_code);
        if truncation_note.is_some() {
            if !hint.is_empty() {
                hint.push(' ');
            }
            hint.push_str(step_log::TRUNCATION_FAILURE_HINT);
        }
        let msg = build_step_failure_message(spec.name, output.exit_code, spec.command, &hint);
        // fail_step uploads the failure into this step's terminal so it is
        // visible in the UI. Prefer step_order=idx over run_one_job's global
        // -1 catch — the frontend maps -1 as a fallback, but per-step errors
        // must not rely on it.
        return Err(log.fail_step(msg).await);
    }
    if output.exit_code != 0 {
        // fail_strategy = "continue": keep going, but the job ends failed.
        let truncated = step_truncation(spec.idx, omitted);
        update_job_status_with_truncation(
            &log.client,
            &log.config,
            &log.token,
            &log.job_id,
            "failed",
            Some(log.step_order),
            &truncated,
        )
        .await?;
        return Ok(true);
    }
    let truncated = step_truncation(spec.idx, omitted);
    update_job_status_with_truncation(
        &log.client,
        &log.config,
        &log.token,
        &log.job_id,
        "success",
        Some(log.step_order),
        &truncated,
    )
    .await?;
    Ok(false)
}

pub struct SandboxOutputMasker {
    pending: String,
    max_secret_bytes: usize,
    secret_values: Vec<String>,
    forced_values: Vec<String>,
}

impl SandboxOutputMasker {
    pub fn new(secret_values: &[String], forced_values: &[String]) -> Self {
        let max_secret_bytes = secret_values
            .iter()
            .chain(forced_values)
            .map(String::len)
            .max()
            .unwrap_or_default();
        Self {
            pending: String::new(),
            max_secret_bytes,
            secret_values: secret_values.to_vec(),
            forced_values: forced_values.to_vec(),
        }
    }

    pub fn push(&mut self, chunk: &str, flush: bool) -> String {
        self.pending.push_str(chunk);
        let mut safe_bytes = if flush || self.max_secret_bytes <= 1 {
            self.pending.len()
        } else {
            self.pending.len().saturating_sub(self.max_secret_bytes - 1)
        };
        while !self.pending.is_char_boundary(safe_bytes) {
            safe_bytes -= 1;
        }
        loop {
            let previous = safe_bytes;
            for secret in self.secret_values.iter().chain(&self.forced_values) {
                let secret_len = secret.len();
                if secret_len == 0 {
                    continue;
                }
                let first_start = safe_bytes.saturating_sub(secret_len - 1);
                for start in first_start..safe_bytes {
                    let end = start.saturating_add(secret_len);
                    if end > safe_bytes && self.pending.get(start..end) == Some(secret.as_str()) {
                        safe_bytes = start;
                        break;
                    }
                }
            }
            while !self.pending.is_char_boundary(safe_bytes) {
                safe_bytes -= 1;
            }
            if safe_bytes == previous {
                break;
            }
        }
        let remainder = self.pending.split_off(safe_bytes);
        let ready = std::mem::replace(&mut self.pending, remainder);
        let masked = crate::masker::mask_secrets_forced(
            &crate::masker::mask_secrets(&ready, &self.secret_values),
            &self.forced_values,
        );
        if flush {
            let tail = std::mem::take(&mut self.pending);
            masked
                + &crate::masker::mask_secrets_forced(
                    &crate::masker::mask_secrets(&tail, &self.secret_values),
                    &self.forced_values,
                )
        } else {
            masked
        }
    }
}

#[allow(clippy::too_many_arguments)] // job execution context: step, sandbox, secrets, logging handles
pub(super) async fn run_sandbox_step(
    log: &StepLogger,
    config: &Config,
    command: &str,
    work_dir: &str,
    environment: &serde_json::Value,
    timeout_seconds: u64,
    secret_values: &[String],
    forced_values: &[String],
    cancel_rx: &mut watch::Receiver<bool>,
    handle: &crate::client::sandbox_client::SandboxHandle,
    stream_start: i32,
) -> anyhow::Result<(engine::shell::ShellOutput, bool, i32, Option<u64>)> {
    let mut stream = crate::client::sandbox_client::exec_step(
        &config.sandbox,
        handle,
        command,
        work_dir,
        environment,
        timeout_seconds,
    )
    .await?;
    let mut budget = StreamBudget::new_at(log.log_base, stream_start);
    let mut output_masker = SandboxOutputMasker::new(secret_values, forced_values);
    loop {
        tokio::select! {
            frame = stream.recv() => {
                match frame {
                    Some(Ok(crate::client::sandbox_client::SandboxExecEvent::Output { content, .. })) => {
                        let masked = output_masker.push(&content, false);
                        if !masked.is_empty() {
                            match budget.take() {
                                Some(chunk_num) => {
                                    if let Err(error) = log.emit_at(chunk_num, masked).await {
                                        tracing::warn!("Sandbox output chunk {chunk_num} upload failed: {error}");
                                    }
                                }
                                None => {
                                    // Window exhausted: keep draining the
                                    // supervisor stream, keep the bounded tail hooked
                                    // for the marker, and annotate once.
                                    budget.omit(&masked);
                                    if let Some(marker) = budget.unnotified_truncation() {
                                        log.emit(LOG_STEP_TRUNCATED, marker).await;
                                    }
                                }
                            }
                        }
                    }
                    Some(Ok(crate::client::sandbox_client::SandboxExecEvent::Exit { code, timed_out })) => {
                        let tail = output_masker.push("", true);
                        if !tail.is_empty() {
                            if let Some(chunk_num) = budget.take() {
                                let _ = log.emit_at(chunk_num, tail).await;
                            }
                        }
                        if budget.truncated() {
                            log.emit(LOG_STEP_TRUNCATED, budget.marker()).await;
                        }
                        return Ok((
                            engine::shell::ShellOutput {
                                stdout: String::new(),
                                stderr: if timed_out {
                                    step_timeout_content(timeout_seconds)
                                } else {
                                    String::new()
                                },
                                exit_code: if timed_out { -1 } else { code },
                            },
                            false,
                            budget.next_index(),
                            budget.omitted(),
                        ));
                    }
                    Some(Ok(crate::client::sandbox_client::SandboxExecEvent::Error { error })) => {
                        anyhow::bail!("sandbox step failed: {error}");
                    }
                    Some(Err(error)) => return Err(error),
                    None => anyhow::bail!("sandbox supervisor closed the step stream"),
                }
            }
            _ = cancel_triggered(cancel_rx) => {
                stream.abort();
                if budget.truncated() {
                    log.emit(LOG_STEP_TRUNCATED, budget.marker()).await;
                }
                return Ok((
                    engine::shell::ShellOutput {
                        stdout: String::new(),
                        stderr: "==> CANCELLED\n".into(),
                        exit_code: -1,
                    },
                    true,
                    budget.next_index(),
                    budget.omitted(),
                ));
            }
        }
    }
}

/// Parse a `$BUZ_ENV` file (GHA `$GITHUB_ENV` format).
///
/// - `KEY=value` — everything after the first `=` is the value (no quote
///   stripping), trailing newline/CR removed.
/// - `KEY<<DELIM` — multiline block; following lines until a line equal to
///   `DELIM` are joined with `\n` (GHA heredoc form).
/// - Blank lines and `# comments` are skipped; anything else is a warning.
///
/// Duplicate keys: the last write wins (file order).
pub fn parse_buz_env_file(path: &str) -> anyhow::Result<Vec<(String, String)>> {
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => anyhow::bail!("cannot read {path}: {e}"),
    };
    parse_buz_env_content(&content)
}

fn parse_buz_env_content(content: &str) -> anyhow::Result<Vec<(String, String)>> {
    let valid_key = |key: &str| {
        let mut chars = key.chars();
        matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
            && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
    };
    let mut out: Vec<(String, String)> = Vec::new();
    let mut lines = content.lines().peekable();
    while let Some(raw) = lines.next() {
        let line = raw.trim_end_matches('\r');
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some((key, delimiter)) = trimmed.split_once("<<") {
            let key = key.trim();
            let delimiter = delimiter.trim();
            if !valid_key(key) || delimiter.is_empty() {
                tracing::warn!("$BUZ_ENV: ignoring malformed line: {trimmed:?}");
                continue;
            }
            let mut value = String::new();
            let mut closed = false;
            for block_line in lines.by_ref() {
                let block_line = block_line.trim_end_matches('\r');
                if block_line.trim() == delimiter {
                    closed = true;
                    break;
                }
                if !value.is_empty() {
                    value.push('\n');
                }
                value.push_str(block_line);
            }
            if !closed {
                tracing::warn!("$BUZ_ENV: unterminated {key}<<{delimiter} block — dropping");
                continue;
            }
            out.retain(|(k, _)| k != key);
            out.push((key.to_string(), value));
        } else if let Some((key, value)) = trimmed.split_once('=') {
            let key = key.trim();
            if !valid_key(key) {
                tracing::warn!("$BUZ_ENV: ignoring malformed line: {trimmed:?}");
                continue;
            }
            out.retain(|(k, _)| k != key);
            out.push((key.to_string(), value.to_string()));
        } else {
            tracing::warn!("$BUZ_ENV: ignoring line without '=': {trimmed:?}");
        }
    }
    Ok(out)
}

/// Normalize the claim response's `metadata` object plus git trigger inputs
/// into `BUZ_*` environment variables (marketplace ADR §5.2). Empty or
/// absent values are omitted — non-git triggers carry no `BUZ_GIT_*`.
pub fn buz_metadata_env(
    task: &serde_json::Value,
    job_id: &str,
    manager_url: &str,
) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut put = |key: &str, value: String| {
        if !value.is_empty() {
            out.push((key.to_string(), value));
        }
    };

    put("BUZ_JOB_ID", job_id.to_string());
    put(
        "BUZ_JOB_URL",
        format!("{}/jobs/{job_id}", manager_url.trim_end_matches('/')),
    );

    let metadata = task.get("metadata");
    let field = |name: &str| {
        metadata
            .and_then(|m| m.get(name))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };
    // Numeric metadata (GHA github.run_number / github.run_attempt analogue);
    // omitted when the claim predates them (put() skips empty values).
    let number = |name: &str| {
        metadata
            .and_then(|m| m.get(name))
            .and_then(|v| v.as_i64())
            .map(|v| v.to_string())
            .unwrap_or_default()
    };
    put("BUZ_PIPELINE_NAME", field("pipeline_name"));
    put("BUZ_TEAM", field("team"));
    put("BUZ_TRIGGER", field("trigger"));
    put("BUZ_TRIGGERED_BY", field("triggered_by"));
    put("BUZ_RUN_NUMBER", number("run_number"));
    put("BUZ_RUN_ATTEMPT", number("run_attempt"));

    // Normalized from the GIT_* trigger inputs (exported as INPUT_GIT_*).
    let input = |name: &str| {
        task.get("inputs")
            .and_then(|i| i.get(name))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };
    put("BUZ_GIT_SHA", input("GIT_SHA"));
    put("BUZ_GIT_BRANCH", input("GIT_BRANCH"));

    out
}

/// Build an SSH command string with environment variable exports prepended.
/// Follows the same INPUT_* convention as the shell and docker engines,
/// and also exports step-level secret values.
pub fn build_ssh_env_command(
    command: &str,
    env: &std::collections::HashMap<String, String>,
) -> String {
    let mut prefix = String::from("set -e; ");
    // DOCKER_CONFIG target dir may not exist on the remote host yet — docker
    // reads it on every command, and a missing dir misbehaves on older CLIs.
    if let Some(dir) = env.get("DOCKER_CONFIG") {
        prefix.push_str(&format!("mkdir -p {} && ", shell_escape(dir)));
    }
    if env.is_empty() {
        return format!("{prefix}{command}");
    }
    for (k, v) in env {
        // $BUZ_ENV is a Runner-host workspace path; remote hosts have no such
        // file. Dynamic values still flow as ordinary exported vars below.
        if k == "BUZ_ENV" {
            continue;
        }
        // Export the raw variable
        let safe_v = shell_escape(v);
        prefix.push_str(&format!("export {k}={safe_v} && "));
        // Also export as INPUT_* for consistency (pipeline inputs convention)
        let input_key = shared::protocol::input_env_key(k);
        prefix.push_str(&format!("export {input_key}={safe_v} && "));
    }
    format!("{}{}", prefix, command)
}

/// Minimal shell-escaping of values in single-quoted `export` statements.
fn shell_escape(value: &str) -> String {
    let escaped = value.replace('\'', "'\\''");
    format!("'{escaped}'")
}

pub(crate) fn ssh_hosts_parallel(step: &serde_json::Value) -> bool {
    step["hosts_strategy"].as_str() == Some("parallel")
}

/// Step-level parallel width cap (1–256). Absent → bounded runner default
/// (5) applied by `shared::pipeline_yaml::effective_hosts_parallel_width`.
pub(crate) fn ssh_hosts_max_parallel(step: &serde_json::Value) -> Option<usize> {
    step["hosts_max_parallel"].as_u64().map(|v| v as usize)
}

/// Parse the step's `host` field (Manager-resolved connection objects) into
/// connection params. Used by the config prepare path (which runs before the
/// run block) and by `execute_ssh_step`.
fn resolve_ssh_conns(step: &serde_json::Value, config: &Config) -> Vec<SshConnectionParams> {
    let host_values = step["host"]
        .as_array()
        .cloned()
        .or_else(|| {
            step["host"]
                .as_object()
                .map(|o| vec![serde_json::Value::Object(o.clone())])
        })
        .unwrap_or_default();
    host_values
        .iter()
        .filter_map(|hv| build_ssh_conn(hv, config).ok())
        .collect()
}

/// Build connection params from one Manager-resolved host object.
fn build_ssh_conn(hv: &serde_json::Value, config: &Config) -> anyhow::Result<SshConnectionParams> {
    let host_params = hv
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("SSH step host data is not an object"))?;
    Ok(SshConnectionParams {
        host: host_params
            .get("host")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        port: host_params
            .get("port")
            .and_then(|v| v.as_u64())
            .unwrap_or(22) as u16,
        username: host_params
            .get("username")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        credential_type: host_params
            .get("credential_type")
            .and_then(|v| v.as_str())
            .unwrap_or("key_path")
            .to_string(),
        credential_value: host_params
            .get("credential_value")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        credential_passphrase: host_params
            .get("credential_passphrase")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        host_key_policy: config.engine.ssh_host_key_policy.clone(),
        known_hosts_path: config.engine.ssh_known_hosts_path.clone(),
    })
}

/// Run the step command on already-open step sessions.
///
/// When `parallel` is true, hosts run concurrently; when false, one-by-one.
/// Wait-all: every host finishes (unless this future is dropped on cancel).
/// Any non-zero exit or exec error fails the step. Per-host output is buffered
/// then concatenated under `--- {step} @ {host} ---` when multi-host.
pub async fn execute_ssh_step(
    sessions: Vec<SshSession>,
    command: &str,
    step_timeout: Option<std::time::Duration>,
    step_name: &str,
    env: &std::collections::HashMap<String, String>,
    parallel: bool,
    max_parallel: Option<usize>,
) -> anyhow::Result<engine::shell::ShellOutput> {
    let multi = sessions.len() > 1;
    let ssh_cmd = build_ssh_env_command(command, env);
    let width = shared::pipeline_yaml::effective_hosts_parallel_width(
        parallel && multi,
        max_parallel.map(|v| v as u32),
        sessions.len(),
    );
    // `hosts_strategy: parallel` runs concurrently at `width` (the effective
    // default is 5, clamped to the host count). When width covers every host
    // the buffer is unbounded — equivalent to join_all — so one code path
    // serves both the capped and the uncapped cases. `chain` keeps the
    // sequential loop, and it still runs every host (failures are collected,
    // never short-circuited).
    let results = if parallel && multi {
        let futs = sessions.into_iter().enumerate().map(|(idx, mut session)| {
            let ssh_cmd = ssh_cmd.clone();
            async move {
                let host_label = session.host_label().to_string();
                let result = session.exec(&ssh_cmd, None, step_timeout).await;
                (idx, host_label, result)
            }
        });
        use futures_util::StreamExt;
        let mut collected = futures_util::stream::iter(futs)
            .buffer_unordered(width)
            .collect::<Vec<_>>()
            .await;
        collected.sort_by_key(|(idx, _, _)| *idx);
        collected
            .into_iter()
            .map(|(_, host_label, result)| (host_label, result))
            .collect()
    } else {
        let mut results = Vec::with_capacity(sessions.len());
        for mut session in sessions {
            let host_label = session.host_label().to_string();
            let result = session.exec(&ssh_cmd, None, step_timeout).await;
            results.push((host_label, result));
        }
        results
    };
    Ok(merge_ssh_host_outputs(step_name, multi, results))
}

/// Merge per-host SSH results into one step output (wait-all semantics).
///
/// `Ok(output)` / `Err(error)` per host. Any failure → non-zero step exit.
pub fn merge_ssh_host_outputs(
    step_name: &str,
    multi: bool,
    mut results: Vec<(String, anyhow::Result<engine::ssh::SshOutput>)>,
) -> engine::shell::ShellOutput {
    results.sort_by(|a, b| a.0.cmp(&b.0));

    let mut combined_stdout = String::new();
    let mut combined_stderr = String::new();
    let mut exit_code: i32 = 0;
    let mut summary: Vec<String> = Vec::new();

    for (host_label, result) in results {
        let ssh_header = if multi {
            format!("--- {step_name} @ {host_label} ---\n")
        } else {
            String::new()
        };
        match result {
            Ok(o) => {
                combined_stdout.push_str(&ssh_header);
                combined_stdout.push_str(&o.stdout);
                if !o.stderr.is_empty() {
                    if !combined_stdout.ends_with('\n') && !combined_stdout.is_empty() {
                        combined_stdout.push('\n');
                    }
                    combined_stderr.push_str(&ssh_header);
                    combined_stderr.push_str(&o.stderr);
                }
                let code = o.exit_code as i32;
                if code != 0 {
                    tracing::warn!(
                        "SSH step '{step_name}' on {host_label} exited with code {code}"
                    );
                    if exit_code == 0 {
                        exit_code = code;
                    }
                }
                summary.push(format!("{host_label}: exit {code}"));
            }
            Err(e) => {
                tracing::error!("SSH step '{step_name}' on {host_label} failed: {e}");
                combined_stderr.push_str(&format!("{ssh_header}ERROR: {e}\n"));
                if exit_code == 0 {
                    exit_code = -1;
                }
                summary.push(format!("{host_label}: error"));
            }
        }
    }

    if multi && !summary.is_empty() {
        let line = format!("--- summary ---\n{}\n", summary.join("\n"));
        if !combined_stdout.is_empty() && !combined_stdout.ends_with('\n') {
            combined_stdout.push('\n');
        }
        combined_stdout.push_str(&line);
    }

    engine::shell::ShellOutput {
        stdout: combined_stdout,
        stderr: combined_stderr,
        exit_code,
    }
}

pub(super) async fn update_job_status(
    client: &Client,
    config: &Config,
    token: &str,
    job_id: &str,
    status: &str,
    step_order: Option<i32>,
) -> anyhow::Result<()> {
    update_job_status_with_truncation(client, config, token, job_id, status, step_order, &[]).await
}

/// The step orders to report alongside `step`: this step when it overflowed,
/// nothing otherwise. One step per terminal report keeps the payload small and
/// the Manager's column merge (union) trivial.
fn step_truncation(step_order: usize, omitted: Option<u64>) -> Vec<i32> {
    if omitted.is_some() {
        vec![step_order as i32]
    } else {
        Vec::new()
    }
}

/// Same report, plus the step orders whose streamed output was truncated. The
/// Manager stores them on the job row, so the truncation badge outlives the
/// log rows that `job_log_retention` eventually deletes.
#[allow(clippy::too_many_arguments)] // one logical status report
pub(super) async fn update_job_status_with_truncation(
    client: &Client,
    config: &Config,
    token: &str,
    job_id: &str,
    status: &str,
    step_order: Option<i32>,
    truncated_steps: &[i32],
) -> anyhow::Result<()> {
    // Retry transient failures up to 3 times with short backoff.
    // Critical: if this fails silently the job stays stuck in "running" forever.
    let mut last_err = None;
    for attempt in 1..=3 {
        let resp = client
            .patch(format!(
                "{}/api/v1/jobs/{job_id}/status",
                config.runner.manager_url
            ))
            .header("Authorization", format!("Bearer {token}"))
            .json(&UpdateJobStatusRequest {
                status: status.to_string(),
                step_order,
                truncated_steps: truncated_steps.to_vec(),
            })
            .timeout(Duration::from_secs(15))
            .send()
            .await;

        match resp {
            Ok(r) if r.status().is_success() => return Ok(()),
            Ok(r) => {
                let code = r.status();
                let body = r.text().await.unwrap_or_default();
                last_err = Some(anyhow::anyhow!("status update returned {code}: {body}",));
            }
            Err(e) => {
                last_err = Some(super::friendly_reqwest_error(
                    e,
                    &config.runner.manager_url,
                    "Job status update failed",
                ));
            }
        }

        if attempt < 3 {
            tokio::time::sleep(Duration::from_millis(500 * attempt as u64)).await;
        }
    }

    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("status update failed after all retries")))
}

pub fn is_safe_workspace_subdir(dir: &str) -> bool {
    let path = Path::new(dir);
    !path.as_os_str().is_empty()
        && !path.is_absolute()
        && path
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_)))
}

/// Job-scoped Docker CLI config directory: `{workspace}/{job_id}/.docker`.
///
/// Per-job `DOCKER_CONFIG` isolation (runner.toml `[engine]
/// docker_config_isolation`, default `per_job`): logins — core or marketplace
/// template — land here instead of the host-global `~/.docker/config.json`.
pub fn job_docker_config_dir(job_ws: &str) -> String {
    format!("{}/.docker", job_ws.trim_end_matches('/'))
}

/// Create the job Docker config dir (0700) when isolation is `per_job`.
///
/// No-op under `shared`. Best-effort by contract: if this fails the job still
/// runs — `docker login` recreates the dir on first use.
pub fn prepare_job_docker_config_dir(
    job_ws: &str,
    isolation: DockerConfigIsolation,
    run_as_user: Option<&str>,
) -> std::io::Result<()> {
    if isolation != DockerConfigIsolation::PerJob {
        return Ok(());
    }
    let dir = job_docker_config_dir(job_ws);
    std::fs::create_dir_all(&dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // A step running as another account still needs to write its Docker
        // CLI config; group access is the only option without broadening the
        // directory to everyone. The account must share the workspace group.
        let mode = if run_as_user
            .map(str::trim)
            .filter(|user| !user.is_empty())
            .is_some()
        {
            0o770
        } else {
            0o700
        };
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(mode))?;
    }
    Ok(())
}

/// Remove the job Docker config dir. Missing dir is fine (already cleaned with
/// the workspace); any other error is logged, never fails the job.
pub fn clean_job_docker_config(job_ws: &str) {
    let dir = job_docker_config_dir(job_ws);
    if let Err(e) = std::fs::remove_dir_all(&dir) {
        if e.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!("Failed to clean job docker config dir '{dir}': {e}");
        }
    }
}

/// Clean the workspace directory by removing all contents.
/// The directory itself is preserved (recreated if needed) for future jobs.
fn clean_workspace(work_dir: &str) {
    match std::fs::remove_dir_all(work_dir) {
        Ok(()) => {
            if let Err(e) = std::fs::create_dir_all(work_dir) {
                tracing::warn!("Failed to recreate workspace dir '{work_dir}': {e}");
            }
        }
        Err(e) => {
            // If the directory doesn't exist, that's fine — nothing to clean
            if e.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!("Failed to clean workspace '{work_dir}': {e}");
            }
        }
    }
}

/// Restrict the per-job workspace to its owner (0700), or to a shared group
/// (0770) when `[engine] run_as_user` makes another account execute steps in
/// it — that account must share the workspace's group. Best-effort: a failure
/// is logged, never fatal (Windows has no POSIX modes).
fn restrict_job_workspace(job_ws: &str, run_as_user: Option<&str>) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = if run_as_user
            .map(str::trim)
            .filter(|u| !u.is_empty())
            .is_some()
        {
            0o770
        } else {
            0o700
        };
        if let Err(e) = std::fs::set_permissions(job_ws, std::fs::Permissions::from_mode(mode)) {
            tracing::warn!("Failed to set mode {mode:o} on job workspace {job_ws}: {e}");
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (job_ws, run_as_user);
    }
}
