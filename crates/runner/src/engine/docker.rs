//! Docker engine — run step `run:` inside an ephemeral container on the runner host.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;

use super::shell::ReadGuard;

/// Fixed mount point / working directory inside the container (design).
pub const CONTAINER_WORKSPACE: &str = "/buz/workspace";

/// Build `docker run` argv (without the `docker` binary name).
///
/// Order: global flags → volumes → env → image → shell + script.
pub fn build_docker_run_args(
    image: &str,
    host_work_dir: &Path,
    command: &str,
    env: &HashMap<String, String>,
    volumes: &[String],
) -> Vec<String> {
    let mut args = vec![
        "run".into(),
        "--rm".into(),
        "-i".into(),
        "-v".into(),
        format!("{}:{CONTAINER_WORKSPACE}", host_work_dir.display()),
        "-w".into(),
        CONTAINER_WORKSPACE.into(),
    ];

    for (k, v) in env {
        args.push("-e".into());
        args.push(format!("{k}={v}"));
        // Match local engine's INPUT_* convention for trigger-style vars.
        let input_key = format!("INPUT_{}", k.to_uppercase());
        if !env.contains_key(&input_key) {
            args.push("-e".into());
            args.push(format!("{input_key}={v}"));
        }
    }

    // User-defined volumes (host:container bind mounts)
    for vol in volumes {
        let v = vol.trim();
        if !v.is_empty() {
            args.push("-v".into());
            args.push(v.to_string());
        }
    }

    args.push(image.into());
    args.push("sh".into());
    args.push("-lc".into());
    args.push(command.into());
    args
}

/// Resolve the host directory to bind-mount (absolute).
///
/// Only allows paths **within** the runner's `work_dir`. An absolute path
/// that escapes `runner_work_dir` returns an error — this prevents Docker
/// containers from mounting arbitrary host paths through `work_dir`.
pub fn resolve_host_work_dir(runner_work_dir: &str, step_work_dir: Option<&str>) -> std::io::Result<PathBuf> {
    let base = PathBuf::from(runner_work_dir).canonicalize().unwrap_or_else(|_| PathBuf::from(runner_work_dir));
    let resolved = match step_work_dir.map(str::trim).filter(|s| !s.is_empty()) {
        Some(rel) => {
            let p = Path::new(rel);
            if p.is_absolute() {
                // Canonicalize so symlinks / .. don't bypass the sandbox
                p.canonicalize().unwrap_or_else(|_| p.to_path_buf())
            } else {
                base.join(rel)
            }
        }
        None => base.clone(),
    };

    // Reject if the resolved path escapes the runner work_dir
    if !resolved.starts_with(&base) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!(
                "work_dir '{}' escapes the runner work directory '{}'",
                step_work_dir.unwrap_or(""),
                runner_work_dir,
            ),
        ));
    }
    Ok(resolved)
}

/// Preconditions before spawning docker.
pub fn preflight(docker_enabled: bool, image: &str) -> anyhow::Result<()> {
    if !docker_enabled {
        anyhow::bail!(
            "Docker engine is disabled on this runner \
             (set engine.docker_enabled = true in runner.toml). \
             Refusing to fall back to local shell."
        );
    }
    let image = image.trim();
    if image.is_empty() {
        anyhow::bail!("Docker step is missing required 'image' field");
    }
    Ok(())
}

/// Spawn `docker run` with streaming stdout/stderr (same shape as local engine).
pub async fn execute_streaming(
    image: &str,
    host_work_dir: &Path,
    command: &str,
    env: &HashMap<String, String>,
    volumes: &[String],
) -> anyhow::Result<(Child, mpsc::UnboundedReceiver<String>, ReadGuard)> {
    std::fs::create_dir_all(host_work_dir).map_err(|e| {
        anyhow::anyhow!(
            "failed to create docker workspace {}: {e}",
            host_work_dir.display()
        )
    })?;

    let args = build_docker_run_args(image, host_work_dir, command, env, volumes);
    let mut cmd = Command::new("docker");
    cmd.args(&args);
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    let mut child = cmd.spawn().map_err(|e| {
        anyhow::anyhow!(
            "failed to spawn docker (is the Docker CLI installed and daemon running?): {e}"
        )
    })?;

    let (tx, rx) = mpsc::unbounded_channel();
    let child_stdout = child.stdout.take();
    let child_stderr = child.stderr.take();

    let tx_out = tx.clone();
    let stdout_handle = tokio::spawn(async move {
        if let Some(stdout) = child_stdout {
            let mut reader = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                let _ = tx_out.send(format!("{line}\n"));
            }
        }
    });

    let tx_err = tx.clone();
    let stderr_handle = tokio::spawn(async move {
        if let Some(stderr) = child_stderr {
            let mut reader = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                let _ = tx_err.send(format!("{line}\n"));
            }
        }
    });

    let guard = ReadGuard::new(tx, stdout_handle, stderr_handle);

    Ok((child, rx, guard))
}

#[cfg(test)]
#[path = "docker_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "docker_e2e_tests.rs"]
mod e2e_tests;
