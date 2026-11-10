//! Step-log formatting layer: chunk layout constants, the per-step emitter
//! ([`StepLogger`]), output masking, and every `==>` banner builder.
//!
//! This module owns the text contract the Job Detail terminal renders; the
//! wire semantics (`LogChunk` fields, ordering) live in `shared::protocol`.

use reqwest::Client;
use shared::protocol::{LogChunk, LogLevel};
use std::time::Duration;

use crate::config::Config;
use crate::masker::SecretCatalog;
use std::collections::HashMap;

use super::update_job_status;

/// Severity implied by a chunk-layout offset: verbose extras are Debug,
/// the cancel marker is Warn, errors are Error, everything else Info.
pub fn level_for_offset(offset: i32) -> LogLevel {
    match offset {
        LOG_STEP_ERROR => LogLevel::Error,
        LOG_STEP_CANCELLED | LOG_STEP_TRUNCATED | LOG_STEP_RETRY => LogLevel::Warn,
        LOG_STEP_VERBOSE_PREPARE
        | LOG_STEP_VERBOSE_CONFIG
        | LOG_STEP_VERBOSE_CONFIG_PREP
        | LOG_STEP_VERBOSE_EXEC
        | LOG_STEP_VERBOSE_RESULT => LogLevel::Debug,
        _ => LogLevel::Info,
    }
}

/// Chunk layout contract lives on `shared::protocol::LogChunk` — the
/// Manager and the terminal both read it from there.
pub fn step_log_base(step_idx: usize) -> i32 {
    (step_idx * 10_000) as i32
}

/// Job-start runner identity banner (`==> RUNNER`), ordered before step 0's
/// first chunk (`step_log_base(0) == 0`) so it appears at the very top of the
/// job's logs.
pub const LOG_RUNNER_BANNER: i32 = -2;

pub const LOG_STEP_VERBOSE_PREPARE: i32 = 0;
pub const LOG_STEP_VERBOSE_CONFIG: i32 = 1;
pub const LOG_STEP_VERBOSE_CONFIG_PREP: i32 = 2;
pub const LOG_LOGIN: i32 = 3;
pub const LOG_STEP_VERBOSE_EXEC: i32 = 4;
pub const LOG_ECHO: i32 = 5;
pub const LOG_STREAM_START: i32 = 6;
/// Last chunk offset streamed output may occupy. The stream starts at
/// `LOG_STREAM_START` (6) and must stop at or below this offset — 9991
/// carries the truncation marker, 9993+ are the semantic sentinels.
/// `job_logs` has `UNIQUE(job_id, chunk_index)` + `INSERT OR REPLACE` on the
/// Manager side: an unguarded stream would silently overwrite the sentinels
/// (and, past the next step's base, another step's chunks).
pub const LOG_STREAM_LAST: i32 = 9990;

/// `==> OUTPUT TRUNCATED` marker offset — uploaded when a step's streamed
/// output exhausts the window (see [`LOG_STREAM_LAST`]). Re-uploaded with the
/// final omitted count when the stream closes; `INSERT OR REPLACE` keeps the
/// last write.
pub const LOG_STEP_TRUNCATED: i32 = shared::protocol::LOG_STEP_TRUNCATED;

/// `==> RETRY` marker offset — one line per automatic retry; later attempts
/// replace it in place (the final view shows the last retry).
pub const LOG_STEP_RETRY: i32 = 9992;

pub const LOG_STEP_VERBOSE_RESULT: i32 = 9993;
pub const LOG_STEP_ERROR: i32 = 9997;
pub const LOG_STEP_CANCELLED: i32 = 9998;
pub const LOG_STEP_SUMMARY: i32 = 9999;

/// Streaming budget for one step's log window: hands out chunk indexes from
/// `LOG_STREAM_START` through `LOG_STREAM_LAST`, then flips to truncated —
/// further chunks are counted as omitted and the caller uploads the
/// [`LOG_STEP_TRUNCATED`] marker (immediately on the first overflow so the
/// live view notices, and again with the final count when the stream closes).
pub struct StreamBudget {
    next: i32,
    last: i32,
    truncated: bool,
    notified: bool,
    omitted: u64,
    /// Bounded tail of masked output past the window — the truncation marker
    /// carries these lines so an over-limit step still shows how it ended.
    tail: std::collections::VecDeque<String>,
}

/// Tail-summary bounds: the marker chunk carries at most this much of the
/// overflow (masked before buffering).
pub const STREAM_TAIL_LINES: usize = 200;
pub const STREAM_TAIL_LINE_MAX: usize = 2000;

impl StreamBudget {
    pub fn new(log_base: i32) -> Self {
        Self::new_at(log_base, log_base + LOG_STREAM_START)
    }

    /// Budget that resumes at `next` (a retry continues after the previous
    /// attempt's stream so the two runs do not overwrite each other in
    /// `job_logs`, where `(job_id, chunk_index)` is unique).
    pub fn new_at(log_base: i32, next: i32) -> Self {
        Self {
            next,
            last: log_base + LOG_STREAM_LAST,
            truncated: false,
            notified: false,
            omitted: 0,
            tail: std::collections::VecDeque::new(),
        }
    }

    /// Index the next chunk would take (callers persist it across retries).
    pub fn next_index(&self) -> i32 {
        self.next
    }

    /// Index for the next uploaded chunk, or `None` when the window is
    /// exhausted (the chunk is counted as omitted; keep draining output —
    /// stopping the read would block the child's pipe).
    pub fn take(&mut self) -> Option<i32> {
        if self.next <= self.last {
            let index = self.next;
            self.next += 1;
            return Some(index);
        }
        self.truncated = true;
        self.omitted += 1;
        None
    }

    /// Whether any chunk has been omitted.
    pub fn truncated(&self) -> bool {
        self.truncated
    }

    /// `Some(omitted)` when the stream overflowed the window, `None` otherwise
    /// — the caller records it on the step so summaries and failure messages
    /// can flag the truncation.
    pub fn omitted(&self) -> Option<u64> {
        self.truncated.then_some(self.omitted)
    }

    /// Marker content exactly once, on the first overflow after the previous
    /// call — `Some(marker)` lets the caller surface the notice while the
    /// step is still running.
    pub fn unnotified_truncation(&mut self) -> Option<String> {
        if self.truncated && !self.notified {
            self.notified = true;
            return Some(self.marker());
        }
        None
    }

    /// Buffer one masked chunk past the window into the tail summary.
    /// Keeps the LAST [`STREAM_TAIL_LINES`] lines, each clipped to
    /// [`STREAM_TAIL_LINE_MAX`] bytes on a char boundary.
    pub fn hold(&mut self, masked_chunk: &str) {
        for line in masked_chunk.lines() {
            let clipped = if line.len() > STREAM_TAIL_LINE_MAX {
                let mut cut = STREAM_TAIL_LINE_MAX;
                while cut > 0 && !line.is_char_boundary(cut) {
                    cut -= 1;
                }
                format!("{} …", &line[..cut])
            } else {
                line.to_string()
            };
            if self.tail.len() == STREAM_TAIL_LINES {
                self.tail.pop_front();
            }
            self.tail.push_back(clipped);
        }
    }

    /// Marker content: total omitted-chunk count plus the bounded tail
    /// (a chunk is one engine output line).
    pub fn marker(&self) -> String {
        let mut out = format!(
            "==> OUTPUT TRUNCATED\n  omitted  {} chunk(s) over the per-step log window\n",
            self.omitted
        );
        if !self.tail.is_empty() {
            out.push_str(&format!(
                "----- last {} line(s) of the stream -----\n",
                self.tail.len()
            ));
            for line in &self.tail {
                out.push_str(line);
                out.push('\n');
            }
        }
        out
    }
}

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
pub(crate) struct StepLogger {
    pub(super) client: Client,
    pub(super) config: Config,
    pub(super) token: String,
    pub(super) job_id: String,
    pub(super) log_base: i32,
    pub(super) step_order: i32,
}

impl StepLogger {
    pub fn new(client: &Client, config: &Config, token: &str, job_id: &str, idx: usize) -> Self {
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

    /// Emit at an absolute chunk index (streamed-output numbering —
    /// always Info severity).
    pub(super) async fn emit_at(
        &self,
        chunk_index: i32,
        content: impl Into<String>,
    ) -> anyhow::Result<()> {
        self.upload(chunk_index, LogLevel::Info, content).await
    }

    /// Emit at `log_base + offset`, propagating upload errors (for the few
    /// callers where a failed upload must fail the step). The severity comes
    /// from the layout offset so every site stays consistent by construction.
    pub(super) async fn try_emit(
        &self,
        offset: i32,
        content: impl Into<String>,
    ) -> anyhow::Result<()> {
        self.upload(self.log_base + offset, level_for_offset(offset), content)
            .await
    }

    async fn upload(
        &self,
        chunk_index: i32,
        level: LogLevel,
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
                level: Some(level),
            },
        )
        .await
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
    /// from `start` (normally `log_base + LOG_STREAM_START`). Spawned as the
    /// background upload task; upload failures are logged, never fatal.
    ///
    /// The window is bounded ([`LOG_STREAM_LAST`]) because `job_logs` is
    /// `UNIQUE(job_id, chunk_index)` + `INSERT OR REPLACE`: an unbounded stream
    /// silently overwrites the step's own sentinels (ERROR/CANCELLED/SUMMARY)
    /// and, past the next step's base, *that* step's chunks. Output past the
    /// window is drained and dropped (dropping it would block the child's
    /// pipe) and reported once as an `==> OUTPUT TRUNCATED` marker.
    ///
    /// Returns the index the next attempt should start from, so a retry
    /// appends after the failed attempt instead of overwriting it.
    pub(super) async fn stream_chunks(
        &self,
        output_rx: &mut tokio::sync::mpsc::UnboundedReceiver<String>,
        secret_values: &[String],
        forced_values: &[String],
        start: i32,
    ) -> StreamOutcome {
        let mut budget = StreamBudget::new_at(self.log_base, start);
        while let Some(chunk) = output_rx.recv().await {
            let masked = mask_output(&chunk, secret_values, forced_values);
            match budget.take() {
                Some(chunk_num) => {
                    if let Err(error) = self.emit_at(chunk_num, masked).await {
                        tracing::warn!("Log upload chunk {chunk_num} failed: {error}");
                    }
                }
                None => {
                    // Window exhausted: keep draining (see doc above), hold a
                    // bounded tail for the summary, and annotate once so the
                    // live terminal shows why output stops being appended.
                    budget.hold(&masked);
                    if let Some(marker) = budget.unnotified_truncation() {
                        self.emit(LOG_STEP_TRUNCATED, marker).await;
                    }
                }
            }
        }
        if budget.truncated() {
            // Final count — replaces the first-overflow marker in place.
            self.emit(LOG_STEP_TRUNCATED, budget.marker()).await;
        }
        StreamOutcome {
            next_index: budget.next_index(),
            omitted: budget.omitted(),
        }
    }
}

/// What one streamed step produced beyond the output itself.
#[derive(Debug, Clone, Copy)]
pub struct StreamOutcome {
    /// Index the next attempt's stream should start from (retries append).
    pub next_index: i32,
    /// `Some(count)` when the stream overflowed the log window.
    pub omitted: Option<u64>,
}

/// Truncation note appended to a step's summary chunk — the step-end record
/// must state that the output above is incomplete even if the live marker
/// scrolled away.
pub fn build_truncation_note(omitted: u64) -> String {
    format!(
        "==> OUTPUT TRUNCATED\n  omitted  {omitted} chunk(s) over the per-step log window — \
         the output above is incomplete\n"
    )
}

/// Extra failure hint when a step fails while its output was truncated.
pub const TRUNCATION_FAILURE_HINT: &str =
    "output was truncated (per-step log window) — the failing line may be missing";

/// `==> RETRY` marker: the step failed and will run again.
pub fn build_retry_marker(name: &str, attempt: u32, max_attempts: u32, detail: &str) -> String {
    let mut out = format!(
        "==> RETRY {attempt}/{}\n  step  {name}\n",
        max_attempts.saturating_sub(1)
    );
    if !detail.is_empty() {
        out.push_str(&format!("  detail  {detail}\n"));
    }
    out.push_str("  retrying\n");
    out
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
pub fn build_step_echo(
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

/// Identity banner for the runner that executed this job (version + OS),
/// uploaded once at job start so every job's logs are self-describing.
pub fn build_runner_banner() -> String {
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
pub fn build_ssh_banner(targets: &[(String, String)]) -> String {
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
pub fn format_resolved_ssh_targets(step: &serde_json::Value) -> Vec<String> {
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
/// Renders only keys/metadata, never secret values, so it is safe for every
/// user.
///
/// Uploaded **before** config prepare so a prepare-stage failure still shows
/// the step shape — previously STEP CONFIG only ran after a successful
/// prepare, so Verbose looked identical to a normal run.
pub fn build_verbose_step_config(step: &serde_json::Value, exec_type: &str) -> String {
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

/// Verbose-mode step result summary (duration + exit code).
pub fn build_verbose_step_result(duration: Duration, exit_code: i32) -> String {
    let dur = if duration.as_secs_f64() >= 1.0 {
        format!("{:.2}s", duration.as_secs_f64())
    } else {
        format!("{}ms", duration.as_millis())
    };
    format!("==> STEP RESULT\n  duration   {dur}\n  exit code  {exit_code}\n")
}

/// Human hint when a step exits non-zero (avoid always blaming interactive input).
pub fn step_failure_hint(stderr: &str, exit_code: i32) -> String {
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
        return "registry pull denied — check the image name/permissions; private images \
                require credentials already configured on the runner host."
            .into();
    }
    if s.contains("docker login") && s.contains("failed") {
        return "docker login failed — check the registry host and credentials used by the command."
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
pub fn build_step_failure_message(
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
