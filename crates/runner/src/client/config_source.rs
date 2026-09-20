//! Config prepare for one step: OSS deliver (+ optional render sugar) and
//! render-only (templates already at `config_dest`).
//!
//! Sequencing (design: docs/content/docs/01-architecture/config-block.md):
//!
//! - **OSS deliver:** `download → unpack → [render] → run block`
//! - **render-only (shell/local):** read templates under `dest` in place → write
//! - **render-only (ssh):** fetch templates (+ old outputs) → render on Runner
//!   (secrets never leave) → upload outputs
//!
//! The rendered `.env` values are returned for forced value redaction: any
//! plaintext `.env` content echoed by the run block must stay masked even for
//! secrets shorter than 3 characters.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context};
use reqwest::Client;

use crate::engine::ssh::{SshOutput, SshSession};
use shared::value_source::{self, ExpandMode, MissingPolicy, ValueMap};

use super::env_render;

/// Timeout for the signed OSS config bundle download.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(300);
/// Default mode for the rendered output file (`0600`).
const DEFAULT_ENV_MODE: u32 = 0o600;
/// Default number of timestamped `.env` backups to retain when overwriting.
const DEFAULT_BACKUP_KEEP: usize = 3;

/// One progress line emitted while preparing a config bundle.
///
/// `verbose_only` lines (e.g. unpacked file listings, changed-key detail) are
/// only shown in Verbose mode; everything else is always visible.
#[derive(Debug, Clone)]
pub struct PrepLogLine {
    pub verbose_only: bool,
    pub text: String,
}

impl PrepLogLine {
    pub fn info(text: impl Into<String>) -> Self {
        Self {
            verbose_only: false,
            text: text.into(),
        }
    }

    pub fn verbose(text: impl Into<String>) -> Self {
        Self {
            verbose_only: true,
            text: text.into(),
        }
    }
}

/// Result of preparing the config bundle for one step.
#[derive(Debug)]
pub struct ConfigPrep {
    /// Env overrides for the step env (`BUZ_CONFIG_DIR`, `BUZ_ENV_CHANGED`, …).
    pub env: HashMap<String, String>,
    /// Secret values written to the rendered `.env` — force-redact in logs.
    pub forced_redact: Vec<String>,
    /// Progress lines to surface under the `==> CONFIG PREP` banner (download
    /// result, unpack listing, per-file render, readiness). Never carries
    /// secrets or the signed URL — only status, sizes and file/template names.
    pub logs: Vec<PrepLogLine>,
}

#[derive(Debug, Clone)]
struct EnvRenderSpec {
    template: String,
    output: String,
    mode: u32,
    on_missing: MissingPolicy,
    /// Number of timestamped `.bak` backups to keep (0 disables backups).
    backup_keep: usize,
}

/// True when the step needs prepare (OSS deliver and/or `config_render`).
pub fn needs_prepare(step: &serde_json::Value) -> bool {
    has_config_auth(step) || has_config_render(step)
}

fn has_config_auth(step: &serde_json::Value) -> bool {
    step.get("config_auth")
        .and_then(|a| a.get("url"))
        .and_then(|v| v.as_str())
        .is_some_and(|s| !s.is_empty())
}

fn has_config_render(step: &serde_json::Value) -> bool {
    match config_render_specs(step) {
        Ok(specs) => !specs.is_empty(),
        Err(_) => {
            // Malformed render still needs prepare so errors surface in-step.
            step.get("config_render")
                .is_some_and(|v| !v.is_null() && v != &serde_json::Value::Null)
        }
    }
}

/// Extract the signed OSS GET URL from the dispatched step.
fn config_url(step: &serde_json::Value) -> anyhow::Result<String> {
    step.get("config_auth")
        .and_then(|a| a.get("url"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            anyhow::anyhow!("config: Manager did not attach a signed OSS URL for this step")
        })
}

/// Human label for the source archive in error messages (never the signed URL).
fn archive_label(step: &serde_json::Value) -> String {
    step.get("config_auth")
        .and_then(|a| a.get("object_key"))
        .and_then(|v| v.as_str())
        .unwrap_or("configs.tar.gz")
        .to_string()
}

fn parse_one_render_entry(
    er: &serde_json::Value,
    idx: Option<usize>,
) -> Result<EnvRenderSpec, String> {
    let label = idx
        .map(|i| format!("config_render[{i}]"))
        .unwrap_or_else(|| "config_render".into());
    let template = er
        .get("template")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    if template.is_empty() {
        return Err(format!(
            "{label}.template is required (relative to $BUZ_CONFIG_DIR)"
        ));
    }
    let output = er
        .get("output")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("{template}.rendered"));
    let mode = match er
        .get("mode")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
    {
        Some(m) => u32::from_str_radix(m.trim(), 8)
            .map_err(|_| format!("{label}.mode '{m}' is not a valid octal mode"))?,
        None => DEFAULT_ENV_MODE,
    };
    let on_missing = match er
        .get("on_missing")
        .and_then(|v| v.as_str())
        .unwrap_or("fail")
    {
        "warn" => MissingPolicy::Warn,
        _ => MissingPolicy::Fail,
    };
    let backup_keep = er
        .get("backup_keep")
        .and_then(|v| v.as_u64())
        .map(|n| n as usize)
        .unwrap_or(DEFAULT_BACKUP_KEEP);
    Ok(EnvRenderSpec {
        template: template.to_string(),
        output,
        mode,
        on_missing,
        backup_keep,
    })
}

/// Parse `config_render` (JSON array or single object) into specs.
fn config_render_specs(step: &serde_json::Value) -> Result<Vec<EnvRenderSpec>, String> {
    let er = match step.get("config_render") {
        Some(v) if !v.is_null() => v,
        _ => return Ok(vec![]),
    };
    if let Some(arr) = er.as_array() {
        if arr.is_empty() {
            return Ok(vec![]);
        }
        return arr
            .iter()
            .enumerate()
            .map(|(i, e)| parse_one_render_entry(e, Some(i)))
            .collect();
    }
    if er.is_object() {
        return Ok(vec![parse_one_render_entry(er, None)?]);
    }
    Err("config_render must be an object or array of objects".into())
}

/// Local unpack / render dir: absolute `config_dest` as-is, relative joined to
/// the job workspace, unset → a per-step dir inside the workspace.
fn resolve_local_dir(step: &serde_json::Value, local_root: &Path, step_idx: usize) -> PathBuf {
    match step
        .get("config_dest")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(d) if d.starts_with('/') => PathBuf::from(d),
        Some(d) => local_root.join(d),
        None => local_root.join(format!("config-{step_idx}")),
    }
}

/// Remote (SSH) dest: absolute `config_dest` as-is, relative or unset → a
/// deterministic per-job temp dir on the target.
fn resolve_remote_dir(step: &serde_json::Value, job_id: &str, step_idx: usize) -> String {
    match step
        .get("config_dest")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(d) if d.starts_with('/') => d.to_string(),
        Some(d) => format!("/tmp/buz-config/{d}"),
        None => format!("/tmp/buz-config/{job_id}-{step_idx}"),
    }
}

/// Extract a single XML tag's text content (OSS error bodies use `<Code>` /
/// `<Message>`). Returns `None` when the tag is absent.
fn xml_tag_text(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find(&close)? + start;
    Some(xml[start..end].trim().to_string())
}

/// Build a human-readable reason from an OSS error body, falling back to a
/// trimmed snippet when the body isn't the expected XML shape. Never exposes
/// the signed URL — only the `<Code>` / `<Message>` OSS reports.
fn oss_error_detail(body: &str) -> String {
    match (xml_tag_text(body, "Code"), xml_tag_text(body, "Message")) {
        (Some(code), Some(msg)) => format!("{code}: {msg}"),
        (Some(code), None) => code,
        _ => {
            let trimmed = body.trim();
            if trimmed.is_empty() {
                "(no response body)".to_string()
            } else {
                let mut s: String = trimmed.chars().take(240).collect();
                if trimmed.chars().count() > 240 {
                    s.push('…');
                }
                s
            }
        }
    }
}

/// Failure message for a non-2xx config bundle download.
///
/// OSS answers an expired signature with `403 AccessDenied: Request has
/// expired.`, which is indistinguishable from a clock problem or from a task
/// that waited too long. The response `Date` header (present on `403` too) is
/// therefore reported next to the local time so the job log itself shows which
/// side is off:
///
/// - clocks aligned → the URL was already expired when it was used (Manager
///   clock lagged at signing time, or the task sat too long before execution);
/// - local behind/ahead of OSS → this host needs NTP.
pub(crate) fn describe_download_failure(
    status: reqwest::StatusCode,
    date_header: Option<&str>,
    sent_at: chrono::DateTime<chrono::Utc>,
    received_at: chrono::DateTime<chrono::Utc>,
    detail: &str,
) -> String {
    let mut msg = format!("config bundle download returned HTTP {status}: {detail}");
    let Some(remote) = date_header.and_then(|raw| shared::clock::parse_http_date(raw).ok()) else {
        return msg;
    };
    let skew = shared::clock::skew_from_http_date(sent_at, received_at, remote);
    let verdict = if skew.abs() < 60 {
        "clocks aligned — the signed URL was already expired when used; \
         check the CONFIG PREP 'signed_at' / 'url_remaining' fields"
    } else if skew > 0 {
        "this host is behind OSS — sync NTP on the runner host"
    } else {
        "this host is ahead of OSS — sync NTP on the runner host"
    };
    msg.push_str(&format!(
        "\n  oss_time     {}\n  local_time   {}\n  clock_skew   {} ({verdict})",
        shared::clock::format_utc(remote.timestamp()),
        shared::clock::format_utc(received_at.timestamp()),
        shared::clock::describe_skew(skew),
    ));
    msg
}

/// Always-visible staging line describing the signed URL's absolute window.
///
/// Emitted whether or not verbose logging is on: a URL that arrives already
/// expired (Manager clock lagged at signing, or the task waited too long) must
/// be obvious in the job log instead of only resurfacing as an opaque OSS 403.
fn push_url_window_log(logs: &mut Vec<PrepLogLine>, step: &serde_json::Value) {
    let Some(age) = super::poll::config_url_age(step, chrono::Utc::now().timestamp()) else {
        return;
    };
    let mut text = format!(
        "  url_window      signed {}s ago, {}s TTL, {} left",
        age.age_secs.max(0),
        age.ttl_secs(),
        age.remaining_label()
    );
    if age.is_expired() {
        tracing::warn!(
            signed_at = age.signed_at_epoch,
            age_secs = age.age_secs,
            ttl_secs = age.ttl_secs(),
            "config-source: signed bundle URL arrived already expired"
        );
        text.push_str(
            " — EXPIRED before download (Manager clock lagged at signing time, or the task waited too long)",
        );
    }
    logs.push(PrepLogLine::info(text));
}

/// Download the signed OSS bundle, returning `(bytes, http_status, byte_len)`.
async fn download(client: &Client, url: &str) -> anyhow::Result<(Vec<u8>, u16, usize)> {
    let sent_at = chrono::Utc::now();
    let resp = client
        .get(url)
        .timeout(DOWNLOAD_TIMEOUT)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("config bundle download failed: {e}"))?;
    let received_at = chrono::Utc::now();
    if !resp.status().is_success() {
        let status = resp.status();
        let date_header = resp
            .headers()
            .get(reqwest::header::DATE)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let body = resp.text().await.unwrap_or_default();
        bail!(
            "{}",
            describe_download_failure(
                status,
                date_header.as_deref(),
                sent_at,
                received_at,
                &oss_error_detail(&body),
            )
        );
    }
    let status = resp.status().as_u16();
    let bytes = resp.bytes().await?.to_vec();
    let len = bytes.len();
    Ok((bytes, status, len))
}

/// Format a byte count as a human-readable size (e.g. `12.3 KiB`).
fn format_bytes(n: usize) -> String {
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let mut value = n as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{n} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// Recursively list regular files under `root` as root-relative paths, sorted.
fn list_files(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut items: Vec<_> = entries.flatten().collect();
        items.sort_by_key(|e| e.path());
        for entry in items {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if let Ok(rel) = path.strip_prefix(root) {
                out.push(rel.to_string_lossy().into_owned());
            }
        }
    }
    out
}

/// Extract a tar.gz byte blob into `dest` via the system `tar`, returning the
/// unpacked file list (root-relative, for the Verbose listing).
fn unpack(tarball: &[u8], dest: &Path) -> anyhow::Result<Vec<String>> {
    std::fs::create_dir_all(dest)
        .with_context(|| format!("cannot create config dir '{}'", dest.display()))?;
    let mut child = std::process::Command::new("tar")
        .args(["-xzf", "-"])
        .arg("-C")
        .arg(dest)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .context("cannot spawn 'tar' for config bundle extraction")?;
    {
        let stdin = child
            .stdin
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("config bundle: no tar stdin"))?;
        stdin.write_all(tarball)?;
    }
    let output = child.wait_with_output()?;
    if !output.status.success() {
        bail!(
            "config bundle extraction failed (tar): {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(list_files(dest))
}

/// Render the template against the job's `ValueMap`, merge with the existing
/// output content, and run the compose round-trip guard.
fn render_env_with_old(
    config_dir: &Path,
    spec: &EnvRenderSpec,
    values: &ValueMap,
    archive: &str,
    old: &str,
) -> anyhow::Result<(String, Vec<String>, Vec<String>)> {
    let template_path = config_dir.join(&spec.template);
    let template = std::fs::read_to_string(&template_path).with_context(|| {
        format!(
            "config_render: template '{}' not found under '{}' (source '{}')",
            spec.template,
            config_dir.display(),
            archive
        )
    })?;
    let expanded = value_source::expand(&template, values, ExpandMode::Strict, spec.on_missing)
        .map_err(|e| anyhow::anyhow!("config_render: {e}"))?;
    // Full rebuild: the template is the sole source of truth. Rendering against
    // an empty `old` drops operator-added keys that are no longer in the
    // template (stale keys would otherwise linger and cause subtle bugs). The
    // pre-overwrite backup preserves the prior content for rollback.
    let (content, _) = env_render::merge_env("", &expanded.content);
    env_render::round_trip_check(&content).map_err(|e| anyhow::anyhow!("config_render: {e}"))?;
    // Diff against the real old content so `BUZ_ENV_CHANGED` stays precise.
    let changed = env_render::diff_env(old, &content);
    // Forced value registration: every secret dispatched for this job is
    // force-redacted so the plaintext `.env` never echoes unredacted.
    let forced_redact: Vec<String> = values.secrets.values().cloned().collect();
    Ok((content, changed, forced_redact))
}

/// Back up the existing output file to `<output>.bak.<timestamp>` before it is
/// overwritten, then prune so only the most recent `keep` backups remain.
/// `keep == 0` disables backups entirely (caller should skip this fn).
fn backup_output_local(path: &Path, old: &str, keep: usize) -> anyhow::Result<()> {
    let file_name = path.file_name().and_then(|s| s.to_str()).unwrap_or("env");
    let ts = chrono::Utc::now().format("%Y%m%d-%H%M%S");
    let backup = path.with_file_name(format!("{file_name}.bak.{ts}"));
    std::fs::write(&backup, old)
        .with_context(|| format!("config_render: cannot write backup '{}'", backup.display()))?;
    prune_backups_local(path, keep)
}

/// Remove the oldest `<output>.bak.*` files beyond `keep`, keeping the newest.
fn prune_backups_local(path: &Path, keep: usize) -> anyhow::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let base = path.file_name().and_then(|s| s.to_str()).unwrap_or("env");
    let prefix = format!("{base}.bak.");
    let mut backups: Vec<PathBuf> = std::fs::read_dir(dir)
        .with_context(|| format!("config_render: cannot list '{}'", dir.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|s| s.to_str())
                .is_some_and(|s| s.starts_with(&prefix))
        })
        .collect();
    backups.sort();
    let excess = backups.len().saturating_sub(keep);
    for b in backups.into_iter().take(excess) {
        let _ = std::fs::remove_file(b);
    }
    Ok(())
}

/// Shell snippet to back up the remote output file in place, keeping only the
/// newest `keep` timestamped backups. `keep == 0` disables backups (caller
/// should skip). Assumes `remote_out` is a single path (no glob chars).
fn backup_output_remote_cmd(remote_out: &str, keep: usize) -> String {
    format!(
        "if [ -f \"{remote_out}\" ]; then \
           ts=$(date +%Y%m%d-%H%M%S); \
           cp -a \"{remote_out}\" \"{remote_out}.bak.$ts\"; \
           ls -1 \"{remote_out}.bak.\"* 2>/dev/null | sort | head -n -{keep} | xargs -r rm -f; \
         fi"
    )
}

/// Append per-render progress lines: an always-visible `template → output`
/// line, plus a Verbose `changed` line when any managed key actually changed.
fn push_render_logs(logs: &mut Vec<PrepLogLine>, template: &str, output: &str, changed: &[String]) {
    logs.push(PrepLogLine::info(format!(
        "  render          {template} → {output}"
    )));
    if !changed.is_empty() {
        logs.push(PrepLogLine::verbose(format!(
            "  changed         {}",
            changed.join(", ")
        )));
    }
}

/// Append the unpacked file listing as Verbose-only progress lines.
fn push_files_logs(logs: &mut Vec<PrepLogLine>, files: &[String]) {
    if files.is_empty() {
        logs.push(PrepLogLine::verbose(
            "  files           (empty bundle)".to_string(),
        ));
        return;
    }
    for file in files {
        logs.push(PrepLogLine::verbose(format!("  files           {file}")));
    }
}

/// Apply all render specs under `config_dir`, returning union of changed keys
/// and forced-redact values from the last render (secret set is job-wide).
fn apply_renders_local(
    config_dir: &Path,
    specs: &[EnvRenderSpec],
    values: &ValueMap,
    archive: &str,
    logs: &mut Vec<PrepLogLine>,
) -> anyhow::Result<(Vec<String>, Vec<String>)> {
    let mut changed_all: Vec<String> = Vec::new();
    let mut forced_redact = Vec::new();
    for spec in specs {
        let output_path = config_dir.join(&spec.output);
        if let Some(parent) = output_path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!(
                    "config_render: cannot create output dir '{}'",
                    parent.display()
                )
            })?;
        }
        let old = std::fs::read_to_string(&output_path).unwrap_or_default();
        let (content, changed, forced) =
            render_env_with_old(config_dir, spec, values, archive, &old)?;
        if spec.backup_keep > 0 && output_path.is_file() {
            backup_output_local(&output_path, &old, spec.backup_keep)?;
        }
        write_atomic(&output_path, &content, spec.mode)?;
        push_render_logs(logs, &spec.template, &spec.output, &changed);
        forced_redact = forced;
        for k in changed {
            if !changed_all.contains(&k) {
                changed_all.push(k);
            }
        }
    }
    Ok((changed_all, forced_redact))
}

/// `flock(LOCK_EX)` on a lock file so concurrent renders of the same output
/// cannot interleave their read-merge-write critical section.
fn flock_lock(file: &std::fs::File) -> anyhow::Result<()> {
    let rc = unsafe { libc::flock(std::os::unix::io::AsRawFd::as_raw_fd(file), libc::LOCK_EX) };
    if rc == 0 {
        Ok(())
    } else {
        bail!("flock failed: {}", std::io::Error::last_os_error())
    }
}

fn flock_unlock(file: &std::fs::File) {
    unsafe {
        libc::flock(std::os::unix::io::AsRawFd::as_raw_fd(file), libc::LOCK_UN);
    }
}

/// Atomic replace: write to a temp file in the same directory, fsync, then
/// rename over the target, all under `flock` on a sibling lock file.
fn write_atomic(path: &Path, content: &str, mode: u32) -> anyhow::Result<()> {
    let lock_path = PathBuf::from(format!("{}.lock", path.display()));
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .read(true)
        .write(true)
        .open(&lock_path)
        .with_context(|| {
            format!(
                "config_render: cannot open lock file '{}'",
                lock_path.display()
            )
        })?;
    flock_lock(&lock)?;

    let file_name = path.file_name().and_then(|s| s.to_str()).unwrap_or("env");
    let tmp = path.with_file_name(format!(
        ".{file_name}.buz-{}",
        uuid::Uuid::new_v4().simple()
    ));
    let result = (|| -> anyhow::Result<()> {
        let mut f = std::fs::File::create(&tmp).with_context(|| {
            format!("config_render: cannot create temp file '{}'", tmp.display())
        })?;
        f.write_all(content.as_bytes())?;
        f.sync_all()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = f.metadata()?.permissions();
            perms.set_mode(mode);
            std::fs::set_permissions(&tmp, perms)?;
        }
        std::fs::rename(&tmp, path).with_context(|| {
            format!(
                "config_render: cannot atomically replace '{}'",
                path.display()
            )
        })?;
        Ok(())
    })();

    flock_unlock(&lock);
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// Set `BUZ_ENV_CHANGED` / `BUZ_ENV_CHANGED_KEYS` for the run block.
fn set_changed_env(env: &mut HashMap<String, String>, changed: &[String]) {
    if changed.is_empty() {
        env.insert("BUZ_ENV_CHANGED".to_string(), "0".to_string());
    } else {
        env.insert("BUZ_ENV_CHANGED".to_string(), "1".to_string());
        env.insert("BUZ_ENV_CHANGED_KEYS".to_string(), changed.join(","));
    }
}

/// Run one remote command on an open session, failing on a non-zero exit.
async fn run_ssh(
    session: &mut SshSession,
    cmd: &str,
    step_timeout: Option<Duration>,
) -> anyhow::Result<()> {
    let out = session.exec(cmd, None, step_timeout).await?;
    if out.exit_code != 0 {
        bail!(
            "remote command failed (exit {}): {}",
            out.exit_code,
            out.stderr.trim()
        );
    }
    Ok(())
}

/// Read a file on the target if it exists and is non-empty.
async fn fetch_remote_text(
    session: &mut SshSession,
    remote_path: &str,
    step_timeout: Option<Duration>,
) -> anyhow::Result<Option<String>> {
    let out = session
        .exec(
            &format!("cat {remote_path} 2>/dev/null || true"),
            None,
            step_timeout,
        )
        .await?;
    if out.exit_code != 0 || out.stdout.is_empty() {
        return Ok(None);
    }
    Ok(Some(out.stdout))
}

/// Format a failed remote command for logs (stderr preferred, else stdout).
fn ssh_fail_detail(out: &SshOutput) -> String {
    let err = out.stderr.trim();
    if !err.is_empty() {
        return format!("exit {}: {err}", out.exit_code);
    }
    let so = out.stdout.trim();
    if !so.is_empty() {
        return format!("exit {}: {so}", out.exit_code);
    }
    format!("exit {} (no remote stderr)", out.exit_code)
}

/// Transfer `bytes` to `remote_path` via the open session (`cat > tmp` over stdin),
/// then an atomic `mv` (and optional `chmod`) into place.
async fn ssh_upload_bytes(
    session: &mut SshSession,
    remote_path: &str,
    bytes: &[u8],
    mode: Option<u32>,
    step_timeout: Option<Duration>,
) -> anyhow::Result<()> {
    let part = format!("/tmp/.buz_upload_{}", uuid::Uuid::new_v4().simple());
    let out = session
        .exec(&format!("cat > {part}"), Some(bytes), step_timeout)
        .await?;
    if out.exit_code != 0 {
        bail!(
            "config upload to {remote_path} failed: {}",
            ssh_fail_detail(&out)
        );
    }
    let mv = match mode {
        Some(m) => format!("mv -f {part} {remote_path} && chmod {m:o} {remote_path}"),
        None => format!("mv -f {part} {remote_path}"),
    };
    let out = session.exec(&mv, None, step_timeout).await?;
    if out.exit_code != 0 {
        bail!(
            "config upload finalize to {remote_path} failed: {}",
            ssh_fail_detail(&out)
        );
    }
    Ok(())
}

async fn ensure_remote_parent(
    session: &mut SshSession,
    remote_dir: &str,
    relative: &str,
    step_timeout: Option<Duration>,
) -> anyhow::Result<()> {
    if let Some(parent) = Path::new(relative).parent() {
        let p = parent.to_string_lossy();
        if !p.is_empty() && p != "." {
            run_ssh(session, &format!("mkdir -p {remote_dir}/{p}"), step_timeout).await?;
        }
    }
    Ok(())
}

/// Prepare on the runner itself (shell / local engines).
pub async fn prepare_local(
    client: &Client,
    step: &serde_json::Value,
    local_root: &Path,
    step_idx: usize,
    values: &ValueMap,
) -> anyhow::Result<ConfigPrep> {
    let specs = config_render_specs(step).map_err(anyhow::Error::msg)?;
    let local_dir = resolve_local_dir(step, local_root, step_idx);
    let mut logs: Vec<PrepLogLine> = Vec::new();

    let archive = if has_config_auth(step) {
        let url = config_url(step)?;
        let label = archive_label(step);
        push_url_window_log(&mut logs, step);
        let (tarball, status, len) = download(client, &url).await?;
        logs.push(PrepLogLine::info(format!(
            "  download        HTTP {status} ({})",
            format_bytes(len)
        )));
        let files = unpack(&tarball, &local_dir)?;
        push_files_logs(&mut logs, &files);
        label
    } else if !specs.is_empty() {
        if !local_dir.is_dir() {
            bail!(
                "config_render: dest '{}' does not exist — deliver config first \
                 (OSS unpack) or create the directory with templates",
                local_dir.display()
            );
        }
        "render-only".to_string()
    } else {
        bail!("config prepare: step has neither config_auth nor config_render");
    };

    let mut env = HashMap::new();
    let mut forced_redact = Vec::new();
    if !specs.is_empty() {
        let (changed_all, forced) =
            apply_renders_local(&local_dir, &specs, values, &archive, &mut logs)?;
        forced_redact = forced;
        set_changed_env(&mut env, &changed_all);
    }
    env.insert(
        "BUZ_CONFIG_DIR".to_string(),
        local_dir.to_string_lossy().into_owned(),
    );
    logs.push(PrepLogLine::info("==> CONFIG READY".to_string()));
    Ok(ConfigPrep {
        env,
        forced_redact,
        logs,
    })
}

/// Prepare for SSH: OSS deliver uploads the bundle; render-only fetches
/// templates from the remote, renders on the Runner, uploads outputs.
///
/// Uses the caller's open [`SshSession`]s (one per host) so prepare shares the
/// same TCP connection as the subsequent `run` block. Multi-host prepare runs
/// **in parallel**; dropping this future (job cancel) aborts all hosts.
#[allow(clippy::too_many_arguments)]
pub async fn prepare_ssh(
    client: &Client,
    step: &serde_json::Value,
    sessions: &mut Vec<SshSession>,
    local_root: &Path,
    job_id: &str,
    step_idx: usize,
    values: &ValueMap,
    step_timeout: Option<Duration>,
    parallel: bool,
) -> anyhow::Result<ConfigPrep> {
    let specs = config_render_specs(step).map_err(anyhow::Error::msg)?;
    let remote_dir = resolve_remote_dir(step, job_id, step_idx);

    if has_config_auth(step) {
        return prepare_ssh_deliver(
            client,
            step,
            sessions,
            local_root,
            job_id,
            step_idx,
            values,
            step_timeout,
            &specs,
            &remote_dir,
            parallel,
        )
        .await;
    }

    if specs.is_empty() {
        bail!("config prepare: step has neither config_auth nor config_render");
    }

    prepare_ssh_render_only(
        step,
        sessions,
        local_root,
        job_id,
        step_idx,
        values,
        step_timeout,
        &specs,
        &remote_dir,
        parallel,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn prepare_ssh_deliver(
    client: &Client,
    step: &serde_json::Value,
    sessions: &mut Vec<SshSession>,
    local_root: &Path,
    job_id: &str,
    step_idx: usize,
    values: &ValueMap,
    step_timeout: Option<Duration>,
    specs: &[EnvRenderSpec],
    remote_dir: &str,
    parallel: bool,
) -> anyhow::Result<ConfigPrep> {
    let url = config_url(step)?;
    let archive = archive_label(step);
    let mut logs: Vec<PrepLogLine> = Vec::new();
    push_url_window_log(&mut logs, step);
    let (tarball_bytes, status, len) = download(client, &url).await?;
    logs.push(PrepLogLine::info(format!(
        "  download        HTTP {status} ({})",
        format_bytes(len)
    )));
    let tarball = std::sync::Arc::new(tarball_bytes);
    let local_dir = local_root.join(format!("config-{job_id}-{step_idx}"));
    let files = unpack(tarball.as_ref(), &local_dir)?;
    push_files_logs(&mut logs, &files);
    let tar_remote = format!("/tmp/buz_config_{job_id}_{step_idx}.tar.gz");
    let remote_dir = remote_dir.to_string();
    let specs = specs.to_vec();
    let values = values.clone();
    let archive = archive.clone();
    let multi_host = sessions.len() > 1;

    let owned = std::mem::take(sessions);
    let results = if parallel && owned.len() > 1 {
        let futs = owned.into_iter().map(|mut session| {
            let tarball = Arc::clone(&tarball);
            let tar_remote = tar_remote.clone();
            let remote_dir = remote_dir.clone();
            let local_dir = local_dir.clone();
            let specs = specs.clone();
            let values = values.clone();
            let archive = archive.clone();
            async move {
                let host = session.host_label().to_string();
                let result = prepare_one_host_deliver(
                    &mut session,
                    &tarball,
                    &tar_remote,
                    &remote_dir,
                    &local_dir,
                    &specs,
                    &values,
                    &archive,
                    step_timeout,
                )
                .await;
                (session, host, result)
            }
        });
        futures_util::future::join_all(futs).await
    } else {
        let mut results = Vec::with_capacity(owned.len());
        for mut session in owned {
            let host = session.host_label().to_string();
            let result = prepare_one_host_deliver(
                &mut session,
                tarball.as_ref(),
                &tar_remote,
                &remote_dir,
                &local_dir,
                &specs,
                &values,
                &archive,
                step_timeout,
            )
            .await;
            results.push((session, host, result));
        }
        results
    };

    let mut forced_redact = Vec::new();
    let mut changed_agg: Vec<String> = Vec::new();
    let mut first_err: Option<anyhow::Error> = None;
    for (session, host, result) in results {
        sessions.push(session);
        match result {
            Ok((changed, forced, host_logs)) => {
                forced_redact = forced;
                if multi_host {
                    logs.push(PrepLogLine::info(format!("  host            {host}")));
                }
                logs.extend(host_logs);
                for k in changed {
                    if !changed_agg.contains(&k) {
                        changed_agg.push(k);
                    }
                }
            }
            Err(e) => {
                let err = e.context(format!("config prepare on {host} failed"));
                if first_err.is_none() {
                    first_err = Some(err);
                }
            }
        }
    }
    if let Some(err) = first_err {
        return Err(err);
    }

    let mut env = HashMap::new();
    if !specs.is_empty() {
        set_changed_env(&mut env, &changed_agg);
    }
    env.insert("BUZ_CONFIG_DIR".to_string(), remote_dir);
    logs.push(PrepLogLine::info("==> CONFIG READY".to_string()));
    Ok(ConfigPrep {
        env,
        forced_redact,
        logs,
    })
}

#[allow(clippy::too_many_arguments)]
async fn prepare_one_host_deliver(
    session: &mut SshSession,
    tarball: &[u8],
    tar_remote: &str,
    remote_dir: &str,
    local_dir: &Path,
    specs: &[EnvRenderSpec],
    values: &ValueMap,
    archive: &str,
    step_timeout: Option<Duration>,
) -> anyhow::Result<(Vec<String>, Vec<String>, Vec<PrepLogLine>)> {
    ssh_upload_bytes(session, tar_remote, tarball, None, step_timeout).await?;
    let unpack_cmd = format!(
        "mkdir -p {remote_dir} && tar xzf {tar_remote} -C {remote_dir} && rm -f {tar_remote}"
    );
    run_ssh(session, &unpack_cmd, step_timeout).await?;
    let mut host_changed: Vec<String> = Vec::new();
    let mut forced_redact = Vec::new();
    let mut host_logs: Vec<PrepLogLine> = Vec::new();
    for spec in specs {
        ensure_remote_parent(session, remote_dir, &spec.output, step_timeout).await?;
        let remote_out = format!("{remote_dir}/{}", spec.output);
        let old = fetch_remote_text(session, &remote_out, step_timeout).await?;
        let (content, changed, forced) = render_env_with_old(
            local_dir,
            spec,
            values,
            archive,
            old.as_deref().unwrap_or(""),
        )?;
        if spec.backup_keep > 0 && old.is_some() {
            run_ssh(
                session,
                &backup_output_remote_cmd(&remote_out, spec.backup_keep),
                step_timeout,
            )
            .await?;
        }
        ssh_upload_bytes(
            session,
            &remote_out,
            content.as_bytes(),
            Some(spec.mode),
            step_timeout,
        )
        .await?;
        push_render_logs(&mut host_logs, &spec.template, &spec.output, &changed);
        forced_redact = forced;
        for k in changed {
            if !host_changed.contains(&k) {
                host_changed.push(k);
            }
        }
    }
    Ok((host_changed, forced_redact, host_logs))
}

#[allow(clippy::too_many_arguments)]
async fn prepare_ssh_render_only(
    _step: &serde_json::Value,
    sessions: &mut Vec<SshSession>,
    local_root: &Path,
    job_id: &str,
    step_idx: usize,
    values: &ValueMap,
    step_timeout: Option<Duration>,
    specs: &[EnvRenderSpec],
    remote_dir: &str,
    parallel: bool,
) -> anyhow::Result<ConfigPrep> {
    let staging = local_root.join(format!("config-render-{job_id}-{step_idx}"));
    std::fs::create_dir_all(&staging).with_context(|| {
        format!(
            "config_render: cannot create staging dir '{}'",
            staging.display()
        )
    })?;

    let remote_dir = remote_dir.to_string();
    let specs = specs.to_vec();
    let values = values.clone();
    let multi_host = sessions.len() > 1;
    let mut logs: Vec<PrepLogLine> = Vec::new();
    // Per-host staging dirs avoid parallel writes colliding on the same template path.
    let owned = std::mem::take(sessions);
    let results = if parallel && owned.len() > 1 {
        let futs = owned.into_iter().enumerate().map(|(host_i, mut session)| {
            let remote_dir = remote_dir.clone();
            let specs = specs.clone();
            let values = values.clone();
            let staging = staging.join(format!("host-{host_i}"));
            async move {
                let host = session.host_label().to_string();
                let result = prepare_one_host_render_only(
                    &mut session,
                    &staging,
                    &remote_dir,
                    &specs,
                    &values,
                    step_timeout,
                )
                .await;
                (session, host, result)
            }
        });
        futures_util::future::join_all(futs).await
    } else {
        let mut results = Vec::with_capacity(owned.len());
        for (host_i, mut session) in owned.into_iter().enumerate() {
            let host = session.host_label().to_string();
            let host_staging = staging.join(format!("host-{host_i}"));
            let result = prepare_one_host_render_only(
                &mut session,
                &host_staging,
                &remote_dir,
                &specs,
                &values,
                step_timeout,
            )
            .await;
            results.push((session, host, result));
        }
        results
    };

    let mut forced_redact = Vec::new();
    let mut changed_agg: Vec<String> = Vec::new();
    let mut first_err: Option<anyhow::Error> = None;
    for (session, host, result) in results {
        sessions.push(session);
        match result {
            Ok((changed, forced, host_logs)) => {
                forced_redact = forced;
                if multi_host {
                    logs.push(PrepLogLine::info(format!("  host            {host}")));
                }
                logs.extend(host_logs);
                for k in changed {
                    if !changed_agg.contains(&k) {
                        changed_agg.push(k);
                    }
                }
            }
            Err(e) => {
                let err = e.context(format!("config_render on {host} failed"));
                if first_err.is_none() {
                    first_err = Some(err);
                }
            }
        }
    }
    if let Some(err) = first_err {
        return Err(err);
    }

    let mut env = HashMap::new();
    set_changed_env(&mut env, &changed_agg);
    env.insert("BUZ_CONFIG_DIR".to_string(), remote_dir);
    logs.push(PrepLogLine::info("==> CONFIG READY".to_string()));
    Ok(ConfigPrep {
        env,
        forced_redact,
        logs,
    })
}

#[allow(clippy::too_many_arguments)]
async fn prepare_one_host_render_only(
    session: &mut SshSession,
    staging: &Path,
    remote_dir: &str,
    specs: &[EnvRenderSpec],
    values: &ValueMap,
    step_timeout: Option<Duration>,
) -> anyhow::Result<(Vec<String>, Vec<String>, Vec<PrepLogLine>)> {
    std::fs::create_dir_all(staging).with_context(|| {
        format!(
            "config_render: cannot create staging dir '{}'",
            staging.display()
        )
    })?;

    run_ssh(
        session,
        &format!(
            "test -d {remote_dir} || {{ echo \"config_render: dest '{remote_dir}' does not exist on remote\" >&2; exit 1; }}"
        ),
        step_timeout,
    )
    .await?;

    let mut host_changed: Vec<String> = Vec::new();
    let mut forced_redact = Vec::new();
    let mut host_logs: Vec<PrepLogLine> = Vec::new();
    for spec in specs {
        let remote_tmpl = format!("{remote_dir}/{}", spec.template);
        let tmpl = fetch_remote_text(session, &remote_tmpl, step_timeout)
            .await?
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "config_render: template '{}' not found on remote at '{}'",
                    spec.template,
                    remote_tmpl
                )
            })?;

        let local_tmpl = staging.join(&spec.template);
        if let Some(parent) = local_tmpl.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&local_tmpl, &tmpl).with_context(|| {
            format!(
                "config_render: cannot stage template '{}'",
                local_tmpl.display()
            )
        })?;

        ensure_remote_parent(session, remote_dir, &spec.output, step_timeout).await?;
        let remote_out = format!("{remote_dir}/{}", spec.output);
        let old = fetch_remote_text(session, &remote_out, step_timeout).await?;
        let (content, changed, forced) = render_env_with_old(
            staging,
            spec,
            values,
            "render-only",
            old.as_deref().unwrap_or(""),
        )?;
        if spec.backup_keep > 0 && old.is_some() {
            run_ssh(
                session,
                &backup_output_remote_cmd(&remote_out, spec.backup_keep),
                step_timeout,
            )
            .await?;
        }
        ssh_upload_bytes(
            session,
            &remote_out,
            content.as_bytes(),
            Some(spec.mode),
            step_timeout,
        )
        .await?;
        push_render_logs(&mut host_logs, &spec.template, &spec.output, &changed);
        forced_redact = forced;
        for k in changed {
            if !host_changed.contains(&k) {
                host_changed.push(k);
            }
        }
    }
    Ok((host_changed, forced_redact, host_logs))
}

#[cfg(test)]
#[path = "config_source_tests.rs"]
mod tests;
