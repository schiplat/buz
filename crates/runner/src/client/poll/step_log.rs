//! Step-log formatting layer: chunk layout constants, the per-step emitter
//! ([`StepLogger`]), output masking, and every `==>` banner builder.
//!
//! This module owns the text contract the Job Detail terminal renders; the
//! wire semantics (`LogChunk` fields, ordering) live in `shared::protocol`.

use reqwest::Client;
use shared::protocol::LogChunk;
use std::time::Duration;

use crate::config::Config;
use crate::masker::SecretCatalog;
use std::collections::HashMap;

use super::update_job_status;

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
///   9998  ==> CANCELLED (cancel marker)
///   9999  ==> output summary
///
/// The job-start runner identity banner uses `step_order = 0` with a negative
/// `chunk_index` (see `LOG_RUNNER_BANNER`), so it sorts before step 0's first
/// chunk. It must NOT reuse `-1`, which is reserved for the global
/// `step_order = -1` error path — `UNIQUE(job_id, chunk_index)` +
/// `INSERT OR REPLACE` would otherwise let the error chunk overwrite the banner.
/// ```
///
/// Verbose prep/config used to live at 9990+, which sorted **after** RUN +
/// stream and made Verbose look "out of order" once the job reloaded from DB.
pub(crate) fn step_log_base(step_idx: usize) -> i32 {
    (step_idx * 10_000) as i32
}

/// Job-start runner identity banner (`==> RUNNER`), ordered before step 0's
/// first chunk (`step_log_base(0) == 0`) so it appears at the very top of the
/// job's logs.
pub(crate) const LOG_RUNNER_BANNER: i32 = -2;

pub(crate) const LOG_STEP_VERBOSE_PREPARE: i32 = 0;
pub(crate) const LOG_STEP_VERBOSE_CONFIG: i32 = 1;
pub(crate) const LOG_STEP_VERBOSE_CONFIG_PREP: i32 = 2;
pub(crate) const LOG_LOGIN: i32 = 3;
pub(crate) const LOG_STEP_VERBOSE_EXEC: i32 = 4;
pub(crate) const LOG_ECHO: i32 = 5;
pub(crate) const LOG_STREAM_START: i32 = 6;
pub(crate) const LOG_STEP_VERBOSE_RESULT: i32 = 9993;
pub(crate) const LOG_STEP_ERROR: i32 = 9997;
const LOG_STEP_CANCELLED: i32 = 9998;
pub(super) const LOG_STEP_SUMMARY: i32 = 9999;

/// Two-pass output redaction shared by every execution path: value masking
/// first, then forced masking (rendered `.env` secret values,
/// length-independent).
pub(crate) fn mask_output(
    content: &str,
    secret_values: &[String],
    forced_values: &[String],
) -> String {
    crate::masker::mask_secrets_forced(
        &crate::masker::mask_secrets(content, secret_values),
        forced_values,
    )
}

/// `==> ERROR` banner content inside a step's log terminal.
pub(crate) fn step_error_content(msg: &str) -> String {
    format!("==> ERROR\n{msg}\n")
}

/// `==> CANCELLED` banner content for a user-cancelled step.
fn step_cancelled_content() -> String {
    "==> CANCELLED\nJob was cancelled by user.\n".into()
}

/// `==> TIMEOUT` stderr marker for a step killed at its deadline.
pub(super) fn step_timeout_content(timeout_secs: u64) -> String {
    format!("==> TIMEOUT [{timeout_secs}s]\n")
}

/// Per-step log emitter.
///
/// Binds the job-level upload context (client / config / token / job_id) to a
/// step's chunk layout (`log_base` + `step_order`), collapsing the five-arg
/// `upload_chunk` + hand-built `LogChunk` boilerplate to one call. Cheap to
/// clone for the spawned streaming-upload task.
#[derive(Clone)]
pub(super) struct StepLogger {
    pub(super) client: Client,
    pub(super) config: Config,
    pub(super) token: String,
    pub(super) job_id: String,
    pub(super) log_base: i32,
    pub(super) step_order: i32,
}

impl StepLogger {
    pub(super) fn new(
        client: &Client,
        config: &Config,
        token: &str,
        job_id: &str,
        idx: usize,
    ) -> Self {
        Self {
            client: client.clone(),
            config: config.clone(),
            token: token.to_string(),
            job_id: job_id.to_string(),
            log_base: step_log_base(idx),
            step_order: idx as i32,
        }
    }

    /// Best-effort emit at `log_base + offset`: upload failures are logged,
    /// never fail the step.
    pub(super) async fn emit(&self, offset: i32, content: impl Into<String>) {
        if let Err(error) = self.try_emit(offset, content).await {
            tracing::warn!(
                step = self.step_order,
                "step log chunk upload failed: {error}"
            );
        }
    }

    /// Emit at an absolute chunk index (streamed-output numbering).
    pub(super) async fn emit_at(
        &self,
        chunk_index: i32,
        content: impl Into<String>,
    ) -> anyhow::Result<()> {
        crate::client::logs::upload_chunk(
            &self.client,
            &self.config,
            &self.token,
            &self.job_id,
            LogChunk {
                chunk_index,
                step_order: self.step_order,
                content: content.into(),
            },
        )
        .await
    }

    /// Emit at `log_base + offset`, propagating upload errors (for the few
    /// callers where a failed upload must fail the step).
    pub(super) async fn try_emit(
        &self,
        offset: i32,
        content: impl Into<String>,
    ) -> anyhow::Result<()> {
        self.emit_at(self.log_base + offset, content).await
    }

    /// Step-failure boilerplate: ERROR chunk → job status "failed" → the error
    /// the caller propagates (`return Err(log.fail_step(&msg).await)`). `msg`
    /// must already be masked when it can contain secret values. A failed
    /// status update takes precedence, matching the previous
    /// `update_job_status(...).await?` + `bail!` sequence.
    pub(super) async fn fail_step(&self, msg: impl Into<String>) -> anyhow::Error {
        let msg = msg.into();
        self.emit(LOG_STEP_ERROR, step_error_content(&msg)).await;
        if let Err(status_error) = update_job_status(
            &self.client,
            &self.config,
            &self.token,
            &self.job_id,
            "failed",
            Some(self.step_order),
        )
        .await
        {
            return status_error;
        }
        anyhow::anyhow!(msg)
    }

    /// Cancel boilerplate: CANCELLED chunk → job status "cancelled" → the
    /// sentinel error `run_one_job` maps to a cancelled report.
    pub(super) async fn cancel_step(&self) -> anyhow::Error {
        self.emit(LOG_STEP_CANCELLED, step_cancelled_content())
            .await;
        if let Err(status_error) = update_job_status(
            &self.client,
            &self.config,
            &self.token,
            &self.job_id,
            "cancelled",
            Some(self.step_order),
        )
        .await
        {
            return status_error;
        }
        anyhow::anyhow!("job cancelled by user")
    }

    /// Stream engine output: mask each chunk, upload under consecutive indexes
    /// from `LOG_STREAM_START`. Spawned as the background upload task; upload
    /// failures are logged, never fatal.
    pub(super) async fn stream_chunks(
        &self,
        output_rx: &mut tokio::sync::mpsc::UnboundedReceiver<String>,
        secret_values: &[String],
        forced_values: &[String],
    ) {
        let mut chunk_num = self.log_base + LOG_STREAM_START;
        while let Some(chunk) = output_rx.recv().await {
            let masked = mask_output(&chunk, secret_values, forced_values);
            if let Err(error) = self.emit_at(chunk_num, masked).await {
                tracing::warn!("Log upload chunk {chunk_num} failed: {error}");
            }
            chunk_num += 1;
        }
    }
}

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

/// Identity banner for the runner that executed this job (version + OS),
/// uploaded once at job start so every job's logs are self-describing.
pub(crate) fn build_runner_banner() -> String {
    let mut out = String::from("==> RUNNER\n");
    out.push_str(&format!(
        "  {:<16} {}\n",
        "version",
        shared::version::VERSION
    ));
    out.push_str(&format!(
        "  {:<16} {}\n",
        "os",
        crate::sysinfo::os_description()
    ));
    out
}

/// Banner after SSH auth: target (`user@host:port`) and remote workspace (`pwd`).
///
/// Never includes credentials. Empty workspace omits the workspace line.
pub(crate) fn build_ssh_banner(targets: &[(String, String)]) -> String {
    let mut out = String::new();
    for (target, workspace) in targets {
        out.push_str(&format!("==> SSH [{target}]\n"));
        if !workspace.is_empty() {
            out.push_str(&format!("  workspace  {workspace}\n"));
        }
    }
    out
}

/// Resolved SSH targets from Manager `host` payload objects (no credentials).
pub(crate) fn format_resolved_ssh_targets(step: &serde_json::Value) -> Vec<String> {
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
        .filter_map(|hv| {
            let obj = hv.as_object()?;
            let host = obj.get("host")?.as_str()?.trim();
            if host.is_empty() {
                return None;
            }
            let user = obj
                .get("username")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim();
            let port = obj.get("port").and_then(|v| v.as_u64()).unwrap_or(22);
            let user = if user.is_empty() { "?" } else { user };
            Some(format!("{user}@{host}:{port}"))
        })
        .collect()
}

/// True when SSH multi-host fan-out should run in parallel (`hosts_strategy: parallel`).
/// Omitted / `chain` / legacy `serial` → chain (one after another).
/// Verbose-mode stage marker (`==> STAGE …`).
pub(super) fn build_verbose_stage(stage: &str) -> String {
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
                    let o = e.get("output").and_then(|v| v.as_str()).unwrap_or(".env");
                    Some(format!("{t} → {o}"))
                })
                .collect()
        } else {
            let t = er
                .get("template")
                .and_then(|v| v.as_str())
                .unwrap_or("env.example");
            let o = er.get("output").and_then(|v| v.as_str()).unwrap_or(".env");
            vec![format!("{t} → {o}")]
        };
        if !pairs.is_empty() {
            push_field("config_render", pairs.join(", "));
        }
    }
    let resolved = format_resolved_ssh_targets(step);
    if !resolved.is_empty() {
        push_field("hosts", resolved.join(", "));
    } else if let Some(hosts) = step["hosts"].as_array() {
        let joined: Vec<&str> = hosts.iter().filter_map(|h| h.as_str()).collect();
        if !joined.is_empty() {
            push_field("hosts", joined.join(", "));
        }
    }
    if exec_type == "ssh" {
        let strategy = step["hosts_strategy"].as_str().unwrap_or("chain");
        push_field("hosts_strategy", strategy.to_string());
    }
    out
}

/// URL age / remaining lifetime of a Manager-signed config bundle URL.
///
/// Populated from the `signed_at_epoch` / `expires_epoch` fields the Manager
/// stamps into `config_auth`; absent when talking to an older Manager.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConfigUrlAge {
    pub signed_at_epoch: i64,
    pub expires_epoch: i64,
    pub age_secs: i64,
    pub remaining_secs: i64,
}

impl ConfigUrlAge {
    /// True once OSS would reject the URL for being expired.
    pub(crate) fn is_expired(&self) -> bool {
        self.remaining_secs <= 0
    }

    /// Effective lifetime granted at signing time.
    pub(crate) fn ttl_secs(&self) -> i64 {
        self.expires_epoch - self.signed_at_epoch
    }

    /// `3598s` while valid, `EXPIRED (61s ago)` once past the deadline.
    pub(crate) fn remaining_label(&self) -> String {
        if self.remaining_secs > 0 {
            format!("{}s", self.remaining_secs)
        } else {
            format!("EXPIRED ({}s ago)", -self.remaining_secs)
        }
    }
}

/// Read the signed-URL window out of `config_auth` against a caller-supplied
/// `now` (kept explicit so tests are deterministic).
pub(crate) fn config_url_age(step: &serde_json::Value, now_epoch: i64) -> Option<ConfigUrlAge> {
    let auth = step.get("config_auth")?;
    let signed_at_epoch = auth.get("signed_at_epoch").and_then(|v| v.as_i64())?;
    let expires_epoch = auth.get("expires_epoch").and_then(|v| v.as_i64())?;
    Some(ConfigUrlAge {
        signed_at_epoch,
        expires_epoch,
        age_secs: now_epoch - signed_at_epoch,
        remaining_secs: expires_epoch - now_epoch,
    })
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
        if let Some(url) = auth
            .get("url")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            if debug_unredact {
                push_field("url", url.to_string());
            } else {
                push_field("url", "***".into());
            }
        }
        // Absolute URL window: makes a stale (or born-expired) URL visible in
        // the job log instead of only as an opaque OSS 403 further down.
        if let Some(age) = config_url_age(step, chrono::Utc::now().timestamp()) {
            push_field("signed_at", shared::clock::format_utc(age.signed_at_epoch));
            push_field("url_age", format!("{}s", age.age_secs.max(0)));
            push_field("url_remaining", age.remaining_label());
            if age.is_expired() {
                push_field(
                    "url_warning",
                    format!(
                        "signed {}s ago with a {}s TTL — already expired before download; \
                         either the Manager clock lagged at signing time or the task waited too long",
                        age.age_secs.max(0),
                        age.ttl_secs()
                    ),
                );
            }
        }
    } else if step.get("config_render").is_some_and(|v| !v.is_null()) {
        push_field("mode", "render-only".into());
    }
    out
}

/// Render config-prepare progress lines (download result, unpack listing,
/// per-file render, readiness) for the `==> CONFIG PREP` chunk. Verbose-only
/// lines are dropped unless `verbose` is set. Always safe — carries no secrets
/// or signed URLs, only status, sizes and file/template names.
pub(crate) fn build_config_prep_progress(
    logs: &[crate::client::config_source::PrepLogLine],
    verbose: bool,
) -> String {
    let mut out = String::new();
    for l in logs {
        if l.verbose_only && !verbose {
            continue;
        }
        out.push_str(&l.text);
        out.push('\n');
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

#[cfg(test)]
#[path = "step_log_tests.rs"]
mod tests;
