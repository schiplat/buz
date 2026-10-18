//! Docker engine — run step `run:` inside an ephemeral container on the runner host.

use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;

use super::shell::ReadGuard;
use crate::config::{DockerMountPolicy, RunnerSection};

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
///
/// Test-only convenience wrapper; production callers pass socket/name via
/// [`build_docker_run_args_with_socket`].
#[cfg(test)]
pub fn build_docker_run_args(
    image: &str,
    runner_workspace_dir: &str,
    command: &str,
    env: &HashMap<String, String>,
    volumes: &[String],
    container_work_dir: Option<&str>,
    privileged: bool,
) -> Vec<String> {
    build_docker_run_args_with_socket(
        image,
        runner_workspace_dir,
        command,
        env,
        volumes,
        container_work_dir,
        privileged,
        None,
        None,
    )
}

fn build_docker_run_args_with_socket(
    image: &str,
    runner_workspace_dir: &str,
    command: &str,
    env: &HashMap<String, String>,
    volumes: &[String],
    container_work_dir: Option<&str>,
    privileged: bool,
    docker_socket_path: Option<&Path>,
    container_name: Option<&str>,
) -> Vec<String> {
    let mut args = vec!["run".into(), "--rm".into()];

    if let Some(name) = container_name {
        args.push("--name".into());
        args.push(name.into());
    }
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
        // When the authorized local socket is mounted, force the in-container
        // Docker CLI to use it rather than a caller-selected remote endpoint.
        if docker_socket_path.is_some() && matches!(k.as_str(), "DOCKER_HOST" | "DOCKER_CONTEXT") {
            continue;
        }
        args.push("-e".into());
        args.push(format!("{k}={v}"));
        // Match shell engine's INPUT_* convention for trigger-style vars.
        let input_key = shared::protocol::input_env_key(k);
        if !env.contains_key(&input_key) {
            args.push("-e".into());
            args.push(format!("{input_key}={v}"));
        }
    }

    // User-defined volumes (host:container bind mounts)
    for vol in volumes {
        let v = vol.trim();
        if !v.is_empty() {
            if docker_socket_path.is_some() && is_socket_volume(v) {
                continue;
            }
            args.push("-v".into());
            args.push(v.to_string());
        }
    }
    if let Some(socket_path) = docker_socket_path {
        args.push("-v".into());
        args.push(format!("{}:/var/run/docker.sock", socket_path.display()));
    }

    args.push(image.into());
    args.push("sh".into());
    args.push("-elc".into());
    args.push(command.into());
    args
}

/// Validate that a Docker step requests only Runner-authorized local resources.
/// This check is independent of Manager's mutable engine policy and must run
/// before any host-side prepare/login action.
pub fn validate_step_policy(
    runner: &RunnerSection,
    capabilities: &[String],
    volumes: &[String],
    privileged: bool,
    has_host_prepare: bool,
) -> anyhow::Result<Option<std::path::PathBuf>> {
    let mut socket_path = None;
    let mut wants_socket = false;
    for capability in capabilities {
        match capability.as_str() {
            "docker-socket" if !wants_socket => wants_socket = true,
            "docker-socket" => anyhow::bail!("duplicate Docker capability 'docker-socket'"),
            other => anyhow::bail!("unsupported Docker capability {other:?}"),
        }
    }
    if wants_socket {
        if !runner.docker_socket_allowed {
            anyhow::bail!("docker-socket capability denied by local Runner policy");
        }
        socket_path = Some(validate_socket_path(&runner.docker_socket_path)?);
    }
    let mut explicit_socket_volume = false;
    for volume in volumes {
        if is_socket_volume(volume) {
            if runner.docker_mount_policy == DockerMountPolicy::Strict {
                anyhow::bail!("explicit Docker socket mounts are forbidden by strict local Runner policy; the locked capability injects the socket automatically");
            }
            let allowed_legacy =
                volume.trim() == format!("{}:/var/run/docker.sock", runner.docker_socket_path);
            if !runner.docker_socket_allowed || !allowed_legacy {
                anyhow::bail!(
                    "Docker socket volume must exactly match the locally authorized socket"
                );
            }
            if explicit_socket_volume {
                anyhow::bail!("duplicate Docker socket volume is not allowed");
            }
            explicit_socket_volume = true;
            socket_path = Some(validate_socket_path(&runner.docker_socket_path)?);
            if !capabilities.iter().any(|c| c == "docker-socket") {
                tracing::warn!("legacy explicit Docker socket volume accepted under local authorization; declare capabilities: [docker-socket] to migrate");
            }
            continue;
        }
        if runner.docker_mount_policy == DockerMountPolicy::Strict {
            anyhow::bail!(
                "explicit Docker volume {volume:?} is forbidden by strict local Runner policy"
            );
        }
    }
    if runner.docker_mount_policy == DockerMountPolicy::Strict && privileged {
        anyhow::bail!("privileged Docker containers are forbidden by strict local Runner policy");
    }
    if runner.docker_mount_policy == DockerMountPolicy::Strict && has_host_prepare {
        anyhow::bail!(
            "strict Docker policy forbids host-side config_source/config_render preparation"
        );
    }
    Ok(socket_path)
}

fn is_socket_volume(volume: &str) -> bool {
    let mut parts = volume.trim().split(':');
    let source = parts.next().unwrap_or_default();
    let target = parts.next().unwrap_or_default();
    source.ends_with("docker.sock") || target.ends_with("docker.sock")
}

fn validate_socket_path(path: &str) -> anyhow::Result<std::path::PathBuf> {
    let p = Path::new(path);
    if !p.is_absolute()
        || p.components().any(|c| {
            !matches!(
                c,
                std::path::Component::RootDir | std::path::Component::Normal(_)
            )
        })
        || path.trim().is_empty()
        || path.contains(':')
        || path.contains(',')
    {
        anyhow::bail!("docker_socket_path must be an absolute, simple Unix socket path");
    }
    let metadata = std::fs::symlink_metadata(p)
        .map_err(|e| anyhow::anyhow!("cannot inspect docker_socket_path {path:?}: {e}"))?;
    if metadata.file_type().is_symlink() {
        anyhow::bail!("docker_socket_path must not be a symlink");
    }
    // Keep an operator-owned socket's parent path stable and mount the canonical
    // target, never a symlink alias supplied by configuration.
    let canonical = std::fs::canonicalize(p)
        .map_err(|e| anyhow::anyhow!("cannot resolve docker_socket_path {path:?}: {e}"))?;
    let metadata = std::fs::metadata(&canonical)
        .map_err(|e| anyhow::anyhow!("cannot inspect docker_socket_path {path:?}: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileTypeExt;
        if !metadata.file_type().is_socket() {
            anyhow::bail!("docker_socket_path {path:?} is not a Unix domain socket");
        }
    }
    #[cfg(not(unix))]
    anyhow::bail!("docker-socket capability is supported only on Unix runners");
    Ok(canonical)
}

/// Preconditions before spawning docker.
pub fn preflight(docker_enabled: bool, image: &str) -> anyhow::Result<()> {
    if !docker_enabled {
        anyhow::bail!(
            "Docker engine is disabled on this runner \
             (enable docker_enabled in Manager UI → Runners). \
             Refusing to fall back to shell engine."
        );
    }
    let image = image.trim();
    if image.is_empty() {
        anyhow::bail!("Docker step is missing required 'image' field");
    }
    Ok(())
}

/// `docker login --password-stdin` for a private registry.
///
/// `docker_config_dir` — when set, the CLI writes credentials into this
/// job-scoped config dir (`DOCKER_CONFIG`) instead of the host-global
/// `~/.docker/config.json`. The caller removes it when the job ends.
pub async fn registry_login(
    registry: &str,
    username: &str,
    password: &str,
    docker_config_dir: Option<&str>,
) -> anyhow::Result<()> {
    let registry = registry.trim();
    let username = username.trim();
    if registry.is_empty() || username.is_empty() || password.is_empty() {
        anyhow::bail!("registry_auth requires non-empty registry, username, and password");
    }

    let mut command = Command::new("docker");
    if let Some(dir) = docker_config_dir {
        command.env("DOCKER_CONFIG", dir);
    }
    let mut child = command
        .args([
            "login",
            "--username",
            username,
            "--password-stdin",
            registry,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| {
            anyhow::anyhow!("failed to spawn docker login (is the Docker CLI installed?): {e}")
        })?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(password.as_bytes())
            .await
            .map_err(|e| anyhow::anyhow!("failed to write password to docker login stdin: {e}"))?;
        // Drop stdin to close the pipe (EOF for --password-stdin).
        drop(stdin);
    }

    let output = tokio::time::timeout(Duration::from_secs(60), child.wait_with_output())
        .await
        .map_err(|_| anyhow::anyhow!("docker login timed out after 60s"))?
        .map_err(|e| anyhow::anyhow!("docker login failed: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        anyhow::bail!(
            "docker login to '{registry}' failed (exit {}): {}{}",
            output.status.code().unwrap_or(-1),
            stderr.trim(),
            if stdout.trim().is_empty() {
                String::new()
            } else {
                format!(" {}", stdout.trim())
            }
        );
    }
    Ok(())
}

/// Best-effort `docker logout` so credentials do not linger across jobs on shared runners.
///
/// `docker_config_dir` — scope the logout to the same job-scoped config dir
/// the login used (see [`registry_login`]); `None` for the host-global default.
pub async fn registry_logout(registry: &str, docker_config_dir: Option<&str>) {
    let registry = registry.trim();
    if registry.is_empty() {
        return;
    }
    let mut command = Command::new("docker");
    if let Some(dir) = docker_config_dir {
        command.env("DOCKER_CONFIG", dir);
    }
    let result = tokio::time::timeout(
        Duration::from_secs(30),
        command
            .args(["logout", registry])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status(),
    )
    .await;
    match result {
        Ok(Ok(status)) if status.success() => {}
        Ok(Ok(status)) => {
            tracing::warn!(
                "docker logout from '{registry}' exited {}",
                status.code().unwrap_or(-1)
            );
        }
        Ok(Err(e)) => tracing::warn!("docker logout from '{registry}' failed: {e}"),
        Err(_) => tracing::warn!("docker logout from '{registry}' timed out"),
    }
}

pub async fn stop_container(name: &str, docker_config_dir: Option<&str>) {
    if name.trim().is_empty() {
        return;
    }
    let mut command = Command::new("docker");
    if let Some(dir) = docker_config_dir {
        command.env("DOCKER_CONFIG", dir);
    }
    match tokio::time::timeout(
        Duration::from_secs(15),
        command
            .args(["rm", "--force", name])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status(),
    )
    .await
    {
        Ok(Ok(status)) if status.success() => {}
        Ok(Ok(status)) => tracing::warn!(
            "Docker cleanup for container {name:?} exited {}",
            status.code().unwrap_or(-1)
        ),
        Ok(Err(error)) => tracing::warn!("Docker cleanup for container {name:?} failed: {error}"),
        Err(_) => tracing::warn!("Docker cleanup for container {name:?} timed out"),
    }
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
///
/// `docker_config_dir` — when set, the Docker CLI (image pull / push auth)
/// reads credentials from this job-scoped config dir (`DOCKER_CONFIG`) instead
/// of the host-global `~/.docker/config.json`. Container env is unaffected.
/// Test-only convenience wrapper; production callers use
/// [`execute_streaming_with_socket`].
#[cfg(test)]
#[allow(clippy::too_many_arguments)] // docker run surface: image/workdir/cmd/env/volumes/workdir/privileged + config scope
pub async fn execute_streaming(
    image: &str,
    runner_workspace_dir: &Path,
    command: &str,
    env: &HashMap<String, String>,
    volumes: &[String],
    container_work_dir: Option<&str>,
    privileged: bool,
    docker_config_dir: Option<&str>,
) -> anyhow::Result<(Child, mpsc::UnboundedReceiver<String>, ReadGuard)> {
    execute_streaming_with_socket(
        image,
        runner_workspace_dir,
        command,
        env,
        volumes,
        container_work_dir,
        privileged,
        docker_config_dir,
        None,
        None,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub async fn execute_streaming_with_socket(
    image: &str,
    runner_workspace_dir: &Path,
    command: &str,
    env: &HashMap<String, String>,
    volumes: &[String],
    container_work_dir: Option<&str>,
    privileged: bool,
    docker_config_dir: Option<&str>,
    docker_socket_path: Option<&Path>,
    container_name: Option<&str>,
) -> anyhow::Result<(Child, mpsc::UnboundedReceiver<String>, ReadGuard)> {
    std::fs::create_dir_all(runner_workspace_dir).map_err(|e| {
        anyhow::anyhow!(
            "failed to create docker workspace {}: {e}",
            runner_workspace_dir.display()
        )
    })?;

    let runner_wd_str = runner_workspace_dir.to_string_lossy();
    let args = build_docker_run_args_with_socket(
        image,
        &runner_wd_str,
        command,
        env,
        volumes,
        container_work_dir,
        privileged,
        docker_socket_path,
        container_name,
    );
    let mut cmd = Command::new("docker");
    cmd.args(&args);
    if let Some(dir) = docker_config_dir {
        cmd.env("DOCKER_CONFIG", dir);
    }
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
