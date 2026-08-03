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
use std::time::Duration;

use anyhow::{bail, Context};
use base64::Engine;
use reqwest::Client;

use crate::engine::ssh::{self, SshConnectionParams};
use shared::value_source::{self, ExpandMode, MissingPolicy, ValueMap};

use super::env_render;

/// Timeout for the signed OSS config bundle download.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(300);
/// Default mode for the rendered output file (`0600`).
const DEFAULT_ENV_MODE: u32 = 0o600;
/// Base64 chunk size for SSH uploads — each `echo | base64 -d` command stays
/// far below the remote ARG_MAX.
const SSH_CHUNK_BASE64: usize = 40 * 1024;

/// Result of preparing the config bundle for one step.
#[derive(Debug)]
pub struct ConfigPrep {
    /// Env overrides for the step env (`BUZ_CONFIG_DIR`, `BUZ_ENV_CHANGED`, …).
    pub env: HashMap<String, String>,
    /// Secret values written to the rendered `.env` — force-redact in logs.
    pub forced_redact: Vec<String>,
}

#[derive(Debug)]
struct EnvRenderSpec {
    template: String,
    output: String,
    mode: u32,
    on_missing: MissingPolicy,
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
    Ok(EnvRenderSpec {
        template: template.to_string(),
        output,
        mode,
        on_missing,
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

/// Download the signed OSS bundle.
async fn download(client: &Client, url: &str) -> anyhow::Result<Vec<u8>> {
    let resp = client
        .get(url)
        .timeout(DOWNLOAD_TIMEOUT)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("config bundle download failed: {e}"))?;
    if !resp.status().is_success() {
        bail!("config bundle download returned HTTP {}", resp.status());
    }
    Ok(resp.bytes().await?.to_vec())
}

/// Extract a tar.gz byte blob into `dest` via the system `tar`.
fn unpack(tarball: &[u8], dest: &Path) -> anyhow::Result<()> {
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
    Ok(())
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
    let (merged, changed) = env_render::merge_env(old, &expanded.content);
    env_render::round_trip_check(&merged).map_err(|e| anyhow::anyhow!("config_render: {e}"))?;
    // Forced value registration: every secret dispatched for this job is
    // force-redacted so the plaintext `.env` never echoes unredacted.
    let forced_redact: Vec<String> = values.secrets.values().cloned().collect();
    Ok((merged, changed, forced_redact))
}

/// Apply all render specs under `config_dir`, returning union of changed keys
/// and forced-redact values from the last render (secret set is job-wide).
fn apply_renders_local(
    config_dir: &Path,
    specs: &[EnvRenderSpec],
    values: &ValueMap,
    archive: &str,
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
        write_atomic(&output_path, &content, spec.mode)?;
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

/// Run one remote command, failing on a non-zero exit.
async fn run_ssh(
    conn: &SshConnectionParams,
    cmd: &str,
    step_timeout: Option<Duration>,
) -> anyhow::Result<()> {
    let out = ssh::execute(cmd, conn, step_timeout).await?;
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
    conn: &SshConnectionParams,
    remote_path: &str,
    step_timeout: Option<Duration>,
) -> anyhow::Result<Option<String>> {
    let out = ssh::execute(
        &format!("cat {remote_path} 2>/dev/null || true"),
        conn,
        step_timeout,
    )
    .await?;
    if out.exit_code != 0 || out.stdout.is_empty() {
        return Ok(None);
    }
    Ok(Some(out.stdout))
}

/// Transfer `bytes` to `remote_path` on the target: chunked base64 appended to
/// a temp file, then an atomic `mv` (and optional `chmod`) into place.
async fn ssh_upload_bytes(
    conn: &SshConnectionParams,
    remote_path: &str,
    bytes: &[u8],
    mode: Option<u32>,
    step_timeout: Option<Duration>,
) -> anyhow::Result<()> {
    let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
    let part = format!("/tmp/.buz_upload_{}", uuid::Uuid::new_v4().simple());
    let mut i = 0;
    while i < b64.len() {
        let end = (i + SSH_CHUNK_BASE64).min(b64.len());
        let chunk = &b64[i..end];
        let cmd = format!("echo -n '{chunk}' | base64 -d >> {part}");
        let out = ssh::execute(&cmd, conn, step_timeout).await?;
        if out.exit_code != 0 {
            bail!(
                "config upload to {remote_path} failed: {}",
                out.stderr.trim()
            );
        }
        i = end;
    }
    let mv = match mode {
        Some(m) => format!("mv -f {part} {remote_path} && chmod {m:o} {remote_path}"),
        None => format!("mv -f {part} {remote_path}"),
    };
    let out = ssh::execute(&mv, conn, step_timeout).await?;
    if out.exit_code != 0 {
        bail!(
            "config upload finalize to {remote_path} failed: {}",
            out.stderr.trim()
        );
    }
    Ok(())
}

async fn ensure_remote_parent(
    conn: &SshConnectionParams,
    remote_dir: &str,
    relative: &str,
    step_timeout: Option<Duration>,
) -> anyhow::Result<()> {
    if let Some(parent) = Path::new(relative).parent() {
        let p = parent.to_string_lossy();
        if !p.is_empty() && p != "." {
            run_ssh(conn, &format!("mkdir -p {remote_dir}/{p}"), step_timeout).await?;
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

    let archive = if has_config_auth(step) {
        let url = config_url(step)?;
        let label = archive_label(step);
        let tarball = download(client, &url).await?;
        unpack(&tarball, &local_dir)?;
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
        let (changed_all, forced) = apply_renders_local(&local_dir, &specs, values, &archive)?;
        forced_redact = forced;
        set_changed_env(&mut env, &changed_all);
    }
    env.insert(
        "BUZ_CONFIG_DIR".to_string(),
        local_dir.to_string_lossy().into_owned(),
    );
    Ok(ConfigPrep { env, forced_redact })
}

/// Prepare for SSH: OSS deliver uploads the bundle; render-only fetches
/// templates from the remote, renders on the Runner, uploads outputs.
#[allow(clippy::too_many_arguments)]
pub async fn prepare_ssh(
    client: &Client,
    step: &serde_json::Value,
    conns: &[SshConnectionParams],
    local_root: &Path,
    job_id: &str,
    step_idx: usize,
    values: &ValueMap,
    step_timeout: Option<Duration>,
) -> anyhow::Result<ConfigPrep> {
    let specs = config_render_specs(step).map_err(anyhow::Error::msg)?;
    let remote_dir = resolve_remote_dir(step, job_id, step_idx);

    if has_config_auth(step) {
        return prepare_ssh_deliver(
            client,
            step,
            conns,
            local_root,
            job_id,
            step_idx,
            values,
            step_timeout,
            &specs,
            &remote_dir,
        )
        .await;
    }

    if specs.is_empty() {
        bail!("config prepare: step has neither config_auth nor config_render");
    }

    prepare_ssh_render_only(
        step,
        conns,
        local_root,
        job_id,
        step_idx,
        values,
        step_timeout,
        &specs,
        &remote_dir,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn prepare_ssh_deliver(
    client: &Client,
    step: &serde_json::Value,
    conns: &[SshConnectionParams],
    local_root: &Path,
    job_id: &str,
    step_idx: usize,
    values: &ValueMap,
    step_timeout: Option<Duration>,
    specs: &[EnvRenderSpec],
    remote_dir: &str,
) -> anyhow::Result<ConfigPrep> {
    let url = config_url(step)?;
    let archive = archive_label(step);
    let tarball = download(client, &url).await?;
    let local_dir = local_root.join(format!("config-{job_id}-{step_idx}"));
    unpack(&tarball, &local_dir)?;
    let tar_remote = format!("/tmp/buz_config_{job_id}_{step_idx}.tar.gz");

    let mut forced_redact = Vec::new();
    let mut changed_agg: Vec<String> = Vec::new();
    for conn in conns {
        ssh_upload_bytes(conn, &tar_remote, &tarball, None, step_timeout).await?;
        let unpack_cmd = format!(
            "mkdir -p {remote_dir} && tar xzf {tar_remote} -C {remote_dir} && rm -f {tar_remote}"
        );
        run_ssh(conn, &unpack_cmd, step_timeout).await?;
        let mut host_changed: Vec<String> = Vec::new();
        for spec in specs {
            ensure_remote_parent(conn, remote_dir, &spec.output, step_timeout).await?;
            let remote_out = format!("{remote_dir}/{}", spec.output);
            let old = fetch_remote_text(conn, &remote_out, step_timeout).await?;
            let (content, changed, forced) = render_env_with_old(
                &local_dir,
                spec,
                values,
                &archive,
                old.as_deref().unwrap_or(""),
            )?;
            ssh_upload_bytes(
                conn,
                &remote_out,
                content.as_bytes(),
                Some(spec.mode),
                step_timeout,
            )
            .await?;
            forced_redact = forced;
            for k in changed {
                if !host_changed.contains(&k) {
                    host_changed.push(k);
                }
            }
        }
        for k in host_changed {
            if !changed_agg.contains(&k) {
                changed_agg.push(k);
            }
        }
    }

    let mut env = HashMap::new();
    if !specs.is_empty() {
        set_changed_env(&mut env, &changed_agg);
    }
    env.insert("BUZ_CONFIG_DIR".to_string(), remote_dir.to_string());
    Ok(ConfigPrep { env, forced_redact })
}

#[allow(clippy::too_many_arguments)]
async fn prepare_ssh_render_only(
    _step: &serde_json::Value,
    conns: &[SshConnectionParams],
    local_root: &Path,
    job_id: &str,
    step_idx: usize,
    values: &ValueMap,
    step_timeout: Option<Duration>,
    specs: &[EnvRenderSpec],
    remote_dir: &str,
) -> anyhow::Result<ConfigPrep> {
    let staging = local_root.join(format!("config-render-{job_id}-{step_idx}"));
    std::fs::create_dir_all(&staging).with_context(|| {
        format!(
            "config_render: cannot create staging dir '{}'",
            staging.display()
        )
    })?;

    let mut forced_redact = Vec::new();
    let mut changed_agg: Vec<String> = Vec::new();

    for conn in conns {
        // Templates must already exist under dest (prior deliver / git / manual).
        run_ssh(
            conn,
            &format!(
                "test -d {remote_dir} || {{ echo \"config_render: dest '{remote_dir}' does not exist on remote\" >&2; exit 1; }}"
            ),
            step_timeout,
        )
        .await?;

        let mut host_changed: Vec<String> = Vec::new();
        for spec in specs {
            let remote_tmpl = format!("{remote_dir}/{}", spec.template);
            let tmpl = fetch_remote_text(conn, &remote_tmpl, step_timeout)
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

            ensure_remote_parent(conn, remote_dir, &spec.output, step_timeout).await?;
            let remote_out = format!("{remote_dir}/{}", spec.output);
            let old = fetch_remote_text(conn, &remote_out, step_timeout).await?;
            let (content, changed, forced) = render_env_with_old(
                &staging,
                spec,
                values,
                "render-only",
                old.as_deref().unwrap_or(""),
            )?;
            ssh_upload_bytes(
                conn,
                &remote_out,
                content.as_bytes(),
                Some(spec.mode),
                step_timeout,
            )
            .await?;
            forced_redact = forced;
            for k in changed {
                if !host_changed.contains(&k) {
                    host_changed.push(k);
                }
            }
        }
        for k in host_changed {
            if !changed_agg.contains(&k) {
                changed_agg.push(k);
            }
        }
    }

    let mut env = HashMap::new();
    set_changed_env(&mut env, &changed_agg);
    env.insert("BUZ_CONFIG_DIR".to_string(), remote_dir.to_string());
    Ok(ConfigPrep { env, forced_redact })
}

#[cfg(test)]
#[path = "config_source_tests.rs"]
mod tests;
