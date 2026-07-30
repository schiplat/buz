//! Docker engine — run step `run:` inside an ephemeral container on the runner host.

use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;

use super::shell::ReadGuard;

/// Fixed mount point / working directory inside the container (design).
pub const CONTAINER_WORKSPACE: &str = "/buz/workspace";

/// Build `docker run` argv (without the `docker` binary name).
///
/// Workspace model (GitHub Actions style):
/// - The runner's `runner_workspace_dir` is **always** bind-mounted as `CONTAINER_WORKSPACE`.
/// - `container_work_dir` is an optional path **inside** the container (relative to workspace).
///   When set, `--workdir` points to `CONTAINER_WORKSPACE/<container_work_dir>`.
///   When absent, `--workdir` defaults to `CONTAINER_WORKSPACE`.
///
/// Order: global flags → volumes → env → image → shell + script.
pub fn build_docker_run_args(
    image: &str,
    runner_workspace_dir: &str,
    command: &str,
    env: &HashMap<String, String>,
    volumes: &[String],
    container_work_dir: Option<&str>,
    privileged: bool,
) -> Vec<String> {
    let mut args = vec![
        "run".into(),
        "--rm".into(),
    ];

    if privileged {
        args.push("--privileged".into());
    }

    args.push("-v".into());
    args.push(format!("{runner_workspace_dir}:{CONTAINER_WORKSPACE}"));
    args.push("-w".into());

    let wd = match container_work_dir.map(str::trim).filter(|s| !s.is_empty()) {
        Some(rel) => format!("{CONTAINER_WORKSPACE}/{rel}"),
        None => CONTAINER_WORKSPACE.into(),
    };
    args.push(wd);

    for (k, v) in env {
        args.push("-e".into());
        args.push(format!("{k}={v}"));
        // Match shell engine's INPUT_* convention for trigger-style vars.
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
    args.push("-elc".into());
    args.push(command.into());
    args
}

/// Preconditions before spawning docker.
pub fn preflight(docker_enabled: bool, image: &str) -> anyhow::Result<()> {
    if !docker_enabled {
        anyhow::bail!(
            "Docker engine is disabled on this runner \
             (set engine.docker_enabled = true in runner.toml). \
             Refusing to fall back to shell engine."
        );
    }
    let image = image.trim();
    if image.is_empty() {
        anyhow::bail!("Docker step is missing required 'image' field");
    }
    Ok(())
}

/// Spawn `docker run` with streaming stdout/stderr (same shape as shell engine).
///
/// `runner_workspace_dir` — the runner's own work directory (e.g. `/tmp/buz-runner`).
/// It is always bind-mounted into the container at `CONTAINER_WORKSPACE`.
///
/// `container_work_dir` — optional subdirectory inside the container workspace
/// to use as the working directory (e.g. `frontend` → `--workdir /buz/workspace/frontend`).
///
/// `privileged` — when true, adds `--privileged` to the container (required for Docker-in-Docker).
pub async fn execute_streaming(
    image: &str,
    runner_workspace_dir: &Path,
    command: &str,
    env: &HashMap<String, String>,
    volumes: &[String],
    container_work_dir: Option<&str>,
    privileged: bool,
) -> anyhow::Result<(Child, mpsc::UnboundedReceiver<String>, ReadGuard)> {
    std::fs::create_dir_all(runner_workspace_dir).map_err(|e| {
        anyhow::anyhow!(
            "failed to create docker workspace {}: {e}",
            runner_workspace_dir.display()
        )
    })?;

    let runner_wd_str = runner_workspace_dir.to_string_lossy();
    let args = build_docker_run_args(
        image,
        &runner_wd_str,
        command,
        env,
        volumes,
        container_work_dir,
        privileged,
    );
    let mut cmd = Command::new("docker");
    cmd.args(&args);
    cmd.stdin(Stdio::null());
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
