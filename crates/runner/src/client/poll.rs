use reqwest::Client;
use shared::protocol::{LogChunk, UpdateJobStatusRequest};
use shared::value_source::ValueMap;
use std::path::Path;
use std::time::Duration;

use crate::config::Config;
use crate::engine;
use crate::engine::ssh::SshConnectionParams;
use crate::masker::SecretCatalog;
use crate::policy::{self, SharedPolicy};
use std::collections::{HashMap, HashSet};

/// Per-step log chunk layout (`UNIQUE(job_id, chunk_index)`).
///
/// Manager / Job Detail display logs with `ORDER BY step_order, chunk_index`,
/// so these offsets must match **semantic** order (not merely upload time):
///
/// ```text
///   0     ==> STAGE prepare     (verbose)
///   1     ==> STEP CONFIG       (verbose)
///   2     ==> CONFIG PREP       (verbose / debug)
///   3     ==> LOGIN […]         (registry)
///   4     ==> STAGE exec        (verbose)
///   5     ==> RUN […]
///   6…    streamed stdout/stderr
///   9993  ==> STEP RESULT       (verbose; after stream)
///   9997  ==> ERROR
///   9998–9999  reserved meta
/// ```
///
/// Verbose prep/config used to live at 9990+, which sorted **after** RUN +
/// stream and made Verbose look "out of order" once the job reloaded from DB.
pub(crate) fn step_log_base(step_idx: usize) -> i32 {
    (step_idx * 10_000) as i32
}

pub(crate) const LOG_STEP_VERBOSE_PREPARE: i32 = 0;
pub(crate) const LOG_STEP_VERBOSE_CONFIG: i32 = 1;
pub(crate) const LOG_STEP_VERBOSE_CONFIG_PREP: i32 = 2;
pub(crate) const LOG_LOGIN: i32 = 3;
pub(crate) const LOG_STEP_VERBOSE_EXEC: i32 = 4;
pub(crate) const LOG_ECHO: i32 = 5;
pub(crate) const LOG_STREAM_START: i32 = 6;
pub(crate) const LOG_STEP_VERBOSE_RESULT: i32 = 9993;
pub(crate) const LOG_STEP_ERROR: i32 = 9997;
const LOG_STEP_META_A: i32 = 9998;
const LOG_STEP_META_B: i32 = 9999;

/// Build a structured step header (shown before command output).
///
/// Format (aligned with LOGIN / ERROR / CANCELLED banners):
/// ```text
/// ==> RUN [docker]
///   note  GIT_TERMINAL_PROMPT=0 (noninteractive)
///   cwd   /path
///   env   KEY=val  SECRET=***
///   cmd   echo hi
///         more…
/// ```
pub(crate) fn build_step_echo(
    exec_type: &str,
    command: &str,
    env: &HashMap<String, String>,
    catalog: &SecretCatalog,
    work_dir: Option<&str>,
) -> String {
    let mut out = format!("==> RUN [{exec_type}]\n");
    out.push_str("  note  GIT_TERMINAL_PROMPT=0 (noninteractive)\n");
    if let Some(wd) = work_dir.filter(|s| !s.is_empty()) {
        out.push_str(&format!("  cwd   {wd}\n"));
    }
    if !env.is_empty() {
        let mut keys: Vec<&String> = env.keys().collect();
        keys.sort();
        let items: Vec<String> = keys
            .into_iter()
            .map(|k| {
                if catalog.is_injected_key(k) {
                    format!("{k}=<injected>")
                } else if catalog.is_secret_key(k) {
                    format!("{k}=***")
                } else {
                    format!("{k}={}", env.get(k).map(String::as_str).unwrap_or(""))
                }
            })
            .collect();
        out.push_str(&format!("  env   {}\n", items.join("  ")));
    }
    let cmd = command.trim_end();
    let mut cmd_lines = cmd.lines();
    if let Some(first) = cmd_lines.next() {
        out.push_str(&format!("  cmd   {first}\n"));
        for line in cmd_lines {
            out.push_str(&format!("        {line}\n"));
        }
    } else {
        out.push_str("  cmd   \n");
    }
    out
}

/// Banner for a successful local `docker login` before pull/run.
pub(crate) fn build_login_banner(registry: &str) -> String {
    format!("==> LOGIN [{registry}]\n")
}

/// Verbose-mode stage marker (`==> STAGE …`).
fn build_verbose_stage(stage: &str) -> String {
    format!("==> STAGE {stage}\n")
}

/// Verbose-mode full step config — the execution shape the runner received.
/// Renders only keys/metadata, never secret values (registry password, signed
/// config URL, …), so it is safe for every user.
///
/// Uploaded **before** `config_source` prepare so a prepare-stage failure
/// (e.g. OSS 404) still shows the step shape — previously STEP CONFIG only
/// ran after a successful unpack, so Verbose looked identical to a normal run.
pub(crate) fn build_verbose_step_config(
    step: &serde_json::Value,
    exec_type: &str,
    registry_secret_ref: &str,
) -> String {
    let mut out = String::from("==> STEP CONFIG\n");
    let mut push_field = |label: &str, value: String| {
        if !value.is_empty() {
            out.push_str(&format!("  {label:<16} {value}\n"));
        }
    };
    push_field("exec_type", exec_type.to_string());
    if let Some(t) = step["timeout_seconds"].as_u64().filter(|t| *t > 0) {
        push_field("timeout", format!("{t}s"));
    }
    push_field(
        "fail_strategy",
        step["fail_strategy"].as_str().unwrap_or("stop").to_string(),
    );
    push_field(
        "work_dir",
        step["work_dir"].as_str().unwrap_or("").to_string(),
    );
    push_field("image", step["image"].as_str().unwrap_or("").to_string());
    push_field("registry_secret", registry_secret_ref.to_string());
    if let Some(cs) = step["config_source"].as_str().filter(|s| !s.is_empty()) {
        let cp = step["config_key_path"].as_str().unwrap_or("");
        push_field("config", format!("{cs} (key: {cp})"));
    }
    if let Some(cd) = step["config_dest"].as_str().filter(|s| !s.is_empty()) {
        push_field("config_dest", cd.to_string());
    }
    if let Some(er) = step.get("config_render").filter(|v| !v.is_null()) {
        let pairs: Vec<String> = if let Some(arr) = er.as_array() {
            arr.iter()
                .filter_map(|e| {
                    let t = e.get("template")?.as_str()?;
                    let o = e
                        .get("output")
                        .and_then(|v| v.as_str())
                        .unwrap_or(".env");
                    Some(format!("{t} → {o}"))
                })
                .collect()
        } else {
            let t = er
                .get("template")
                .and_then(|v| v.as_str())
                .unwrap_or("env.example");
            let o = er
                .get("output")
                .and_then(|v| v.as_str())
                .unwrap_or(".env");
            vec![format!("{t} → {o}")]
        };
        if !pairs.is_empty() {
            push_field("config_render", pairs.join(", "));
        }
    }
    if let Some(hosts) = step["hosts"].as_array() {
        let joined: Vec<&str> = hosts.iter().filter_map(|h| h.as_str()).collect();
        if !joined.is_empty() {
            push_field("hosts", joined.join(", "));
        }
    }
    out
}

/// Banner for the config prepare stage (OSS deliver and/or `config_render`).
///
/// Always safe for Verbose (URL redacted). In Debug mode the signed URL is
/// printed in full — that is the main visible difference for prepare-only
/// failures like OSS 404, where no RUN echo ever happens.
pub(crate) fn build_config_prep_banner(step: &serde_json::Value, debug_unredact: bool) -> String {
    let mut out = String::from("==> CONFIG PREP\n");
    let mut push_field = |label: &str, value: String| {
        if !value.is_empty() {
            out.push_str(&format!("  {label:<16} {value}\n"));
        }
    };
    if let Some(cs) = step["config_source"].as_str().filter(|s| !s.is_empty()) {
        push_field("source", cs.to_string());
    }
    if let Some(dest) = step["config_dest"].as_str().filter(|s| !s.is_empty()) {
        push_field("dest", dest.to_string());
    }
    if let Some(auth) = step.get("config_auth") {
        if let Some(key) = auth.get("object_key").and_then(|v| v.as_str()) {
            push_field("object_key", key.to_string());
        }
        if let Some(ttl) = auth.get("expires_in_secs").and_then(|v| v.as_u64()) {
            push_field("expires_in", format!("{ttl}s"));
        }
        if let Some(url) = auth.get("url").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
            if debug_unredact {
                push_field("url", url.to_string());
            } else {
                push_field("url", "***".into());
            }
        }
    } else if step
        .get("config_render")
        .is_some_and(|v| !v.is_null())
    {
        push_field("mode", "render-only".into());
    }
    out
}

/// Verbose-mode step result summary (duration + exit code).
pub(crate) fn build_verbose_step_result(duration: Duration, exit_code: i32) -> String {
    let dur = if duration.as_secs_f64() >= 1.0 {
        format!("{:.2}s", duration.as_secs_f64())
    } else {
        format!("{}ms", duration.as_millis())
    };
    format!("==> STEP RESULT\n  duration   {dur}\n  exit code  {exit_code}\n")
}

/// Human hint when a step exits non-zero (avoid always blaming interactive input).
pub(crate) fn step_failure_hint(stderr: &str, exit_code: i32) -> String {
    let s = stderr.to_ascii_lowercase();
    if s.contains("timeout") {
        return "the step exceeded its timeout.".into();
    }
    if s.contains("no matching manifest") || s.contains("no match for platform") {
        return "image has no manifest for this runner's CPU architecture \
                (e.g. arm64 host vs amd64-only image). Use a multi-arch image \
                or rebuild for this platform."
            .into();
    }
    if s.contains("pull access denied")
        || s.contains("unauthorized")
        || s.contains("authentication required")
        || s.contains("denied: requested access")
    {
        return "registry pull denied — check registry_secret credentials and image name/permissions."
            .into();
    }
    if s.contains("docker login") && s.contains("failed") {
        return "docker login failed — check registry_secret username/password and registry host."
            .into();
    }
    if exit_code == 125 {
        return "docker could not start the container (image/platform/daemon). See output above."
            .into();
    }
    if exit_code == 127 {
        return "command not found in the execution environment.".into();
    }
    "see step output above for details.".into()
}

/// Structured failure body under `==> ERROR`.
pub(crate) fn build_step_failure_message(
    step_name: &str,
    exit_code: i32,
    command: &str,
    hint: &str,
) -> String {
    let cmd = command.trim().lines().next().unwrap_or("").trim();
    let cmd_more = command.trim().lines().count() > 1;
    let cmd_display = if cmd_more {
        format!("{cmd} …")
    } else {
        cmd.to_string()
    };
    format!(
        "step '{step_name}' failed (exit {exit_code})\n  \
         command  {cmd_display}\n  \
         hint     {hint}"
    )
}

/// Poll the manager to check if this job has been cancelled.
/// Returns `true` if the job is cancelled and execution should stop.
pub async fn check_job_cancelled(
    client: &Client,
    config: &Config,
    token: &str,
    job_id: &str,
) -> anyhow::Result<bool> {
    let resp = client
        .get(format!(
            "{}/api/v1/jobs/{job_id}/status-check",
            config.runner.manager_url
        ))
        .header("Authorization", format!("Bearer {token}"))
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| {
            super::friendly_reqwest_error(e, &config.runner.manager_url, "Status check failed")
        })?;

    if !resp.status().is_success() {
        return Ok(false);
    }

    let body = resp.json::<serde_json::Value>().await?;
    let status = body["data"]["status"].as_str().unwrap_or("");

    Ok(status == "cancelled")
}

/// Poll loop with concurrent job slots and exponential backoff on failures.
///
/// Capacity and strategy (engines, cleanup) come from Manager UI via
/// `shared_policy` (refreshed by heartbeat).
pub async fn run_poll_loop(
    client: &Client,
    config: &Config,
    token: &str,
    shared_policy: SharedPolicy,
) -> anyhow::Result<()> {
    use tokio::task::JoinSet;

    let mut consecutive_failures: u32 = 0;
    let mut jobs: JoinSet<anyhow::Result<()>> = JoinSet::new();

    loop {
        // Reap finished jobs
        while let Some(joined) = jobs.try_join_next() {
            match joined {
                Ok(Ok(())) => {}
                Ok(Err(e)) => tracing::error!("Job task error: {e}"),
                Err(e) => tracing::error!("Job task panicked: {e}"),
            }
        }

        let local_max = policy::max_concurrent(&shared_policy);
        let active = jobs.len() as u32;

        if active >= local_max {
            // At capacity — wait briefly for a slot or heartbeat update
            tokio::time::sleep(Duration::from_secs(2)).await;
            continue;
        }

        match poll_for_task(client, config, token).await {
            Ok(Some(task)) => {
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
                job_config.runner.workspace_dir = job_ws.clone();
                let t = token.to_string();
                let clean = snap.clean_workspace_after_job;

                jobs.spawn(async move {
                    let result = run_one_job(&c, &job_config, &t, &task).await;
                    if clean {
                        tracing::info!("Cleaning job workspace: {job_ws}");
                        clean_workspace(&job_ws);
                    }
                    result
                });
            }
            Ok(None) => {
                if consecutive_failures > 0 {
                    tracing::info!("Poll recovered after {consecutive_failures} failure(s)");
                }
                consecutive_failures = 0;
                // Idle: short pause so we don't busy-spin when Manager returns null immediately
                tokio::time::sleep(Duration::from_secs(2)).await;
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
) -> anyhow::Result<()> {
    let job_id = task["job_id"].as_str().unwrap_or("unknown");

    if let Err(e) = execute_job(client, config, token, task).await {
        let msg = e.to_string();
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
            tracing::error!("Job {job_id} failed: {e}");
            if let Err(le) = super::logs::upload_chunk(
                client,
                config,
                token,
                job_id,
                LogChunk {
                    chunk_index: -1,
                    step_order: -1,
                    content: format!("==> ERROR\n{e}\n"),
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

async fn poll_for_task(
    client: &Client,
    config: &Config,
    token: &str,
) -> anyhow::Result<Option<serde_json::Value>> {
    let resp = client
        .get(format!("{}/api/v1/runners/poll", config.runner.manager_url))
        .header("Authorization", format!("Bearer {token}"))
        .timeout(Duration::from_secs(50))
        .send()
        .await
        .map_err(|e| super::friendly_reqwest_error(e, &config.runner.manager_url, "Poll failed"))?;

    if resp.status() == 204 {
        return Ok(None);
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

    let data = body.get("data").cloned().unwrap_or_default();
    if data.is_null() {
        return Ok(None);
    }

    Ok(Some(data))
}

async fn execute_job(
    client: &Client,
    config: &Config,
    token: &str,
    task: &serde_json::Value,
) -> anyhow::Result<()> {
    let job_id = task["job_id"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("missing job_id"))?;

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

    // Build base env from trigger inputs + pipeline env + pipeline secrets.
    // Infrastructure Secrets and masking inputs are tracked for log redaction.
    let mut base_env: std::collections::HashMap<String, String> = task
        .get("inputs")
        .and_then(|v| v.as_object())
        .map(|obj| {
            obj.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default();

    // Non-secret pipeline env (inline `env:`, platform Variables) — show in clear in logs
    if let Some(env_obj) = task.get("env").and_then(|v| v.as_object()) {
        for (k, v) in env_obj {
            if let Some(s) = v.as_str() {
                base_env.insert(k.clone(), s.to_string());
            }
        }
    }

    // Pipeline-level secrets from Manager — values redacted in RUN echo + stream logs
    let mut pipeline_secret_keys: HashSet<String> = HashSet::new();
    if let Some(secrets_obj) = task.get("secrets").and_then(|v| v.as_object()) {
        for (k, v) in secrets_obj {
            if let Some(s) = v.as_str() {
                base_env.insert(k.clone(), s.to_string());
                pipeline_secret_keys.insert(k.clone());
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
            pipeline_secret_keys.insert(format!("INPUT_{}", key.to_uppercase()));
        }
    }

    // Build the `config_render` ValueMap once per job:
    // - `secrets` domain resolves `${secrets.X}` by logical name from job
    //   `secrets` plus per-step `render_env_secrets` (Manager attaches the
    //   latter when a step has `config_render`);
    // - `vars` domain is pipeline env + trigger inputs (both `NAME` /
    //   `INPUT_NAME` forms), with `masking: true` inputs excluded so a
    //   sensitive trigger value can never route into `.env` via the
    //   non-sensitive namespace.
    let mut render_values = ValueMap::new();
    if let Some(obj) = task.get("secrets").and_then(|v| v.as_object()) {
        for (k, v) in obj {
            if let Some(s) = v.as_str() {
                render_values.secrets.insert(k.clone(), s.to_string());
            }
        }
    }
    for step in tasks {
        if let Some(obj) = step.get("render_env_secrets").and_then(|v| v.as_object()) {
            for (k, v) in obj {
                if let Some(s) = v.as_str() {
                    render_values.secrets.insert(k.clone(), s.to_string());
                }
            }
        }
    }
    if let Some(obj) = task.get("env").and_then(|v| v.as_object()) {
        for (k, v) in obj {
            if let Some(s) = v.as_str() {
                render_values.vars.insert(k.clone(), s.to_string());
            }
        }
    }
    let masking_input_keys: HashSet<String> = task
        .get("masking_input_keys")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    if let Some(obj) = task.get("inputs").and_then(|v| v.as_object()) {
        for (k, v) in obj {
            if masking_input_keys.contains(k) {
                continue;
            }
            if let Some(s) = v.as_str() {
                render_values.vars.insert(k.clone(), s.to_string());
                render_values
                    .vars
                    .insert(format!("INPUT_{}", k.to_uppercase()), s.to_string());
            }
        }
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

    // ---- Cancel watcher helper -------------------------------------------------
    /// Returns `true` when the job has been cancelled.
    /// Polls the Manager every 2 s.
    async fn cancel_watcher(client: &Client, config: &Config, token: &str, job_id: &str) -> bool {
        loop {
            tokio::time::sleep(Duration::from_secs(2)).await;
            if check_job_cancelled(client, config, token, job_id)
                .await
                .unwrap_or(false)
            {
                return true;
            }
        }
    }
    // ---------------------------------------------------------------------------

    // Tracks whether any step failed under fail_strategy = "continue". The job
    // must end as failed even though execution continued past the failing step.
    let mut any_step_failed = false;

    for (idx, step) in tasks.iter().enumerate() {
        let step_name = step["name"].as_str().unwrap_or("unnamed");
        let command = step["command_template"].as_str().unwrap_or("");
        let work_dir = step["work_dir"].as_str().filter(|s| !s.is_empty());
        let exec_type = step["exec_type"].as_str().unwrap_or("shell");
        let fail_strategy = step["fail_strategy"].as_str().unwrap_or("stop");
        let step_timeout_secs = step["timeout_seconds"].as_u64().unwrap_or(0);
        let step_timeout = if step_timeout_secs > 0 {
            Some(Duration::from_secs(step_timeout_secs))
        } else {
            None
        };
        let has_timeout = step_timeout.is_some();

        // ── Build step-specific env: base (inputs+pipeline-secrets) + step-level secrets ──
        let mut step_env = base_env.clone();
        let mut catalog = SecretCatalog::new();
        catalog.add_keys(pipeline_secret_keys.iter().cloned());
        if let Some(ss_obj) = step.get("step_secrets").and_then(|v| v.as_object()) {
            for (k, v) in ss_obj {
                if let Some(s) = v.as_str() {
                    step_env.insert(k.clone(), s.to_string());
                    catalog.add_key(k.clone());
                }
            }
        }

        // OSS config delivery: expose the signed GET URL as $BUZ_CONFIG_URL for this
        // step. The key is registered as "injected" so the RUN echo shows a readable
        // `BUZ_CONFIG_URL=<injected>` hint instead of `***`, while the URL value
        // still joins value-based redaction via `log_mask_values`.
        if let Some(url) = step
            .get("config_auth")
            .and_then(|a| a.get("url"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            step_env.insert("BUZ_CONFIG_URL".to_string(), url.to_string());
            catalog.add_injected_key("BUZ_CONFIG_URL".to_string());
        }

        // Sensitive values are registered by the Manager per step in
        // `log_mask_values` (registry password, OSS signed URL, …).
        if let Some(values) = step.get("log_mask_values").and_then(|v| v.as_array()) {
            for v in values.iter().filter_map(|x| x.as_str()) {
                catalog.add_value(v.to_string());
            }
        }

        // Debug mode: RUN echo + streams show real values, so the catalog is emptied.
        if debug_unredact {
            catalog = SecretCatalog::new();
        }

        // Collect values for log masking — secrets, registry passwords, masking inputs.
        let secret_values = catalog.secret_values(&step_env);
        // Forced values: rendered `.env` secret values that must be masked
        // regardless of length (plaintext `.env` content may be echoed by the
        // run block, e.g. `cat .env`).
        let forced_values = catalog.forced_secret_values();

        let log_base = step_log_base(idx);

        // Verbose mode: stage marker + STEP CONFIG up front (before prepare /
        // registry fail-fast) so a prepare-stage failure still shows the step
        // shape. Debug alone does not emit these — it only unmasks values.
        let step_started = std::time::Instant::now();
        let registry_secret_ref_early = step
            .get("registry_secret")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        if verbose {
            let _ = super::logs::upload_chunk(
                client,
                config,
                token,
                job_id,
                LogChunk {
                    chunk_index: log_base + LOG_STEP_VERBOSE_PREPARE,
                    step_order: idx as i32,
                    content: build_verbose_stage("prepare"),
                },
            )
            .await;
            let cfg = build_verbose_step_config(step, exec_type, registry_secret_ref_early);
            let _ = super::logs::upload_chunk(
                client,
                config,
                token,
                job_id,
                LogChunk {
                    chunk_index: log_base + LOG_STEP_VERBOSE_CONFIG,
                    step_order: idx as i32,
                    content: cfg,
                },
            )
            .await;
        }

        if let Err(e) = config.engine.ensure_enabled(exec_type) {
            let msg = e.to_string();
            let _ = super::logs::upload_chunk(
                client,
                config,
                token,
                job_id,
                LogChunk {
                    chunk_index: log_base + LOG_STEP_ERROR,
                    step_order: idx as i32,
                    content: format!("==> ERROR\n{msg}\n"),
                },
            )
            .await;
            update_job_status(client, config, token, job_id, "failed", Some(idx as i32)).await?;
            anyhow::bail!(msg);
        }

        let env_count = step_env.len();
        tracing::info!(
            "Step {idx}: {step_name} ({exec_type}){}",
            if env_count > 0 {
                format!(" [with {env_count} env vars]")
            } else {
                String::new()
            }
        );

        update_job_status(client, config, token, job_id, "running", Some(idx as i32)).await?;

        // Fail fast if Manager could not resolve registry_secret.
        if let Some(err) = step
            .get("registry_auth_error")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            let msg = format!("registry_secret: {err}");
            let _ = super::logs::upload_chunk(
                client,
                config,
                token,
                job_id,
                LogChunk {
                    chunk_index: log_base + LOG_STEP_ERROR,
                    step_order: idx as i32,
                    content: format!("==> ERROR\n{msg}\n"),
                },
            )
            .await;
            update_job_status(client, config, token, job_id, "failed", Some(idx as i32)).await?;
            anyhow::bail!(msg);
        }

        // Fail fast if Manager could not resolve config_source.
        if let Some(err) = step
            .get("config_auth_error")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            let msg = format!("config_source: {err}");
            let _ = super::logs::upload_chunk(
                client,
                config,
                token,
                job_id,
                LogChunk {
                    chunk_index: log_base + LOG_STEP_ERROR,
                    step_order: idx as i32,
                    content: format!("==> ERROR\n{msg}\n"),
                },
            )
            .await;
            if verbose {
                let result = build_verbose_step_result(step_started.elapsed(), -1);
                let _ = super::logs::upload_chunk(
                    client,
                    config,
                    token,
                    job_id,
                    LogChunk {
                        chunk_index: log_base + LOG_STEP_VERBOSE_RESULT,
                        step_order: idx as i32,
                        content: result,
                    },
                )
                .await;
            }
            update_job_status(client, config, token, job_id, "failed", Some(idx as i32)).await?;
            anyhow::bail!(msg);
        }

        // ── config prepare: OSS deliver and/or config_render ─────────────────
        // OSS: download → unpack → [render] → run.
        // Render-only: templates already at dest (shell in place / SSH fetch→
        // render on Runner → upload). See config-block.md.
        //
        // Prepare failures MUST be reported with this step's `step_order` (not
        // the job-level -1 catch in `run_one_job`) so Job Detail attaches the
        // ERROR to the correct terminal without frontend guessing.
        let mut config_prep: Option<super::config_source::ConfigPrep> = None;
        if super::config_source::needs_prepare(step) {
            // Verbose / Debug: show dest / object_key (+ URL when debug) before
            // prepare so prepare-only failures still differ from a normal run.
            if verbose || debug_unredact {
                let _ = super::logs::upload_chunk(
                    client,
                    config,
                    token,
                    job_id,
                    LogChunk {
                        chunk_index: log_base + LOG_STEP_VERBOSE_CONFIG_PREP,
                        step_order: idx as i32,
                        content: build_config_prep_banner(step, debug_unredact),
                    },
                )
                .await;
            }
            let prep_result = if exec_type == "ssh" {
                let conns = resolve_ssh_conns(step, config);
                if conns.is_empty() {
                    // The SSH branch below reports the missing-host error.
                    Ok(None)
                } else {
                    super::config_source::prepare_ssh(
                        client,
                        step,
                        &conns,
                        Path::new(&config.runner.workspace_dir),
                        job_id,
                        idx,
                        &render_values,
                        step_timeout,
                    )
                    .await
                    .map(Some)
                }
            } else {
                super::config_source::prepare_local(
                    client,
                    step,
                    Path::new(&config.runner.workspace_dir),
                    idx,
                    &render_values,
                )
                .await
                .map(Some)
            };
            match prep_result {
                Ok(prep) => config_prep = prep,
                Err(e) => {
                    let msg = e.to_string();
                    let _ = super::logs::upload_chunk(
                        client,
                        config,
                        token,
                        job_id,
                        LogChunk {
                            chunk_index: log_base + LOG_STEP_ERROR,
                            step_order: idx as i32,
                            content: format!("==> ERROR\n{msg}\n"),
                        },
                    )
                    .await;
                    if verbose {
                        let result =
                            build_verbose_step_result(step_started.elapsed(), -1);
                        let _ = super::logs::upload_chunk(
                            client,
                            config,
                            token,
                            job_id,
                            LogChunk {
                                chunk_index: log_base + LOG_STEP_VERBOSE_RESULT,
                                step_order: idx as i32,
                                content: result,
                            },
                        )
                        .await;
                    }
                    update_job_status(client, config, token, job_id, "failed", Some(idx as i32))
                        .await?;
                    anyhow::bail!(msg);
                }
            }
        }

        // Inject BUZ_CONFIG_DIR / BUZ_ENV_CHANGED(_KEYS) into the step env and
        // register every rendered secret value for forced value redaction.
        if let Some(prep) = &config_prep {
            for (k, v) in &prep.env {
                step_env.insert(k.clone(), v.clone());
            }
            // Debug mode disables all redaction, forced values included.
            if !debug_unredact {
                for v in &prep.forced_redact {
                    catalog.add_forced_value(v.clone());
                }
            }
        }

        let registry_secret_ref = registry_secret_ref_early;

        let registry_login = step.get("registry_auth").and_then(|a| {
            let registry = a.get("registry")?.as_str()?.trim();
            let username = a.get("username")?.as_str()?.trim();
            let password = a.get("password")?.as_str()?;
            if registry.is_empty() || username.is_empty() || password.is_empty() {
                return None;
            }
            Some((
                registry.to_string(),
                username.to_string(),
                password.to_string(),
            ))
        });

        // registry_secret set but no usable registry_auth → never silent-skip login.
        if !registry_secret_ref.is_empty() && registry_login.is_none() {
            let msg = if step.get("registry_auth").is_some() {
                format!(
                    "registry_secret '{registry_secret_ref}' resolved but \
                     registry/username/password was empty"
                )
            } else {
                format!(
                    "registry_secret '{registry_secret_ref}' is set but Manager did not \
                     attach registry_auth — restart Manager with a build that resolves \
                     registry_secret at claim time"
                )
            };
            let _ = super::logs::upload_chunk(
                client,
                config,
                token,
                job_id,
                LogChunk {
                    chunk_index: log_base + LOG_STEP_ERROR,
                    step_order: idx as i32,
                    content: format!("==> ERROR\n{msg}\n"),
                },
            )
            .await;
            update_job_status(client, config, token, job_id, "failed", Some(idx as i32)).await?;
            anyhow::bail!(msg);
        }

        // Verbose: exec-stage marker only (STEP CONFIG already uploaded above).
        if verbose {
            let _ = super::logs::upload_chunk(
                client,
                config,
                token,
                job_id,
                LogChunk {
                    chunk_index: log_base + LOG_STEP_VERBOSE_EXEC,
                    step_order: idx as i32,
                    content: build_verbose_stage("exec"),
                },
            )
            .await;
        }

        // ── Race step execution against cancel polling ────────────────────────
        // SSH output is uploaded as one combined chunk — mask it with the SSH env
        // catalog (which includes BUZ_REGISTRY_PASSWORD), not the local one.
        let mut output_secret_values = secret_values.clone();
        let (output, was_cancelled): (engine::shell::ShellOutput, bool) = match exec_type {
            "local" | "shell" => {
                if let Some((ref registry, ref username, ref password)) = registry_login {
                    if let Err(e) =
                        engine::docker::registry_login(registry, username, password).await
                    {
                        let msg = e.to_string();
                        let masked = crate::masker::mask_secrets_forced(
                            &crate::masker::mask_secrets(&msg, &secret_values),
                            &forced_values,
                        );
                        let _ = super::logs::upload_chunk(
                            client,
                            config,
                            token,
                            job_id,
                            LogChunk {
                                chunk_index: log_base + LOG_STEP_ERROR,
                                step_order: idx as i32,
                                content: format!("==> ERROR\n{masked}\n"),
                            },
                        )
                        .await;
                        update_job_status(
                            client,
                            config,
                            token,
                            job_id,
                            "failed",
                            Some(idx as i32),
                        )
                        .await?;
                        anyhow::bail!(masked);
                    }
                    let _ = super::logs::upload_chunk(
                        client,
                        config,
                        token,
                        job_id,
                        LogChunk {
                            chunk_index: log_base + LOG_LOGIN,
                            step_order: idx as i32,
                            content: build_login_banner(registry),
                        },
                    )
                    .await;
                }

                let cmd = command.to_string();
                let wd = work_dir.map(|s| s.to_string());
                let env = step_env.clone();

                // Debug echo — show the command before execution (like bash -x)
                {
                    let echo_line = build_step_echo(exec_type, &cmd, &env, &catalog, work_dir);
                    let _ = super::logs::upload_chunk(
                        client,
                        config,
                        token,
                        job_id,
                        LogChunk {
                            chunk_index: log_base + LOG_ECHO,
                            step_order: idx as i32,
                            content: echo_line,
                        },
                    )
                    .await;
                }

                let c = client.clone();
                let cfg = config.clone();
                let t = token.to_string();
                let jid = job_id.to_string();
                let sname = step_name.to_string();

                let exec_result =
                    engine::shell::execute_streaming(&cmd, wd.as_deref(), Some(&env)).await;
                let (mut child, mut output_rx, read_guard) = match exec_result {
                    Ok(v) => v,
                    Err(e) => {
                        if let Some((ref reg, _, _)) = registry_login {
                            engine::docker::registry_logout(reg).await;
                        }
                        return Err(e);
                    }
                };

                // Clone secret values for use in the async upload task
                let sv_for_upload = secret_values.clone();
                let forced_for_upload = forced_values.clone();

                // Background task: stream output chunks to the manager in real time
                let c_log = client.clone();
                let cfg_log = config.clone();
                let t_log = token.to_string();
                let jid_log = job_id.to_string();
                let idx_i32 = idx as i32;
                let stream_base = log_base + LOG_STREAM_START;
                let upload_task = tokio::spawn(async move {
                    let mut chunk_num = stream_base;
                    while let Some(chunk) = output_rx.recv().await {
                        let masked_chunk = crate::masker::mask_secrets_forced(
                            &crate::masker::mask_secrets(&chunk, &sv_for_upload),
                            &forced_for_upload,
                        );
                        match super::logs::upload_chunk(
                            &c_log,
                            &cfg_log,
                            &t_log,
                            &jid_log,
                            LogChunk {
                                chunk_index: chunk_num,
                                step_order: idx_i32,
                                content: masked_chunk,
                            },
                        )
                        .await
                        {
                            Ok(_) => {}
                            Err(e) => tracing::warn!("Log upload chunk {chunk_num} failed: {e}"),
                        }
                        chunk_num += 1;
                    }
                });

                // Race: child completion vs cancellation vs timeout
                let (exit_code, was_cancelled_local, was_timeout) = tokio::select! {
                    status = child.wait() => {
                        let code = status?.code().unwrap_or(-1);
                        // Ensure all pipe data is read before proceeding
                        let _ = read_guard.join().await;
                        let _ = upload_task.await;
                        (code, false, false)
                    }
                    _ = cancel_watcher(&c, &cfg, &t, &jid) => {
                        tracing::warn!("Job {jid} cancelled during step '{sname}' — killing");
                        let _ = child.kill().await;
                        let _ = child.wait().await;
                        upload_task.abort();
                        (-1, true, false)
                    }
                    _ = tokio::time::sleep(step_timeout.unwrap_or(Duration::from_secs(3600))), if has_timeout => {
                        tracing::warn!("Step '{sname}' timed out after {step_timeout_secs}s — killing process");
                        let _ = child.kill().await;
                        let _ = child.wait().await;
                        let _ = read_guard.join().await;
                        upload_task.abort();
                        (-1, false, true)
                    }
                };

                if let Some((ref reg, _, _)) = registry_login {
                    engine::docker::registry_logout(reg).await;
                }

                (
                    engine::shell::ShellOutput {
                        stdout: String::new(),
                        stderr: if was_cancelled_local {
                            "==> CANCELLED\n".into()
                        } else if was_timeout {
                            format!("==> TIMEOUT [{step_timeout_secs}s]\n")
                        } else {
                            String::new()
                        },
                        exit_code: if was_cancelled_local || was_timeout {
                            -1
                        } else {
                            exit_code
                        },
                    },
                    was_cancelled_local,
                )
            }
            "ssh" => {
                let host_values = step["host"]
                    .as_array()
                    .cloned()
                    .or_else(|| {
                        step["host"]
                            .as_object()
                            .map(|o| vec![serde_json::Value::Object(o.clone())])
                    })
                    .unwrap_or_else(|| {
                        let _msg =
                            format!("SSH step '{step_name}' missing host connection details");
                        vec![]
                    });

                if host_values.is_empty() {
                    let msg = if step["host"].is_null() {
                        format!("SSH step '{step_name}' missing host connection details")
                    } else {
                        format!("SSH step '{step_name}': no hosts resolved from the database. Register your hosts in the Manager UI first.")
                    };
                    // Upload the error as a log chunk so it appears in the step's terminal
                    let _ = super::logs::upload_chunk(
                        client,
                        config,
                        token,
                        job_id,
                        LogChunk {
                            chunk_index: log_base + LOG_STEP_ERROR,
                            step_order: idx as i32,
                            content: format!("==> ERROR\n{msg}\n"),
                        },
                    )
                    .await;
                    update_job_status(client, config, token, job_id, "failed", Some(idx as i32))
                        .await?;
                    anyhow::bail!(msg);
                }

                let mut ssh_env = step_env.clone();
                let mut ssh_catalog = catalog.clone();
                let cmd = if let Some((ref registry, ref username, ref password)) = registry_login {
                    ssh_env.insert("BUZ_REGISTRY_HOST".into(), registry.clone());
                    ssh_env.insert("BUZ_REGISTRY_USER".into(), username.clone());
                    ssh_env.insert("BUZ_REGISTRY_PASSWORD".into(), password.clone());
                    ssh_catalog.add_key("BUZ_REGISTRY_PASSWORD".to_string());
                    wrap_remote_cmd_with_registry_login(command)
                } else {
                    command.to_string()
                };
                let c = client.clone();

                // Debug echo — show the command before execution (like bash -x)
                {
                    let echo_line =
                        build_step_echo(exec_type, &cmd, &ssh_env, &ssh_catalog, work_dir);
                    let _ = super::logs::upload_chunk(
                        client,
                        config,
                        token,
                        job_id,
                        LogChunk {
                            chunk_index: log_base + LOG_ECHO,
                            step_order: idx as i32,
                            content: echo_line,
                        },
                    )
                    .await;
                }

                // Remote output is uploaded combined — mask with the SSH catalog
                // (covers BUZ_REGISTRY_PASSWORD injected into the remote env).
                output_secret_values = ssh_catalog.secret_values(&ssh_env);

                let cfg = config.clone();
                let cfg_ssh = cfg.clone();
                let t = token.to_string();
                let jid = job_id.to_string();
                let sname = step_name.to_string();
                let sname2 = sname.clone();
                let hosts_json = serde_json::Value::Array(host_values);
                let step_env_ssh = ssh_env;

                let step_task = tokio::spawn(async move {
                    execute_ssh_step(
                        &cfg_ssh,
                        &cmd,
                        &hosts_json,
                        step_timeout,
                        &sname,
                        &step_env_ssh,
                    )
                    .await
                });
                let abort_handle = step_task.abort_handle();

                tokio::select! {
                    result = step_task => {
                        (result??, false)
                    }
                    _ = cancel_watcher(&c, &cfg, &t, &jid) => {
                        tracing::warn!("Job {jid} cancelled during SSH step '{sname2}' — aborting");
                        abort_handle.abort();
                        (engine::shell::ShellOutput {
                            stdout: String::new(),
                            stderr: "==> CANCELLED\n".to_string(),
                            exit_code: -1,
                        }, true)
                    }
                }
            }
            "docker" => {
                let image = step["image"].as_str().unwrap_or("").trim();
                if let Err(e) = engine::docker::preflight(config.engine.docker_enabled, image) {
                    let msg = e.to_string();
                    let _ = super::logs::upload_chunk(
                        client,
                        config,
                        token,
                        job_id,
                        LogChunk {
                            chunk_index: log_base + LOG_STEP_ERROR,
                            step_order: idx as i32,
                            content: format!("==> ERROR\n{msg}\n"),
                        },
                    )
                    .await;
                    update_job_status(client, config, token, job_id, "failed", Some(idx as i32))
                        .await?;
                    anyhow::bail!(msg);
                }

                let pull_registry = registry_login.as_ref().map(|(r, _, _)| r.clone());
                if let Some((ref registry, ref username, ref password)) = registry_login {
                    if let Err(e) =
                        engine::docker::registry_login(registry, username, password).await
                    {
                        let msg = e.to_string();
                        let masked = crate::masker::mask_secrets_forced(
                            &crate::masker::mask_secrets(&msg, &secret_values),
                            &forced_values,
                        );
                        let _ = super::logs::upload_chunk(
                            client,
                            config,
                            token,
                            job_id,
                            LogChunk {
                                chunk_index: log_base + LOG_STEP_ERROR,
                                step_order: idx as i32,
                                content: format!("==> ERROR\n{masked}\n"),
                            },
                        )
                        .await;
                        update_job_status(
                            client,
                            config,
                            token,
                            job_id,
                            "failed",
                            Some(idx as i32),
                        )
                        .await?;
                        anyhow::bail!(masked);
                    }
                    let _ = super::logs::upload_chunk(
                        client,
                        config,
                        token,
                        job_id,
                        LogChunk {
                            chunk_index: log_base + LOG_LOGIN,
                            step_order: idx as i32,
                            content: build_login_banner(registry),
                        },
                    )
                    .await;
                }

                let cmd = command.to_string();
                let volumes: Vec<String> = step
                    .get("volumes")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|x| x.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                let privileged = step
                    .get("privileged")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);

                // Debug echo — show the command before execution (like bash -x)
                {
                    let echo_line = build_step_echo(exec_type, &cmd, &step_env, &catalog, work_dir);
                    let _ = super::logs::upload_chunk(
                        client,
                        config,
                        token,
                        job_id,
                        LogChunk {
                            chunk_index: log_base + LOG_ECHO,
                            step_order: idx as i32,
                            content: echo_line,
                        },
                    )
                    .await;
                }

                let img = image.to_string();
                let env = step_env.clone();
                let c = client.clone();
                let cfg = config.clone();
                let t = token.to_string();
                let jid = job_id.to_string();
                let sname = step_name.to_string();
                let sv_for_upload = secret_values.clone();
                let forced_for_upload = forced_values.clone();

                let exec_result = engine::docker::execute_streaming(
                    &img,
                    Path::new(&config.runner.workspace_dir),
                    &cmd,
                    &env,
                    &volumes,
                    work_dir,
                    privileged,
                )
                .await;

                let (mut child, mut output_rx, read_guard) = match exec_result {
                    Ok(v) => v,
                    Err(e) => {
                        if let Some(reg) = &pull_registry {
                            engine::docker::registry_logout(reg).await;
                        }
                        return Err(e);
                    }
                };

                let c_log = client.clone();
                let cfg_log = config.clone();
                let t_log = token.to_string();
                let jid_log = job_id.to_string();
                let idx_i32 = idx as i32;
                let stream_base = log_base + LOG_STREAM_START;
                let upload_task = tokio::spawn(async move {
                    let mut chunk_num = stream_base;
                    while let Some(chunk) = output_rx.recv().await {
                        let masked_chunk = crate::masker::mask_secrets_forced(
                            &crate::masker::mask_secrets(&chunk, &sv_for_upload),
                            &forced_for_upload,
                        );
                        match super::logs::upload_chunk(
                            &c_log,
                            &cfg_log,
                            &t_log,
                            &jid_log,
                            LogChunk {
                                chunk_index: chunk_num,
                                step_order: idx_i32,
                                content: masked_chunk,
                            },
                        )
                        .await
                        {
                            Ok(_) => {}
                            Err(e) => tracing::warn!("Log upload chunk {chunk_num} failed: {e}"),
                        }
                        chunk_num += 1;
                    }
                });

                let (exit_code, was_cancelled_docker, was_timeout_docker) = tokio::select! {
                    status = child.wait() => {
                        let code = status?.code().unwrap_or(-1);
                        let _ = read_guard.join().await;
                        let _ = upload_task.await;
                        (code, false, false)
                    }
                    _ = cancel_watcher(&c, &cfg, &t, &jid) => {
                        tracing::warn!("Job {jid} cancelled during docker step '{sname}' — killing container process");
                        let _ = child.kill().await;
                        let _ = child.wait().await;
                        upload_task.abort();
                        (-1, true, false)
                    }
                    _ = tokio::time::sleep(step_timeout.unwrap_or(Duration::from_secs(3600))), if has_timeout => {
                        tracing::warn!("Docker step '{sname}' timed out after {step_timeout_secs}s — killing container");
                        let _ = child.kill().await;
                        let _ = child.wait().await;
                        let _ = read_guard.join().await;
                        upload_task.abort();
                        (-1, false, true)
                    }
                };

                if let Some(reg) = &pull_registry {
                    engine::docker::registry_logout(reg).await;
                }

                (
                    engine::shell::ShellOutput {
                        stdout: String::new(),
                        stderr: if was_cancelled_docker {
                            "==> CANCELLED\n".into()
                        } else if was_timeout_docker {
                            format!("==> TIMEOUT [{step_timeout_secs}s]\n")
                        } else {
                            String::new()
                        },
                        exit_code: if was_cancelled_docker || was_timeout_docker {
                            -1
                        } else {
                            exit_code
                        },
                    },
                    was_cancelled_docker,
                )
            }
            other => anyhow::bail!("unsupported engine type: {other}"),
        };
        // ─────────────────────────────────────────────────────────────────────

        if was_cancelled {
            let _ = super::logs::upload_chunk(
                client,
                config,
                token,
                job_id,
                LogChunk {
                    chunk_index: log_base + LOG_STEP_META_A,
                    step_order: idx as i32,
                    content: "==> CANCELLED\nJob was cancelled by user.\n".into(),
                },
            )
            .await;
            update_job_status(client, config, token, job_id, "cancelled", Some(idx as i32)).await?;
            anyhow::bail!("job cancelled by user");
        }

        // For SSH steps, upload the accumulated output as a summary chunk.
        // Local steps are fully streamed so we skip this redundant upload.
        if exec_type == "ssh" {
            let combined = format!("==> {step_name}\n{}\n{}", output.stdout, output.stderr);
            let masked = crate::masker::mask_secrets_forced(
                &crate::masker::mask_secrets(&combined, &output_secret_values),
                &forced_values,
            );
            super::logs::upload_chunk(
                client,
                config,
                token,
                job_id,
                LogChunk {
                    chunk_index: log_base + LOG_STEP_META_B,
                    step_order: idx as i32,
                    content: masked,
                },
            )
            .await?;
        }

        // Verbose mode: result summary (duration + exit code). Uploaded even on
        // failure so troubleshooting users see exactly how long the step ran.
        if verbose && !was_cancelled {
            let result = build_verbose_step_result(step_started.elapsed(), output.exit_code);
            let _ = super::logs::upload_chunk(
                client,
                config,
                token,
                job_id,
                LogChunk {
                    chunk_index: log_base + LOG_STEP_VERBOSE_RESULT,
                    step_order: idx as i32,
                    content: result,
                },
            )
            .await;
        }

        if output.exit_code != 0 && fail_strategy == "stop" {
            update_job_status(client, config, token, job_id, "failed", Some(idx as i32)).await?;
            let hint = step_failure_hint(&output.stderr, output.exit_code);
            let msg = build_step_failure_message(step_name, output.exit_code, command, &hint);
            // Upload the failure into this step's terminal so it is visible in the
            // UI. Prefer step_order=idx over run_one_job's global -1 catch — the
            // frontend maps -1 as a fallback, but per-step errors must not rely on it.
            let _ = super::logs::upload_chunk(
                client,
                config,
                token,
                job_id,
                LogChunk {
                    chunk_index: log_base + LOG_STEP_ERROR,
                    step_order: idx as i32,
                    content: format!("==> ERROR\n{msg}\n"),
                },
            )
            .await;
            anyhow::bail!("{}", msg);
        } else if output.exit_code != 0 && fail_strategy == "continue" {
            any_step_failed = true;
            update_job_status(client, config, token, job_id, "failed", Some(idx as i32)).await?;
        } else {
            update_job_status(client, config, token, job_id, "success", Some(idx as i32)).await?;
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

/// Build an SSH command string with environment variable exports prepended.
/// Follows the same INPUT_* convention as the shell and docker engines,
/// and also exports step-level secret values.
fn build_ssh_env_command(command: &str, env: &std::collections::HashMap<String, String>) -> String {
    let mut prefix = String::from("set -e; ");
    if env.is_empty() {
        return format!("{prefix}{command}");
    }
    for (k, v) in env {
        // Export the raw variable
        let safe_v = shell_escape(v);
        prefix.push_str(&format!("export {k}={safe_v} && "));
        // Also export as INPUT_* for consistency (pipeline inputs convention)
        let input_key = format!("INPUT_{}", k.to_uppercase());
        prefix.push_str(&format!("export {input_key}={safe_v} && "));
    }
    format!("{}{}", prefix, command)
}

/// Minimal shell-escaping of values in single-quoted `export` statements.
fn shell_escape(value: &str) -> String {
    let escaped = value.replace('\'', "'\\''");
    format!("'{escaped}'")
}

/// Wrap a remote (SSH) command with registry login/logout using env credentials.
///
/// Expects `BUZ_REGISTRY_HOST`, `BUZ_REGISTRY_USER`, `BUZ_REGISTRY_PASSWORD`
/// already exported on the remote (via [`build_ssh_env_command`]).
///
/// `docker logout` runs via an EXIT trap so `set -e` (prepended by
/// [`build_ssh_env_command`]) stays active for the user command: fail-fast is
/// built in and a failing command still triggers the logout before the shell
/// exits with its exit code.
fn wrap_remote_cmd_with_registry_login(user_command: &str) -> String {
    format!(
        r#"printf '%s\n' "$BUZ_REGISTRY_PASSWORD" | docker login -u "$BUZ_REGISTRY_USER" --password-stdin "$BUZ_REGISTRY_HOST" || exit $?
trap 'docker logout "$BUZ_REGISTRY_HOST" >/dev/null 2>&1 || true' EXIT
{user_command}"#
    )
}

/// Parse the step's `host` field (Manager-resolved connection objects) into
/// connection params. Used by the config-source delivery path (which runs
/// before the run block) and by `execute_ssh_step`.
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

/// Extracted SSH execution logic (keeps `execute_job` readable).
async fn execute_ssh_step(
    config: &Config,
    command: &str,
    host_values: &serde_json::Value,
    step_timeout: Option<std::time::Duration>,
    step_name: &str,
    env: &std::collections::HashMap<String, String>,
) -> anyhow::Result<engine::shell::ShellOutput> {
    let hosts = host_values
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("SSH step '{step_name}' host data is not an array"))?;

    let mut combined_stdout = String::new();
    let mut combined_stderr = String::new();
    let mut last_exit_code: i32 = 0;

    for (hi, hv) in hosts.iter().enumerate() {
        let host_label = hv["host"].as_str().unwrap_or("?");
        let ssh_conn = build_ssh_conn(hv, config)
            .map_err(|e| anyhow::anyhow!("SSH step '{step_name}' host[{hi}]: {e}"))?;

        let ssh_header = if hosts.len() > 1 {
            format!("--- {step_name} @ {host_label} ---\n")
        } else {
            String::new()
        };

        let ssh_cmd = build_ssh_env_command(command, env);
        match engine::ssh::execute(&ssh_cmd, &ssh_conn, step_timeout).await {
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
                last_exit_code = o.exit_code as i32;
                if o.exit_code != 0 {
                    tracing::warn!(
                        "SSH step '{step_name}' on {host_label} exited with code {}",
                        o.exit_code
                    );
                }
            }
            Err(e) => {
                tracing::error!("SSH step '{step_name}' on {host_label} failed: {e}");
                combined_stderr.push_str(&format!("{ssh_header}ERROR: {e}\n"));
                last_exit_code = -1;
            }
        }
    }

    Ok(engine::shell::ShellOutput {
        stdout: combined_stdout,
        stderr: combined_stderr,
        exit_code: last_exit_code,
    })
}

async fn update_job_status(
    client: &Client,
    config: &Config,
    token: &str,
    job_id: &str,
    status: &str,
    step_order: Option<i32>,
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

#[cfg(test)]
#[path = "poll_tests.rs"]
mod tests;
