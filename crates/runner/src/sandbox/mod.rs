//! Host-side job sandbox supervisor (`buz-sandboxd` binary).
//!
//! This is the **server half** of the sandbox protocol; the Runner-side client
//! lives in `crate::client::sandbox_client`. Run this service under an
//! operator-managed account with access to the selected container
//! daemon/runtime. The Runner connects over a permissioned Unix socket and
//! never receives daemon, root, or KVM access itself.

pub mod backend;
pub mod config;
pub mod manager_fetch;

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::process::Command;
use tokio::sync::Mutex;

use backend::{validate_prerequisites, BackendDescriptor};
pub use config::{AppliedConfig, Bootstrap, ConfigState};
use shared::protocol::{RunnerExecutionCapabilities, RunnerExecutionMode};
use shared::sandbox_config::ResourceProfile;

const MAX_REQUEST_BYTES: usize = 1024 * 1024;
const MAX_EXEC_SECONDS: u64 = 24 * 60 * 60;

/// The supervisor's view of the physical host, injectable so prerequisite
/// rules (Linux, `/dev/kvm`) are testable on any development machine.
#[derive(Debug, Clone)]
pub struct HostFacts {
    pub os: String,
    pub kvm_path: PathBuf,
}

impl HostFacts {
    pub fn detect() -> Self {
        Self {
            os: std::env::consts::OS.to_string(),
            kvm_path: PathBuf::from("/dev/kvm"),
        }
    }
}

impl Default for HostFacts {
    fn default() -> Self {
        Self::detect()
    }
}

pub async fn cleanup_stale_containers(config: &Bootstrap) -> anyhow::Result<()> {
    let output = Command::new(&config.docker_cli)
        .args([
            "ps",
            "--all",
            "--quiet",
            "--filter",
            &format!("label=buz.sandboxd.instance={}", config.instance_id),
            "--filter",
            "label=buz.sandboxd.managed=true",
        ])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .await?;
    anyhow::ensure!(
        output.status.success(),
        "cannot list stale sandbox containers"
    );
    for id in String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|id| !id.is_empty())
    {
        let mut remove = Command::new(&config.docker_cli);
        remove
            .args(["rm", "--force", id])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let result = tokio::time::timeout(Duration::from_secs(30), remove.output())
            .await
            .map_err(|_| anyhow::anyhow!("stale sandbox cleanup timed out"))??;
        anyhow::ensure!(
            result.status.success(),
            "failed to remove stale sandbox container {id}: {}",
            String::from_utf8_lossy(&result.stderr).trim()
        );
    }
    Ok(())
}

/// Per-connection view of the supervisor: immutable bootstrap, the
/// atomically replaceable Manager policy, live jobs, and host facts.
#[derive(Clone)]
pub struct Service {
    pub bootstrap: Arc<Bootstrap>,
    pub config: ConfigState,
    pub jobs: Arc<Mutex<HashMap<String, JobRuntime>>>,
    pub host: HostFacts,
}

#[derive(Clone)]
pub struct JobRuntime {
    pub container: String,
    pub workspace: PathBuf,
}

async fn health_response(stream: UnixStream) -> anyhow::Result<()> {
    let mut request = serde_json::to_vec(&json!({"op":"health"}))?;
    request.push(b'\n');
    let mut stream = stream;
    stream.write_all(&request).await?;
    stream.shutdown().await?;
    let mut line = String::new();
    let count = tokio::time::timeout(
        Duration::from_secs(2),
        BufReader::new(stream).read_line(&mut line),
    )
    .await
    .map_err(|_| anyhow::anyhow!("sandboxd health response timed out"))??;
    anyhow::ensure!(count > 0, "sandboxd returned no health response");
    let response: Value = serde_json::from_str(&line)?;
    anyhow::ensure!(
        response["ok"] == true,
        "sandboxd health response is not healthy"
    );
    Ok(())
}

pub async fn healthcheck(socket: &str) -> anyhow::Result<()> {
    let stream = tokio::time::timeout(Duration::from_secs(2), UnixStream::connect(socket))
        .await
        .map_err(|_| anyhow::anyhow!("sandboxd healthcheck connect timed out"))??;
    health_response(stream).await
}
pub async fn serve(bootstrap: Bootstrap, initial: ConfigState) -> anyhow::Result<()> {
    anyhow::ensure!(
        bootstrap.allowed_runner_uid.is_some(),
        "sandboxd allowed_runner_uid must be configured"
    );
    anyhow::ensure!(
        !bootstrap.instance_id.trim().is_empty(),
        "sandboxd instance_id cannot be empty"
    );
    let workspace_root = Path::new(&bootstrap.workspace_root);
    std::fs::create_dir_all(workspace_root)?;
    let root = std::fs::canonicalize(workspace_root)?;
    let socket = Path::new(&bootstrap.socket);
    std::fs::create_dir_all(socket.parent().unwrap_or(Path::new("/run")))?;
    if std::fs::symlink_metadata(socket).is_ok() {
        match UnixStream::connect(socket).await {
            Ok(active) => {
                health_response(active).await?;
                anyhow::bail!(
                    "another sandboxd instance is already listening at {}",
                    socket.display()
                );
            }
            Err(_) => std::fs::remove_file(socket)?,
        }
    }
    cleanup_stale_containers(&bootstrap).await?;
    let listener = UnixListener::bind(socket)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{chown, PermissionsExt};
        if let Some(parent) = socket.parent() {
            chown(parent, Some(0), Some(bootstrap.socket_gid))?;
            std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o2770))?;
        }
        chown(socket, Some(0), Some(bootstrap.socket_gid))?;
        std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o660))?;
    }
    tracing::info!(socket = %bootstrap.socket, workspace_root = %root.display(), "sandbox supervisor listening");
    let service = Service {
        config: initial,
        jobs: Arc::new(Mutex::new(HashMap::new())),
        host: HostFacts::detect(),
        bootstrap: Arc::new(Bootstrap {
            workspace_root: root.to_string_lossy().into_owned(),
            ..bootstrap
        }),
    };
    // Deliver Manager policy only when a Manager URL is configured; tests and
    // air-gapped debugging run offline against the (empty) state.
    if !service.bootstrap.manager_url.trim().is_empty() {
        let fetch_bootstrap = (*service.bootstrap).clone();
        let fetch_state = service.config.clone();
        tokio::spawn(manager_fetch::run_fetch_loop(fetch_bootstrap, fetch_state));
    }
    loop {
        let (stream, _) = listener.accept().await?;
        let service = service.clone();
        tokio::spawn(async move {
            if let Err(error) = handle_connection(service, stream).await {
                tracing::warn!(error = %error, "sandbox supervisor request failed");
            }
        });
    }
}

async fn handle_connection(service: Service, mut stream: UnixStream) -> anyhow::Result<()> {
    let (uid, gid) = peer_identity(&stream)?;
    if service
        .bootstrap
        .allowed_runner_uid
        .is_some_and(|allowed| allowed != uid)
    {
        return write_response(
            &mut stream,
            json!({"ok": false, "error": "runner identity is not allowed"}),
        )
        .await;
    }
    let reader = BufReader::new(&mut stream);
    let mut line = Vec::new();
    let mut limited = reader.take((MAX_REQUEST_BYTES + 1) as u64);
    limited.read_until(b'\n', &mut line).await?;
    if line.len() > MAX_REQUEST_BYTES {
        return write_response(
            &mut stream,
            json!({"ok": false, "error": "request too large"}),
        )
        .await;
    }
    let request: Value = match serde_json::from_slice(&line) {
        Ok(value) => value,
        Err(_) => {
            return write_response(
                &mut stream,
                json!({"ok": false, "error": "invalid request"}),
            )
            .await
        }
    };
    let operation = request
        .get("op")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if operation == "health" {
        return write_response(&mut stream, json!({"ok": true})).await;
    }
    if operation == "exec_step" {
        return exec_step_stream(&service, &request, stream).await;
    }
    let response = match process_request(&service, request, uid, gid).await {
        Ok(value) => value,
        Err(error) => json!({"ok": false, "error": error.to_string()}),
    };
    write_response(&mut stream, response).await
}

#[cfg(unix)]
fn peer_identity(stream: &UnixStream) -> std::io::Result<(u32, u32)> {
    let credentials = stream.peer_cred()?;
    Ok((credentials.uid(), credentials.gid()))
}

#[cfg(not(unix))]
fn peer_identity(_stream: &UnixStream) -> std::io::Result<(u32, u32)> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "Unix peer credentials unavailable",
    ))
}

async fn write_response<W: AsyncWrite + Unpin>(
    stream: &mut W,
    response: Value,
) -> anyhow::Result<()> {
    let mut bytes = serde_json::to_vec(&response)?;
    bytes.push(b'\n');
    stream.write_all(&bytes).await?;
    Ok(())
}

async fn process_request(
    service: &Service,
    request: Value,
    uid: u32,
    gid: u32,
) -> anyhow::Result<Value> {
    match request
        .get("op")
        .and_then(Value::as_str)
        .unwrap_or_default()
    {
        "health" => Ok(json!({"ok": true})),
        "status" => Ok(status(service)),
        "get_config" => Ok(get_config(service)),
        "create_job" => create_job(service, &request, uid, gid).await,
        "probe_capabilities" => probe_capabilities(service).await,
        "read_file" => read_file(service, &request).await,
        "destroy_job" => destroy_job(service, &request).await,
        _ => anyhow::bail!("unsupported sandbox operation"),
    }
}

/// Probes every sandboxed tier the applied Manager document configures —
/// independent of the mode currently selected for any Runner — so a mode
/// switch never waits on a first probe and operators see why an unavailable
/// tier is unavailable. Without an applied document every tier is reported
/// unsupported with the reason.
pub async fn probe_capabilities(service: &Service) -> anyhow::Result<Value> {
    let mut capabilities = RunnerExecutionCapabilities::default();
    let checked_at = chrono::Utc::now().to_rfc3339();
    let Some(applied) = service.config.snapshot() else {
        for mode in RunnerExecutionMode::ALL
            .into_iter()
            .filter(|m| m.is_sandboxed())
        {
            capabilities.set_backend(
                mode,
                shared::protocol::ExecutionBackendStatus::Unsupported,
                Some("sandbox host has no applied configuration yet".into()),
            );
        }
        capabilities.checked_at = Some(checked_at);
        return Ok(json!({"ok": true, "capabilities": capabilities, "config_version": 0}));
    };
    let backends = applied.backends();
    for mode in RunnerExecutionMode::ALL
        .into_iter()
        .filter(|m| m.is_sandboxed())
    {
        let Some(descriptor) = backends.get(mode) else {
            continue;
        };
        let outcome = if !descriptor.is_configured() {
            Err(anyhow::anyhow!(
                "'{}' tier is not configured on this sandbox host",
                mode.as_str()
            ))
        } else {
            match validate_prerequisites(
                descriptor,
                &service.host.os,
                &service.host.kvm_path,
                &descriptor.oci_runtime,
            ) {
                Ok(()) => probe_backend(service, &applied, descriptor).await,
                Err(error) => Err(error),
            }
        };
        backend::record_probe(&mut capabilities, descriptor, outcome);
    }
    capabilities.checked_at = Some(checked_at);
    Ok(json!({
        "ok": true,
        "capabilities": capabilities,
        "config_version": applied.version,
        "config_sha256": applied.sha256,
    }))
}

/// `status`: cheap, 30 s-cadence view for the Runner's heartbeat block.
pub fn status(service: &Service) -> Value {
    let applied = service.config.snapshot();
    json!({
        "ok": true,
        "instance_id": service.bootstrap.instance_id,
        "host_id": service.bootstrap.host_id,
        "socket": service.bootstrap.socket,
        "workspace_root": service.bootstrap.workspace_root,
        "workspace_volume": service.bootstrap.workspace_volume,
        "allowed_runner_uid": service.bootstrap.allowed_runner_uid,
        "manager_url": service.bootstrap.manager_url,
        "configured": applied.is_some(),
        "config_version": applied.as_ref().map(|c| c.version).unwrap_or(0),
        "config_sha256": applied.as_ref().map(|c| c.sha256.clone()).unwrap_or_default(),
        "applied_at": applied.as_ref().map(|c| c.applied_at.clone()),
        "manager_instance_id": applied.as_ref().map(|c| c.manager_instance_id.clone()),
        "profiles": applied.as_ref().map(|c| c.document.profiles.clone()),
        "active_jobs": service.jobs.try_lock().map(|j| j.len()).unwrap_or(0),
    })
}

/// `get_config`: the full applied document (Runner writes its observational
/// `sandboxd.snapshot.json` from this only when the hash changes).
pub fn get_config(service: &Service) -> Value {
    match service.config.snapshot() {
        Some(applied) => json!({
            "ok": true,
            "configured": true,
            "version": applied.version,
            "sha256": applied.sha256,
            "applied_at": applied.applied_at,
            "manager_instance_id": applied.manager_instance_id,
            "config": shared::sandbox_config::canonical_json(&applied.document),
        }),
        None => json!({"ok": true, "configured": false, "version": 0}),
    }
}

/// Parameters for building a hardened `docker create` command for a sandbox
/// container (probe or job). The hardening flags (read-only root, dropped
/// capabilities, resource limits, …) are shared so probes exercise the same
/// container policy as real jobs.
struct SandboxCreateSpec<'a> {
    container: &'a str,
    image: &'a str,
    runtime: Option<&'a str>,
    user: &'a str,
    mount: Option<&'a str>,
    probe: bool,
    entrypoint_command: &'a str,
    /// Resource envelope for this container (profile resolved by the caller).
    limits: &'a ResourceProfile,
    network: &'a str,
}

fn sandbox_create_command(service: &Service, spec: SandboxCreateSpec<'_>) -> Command {
    let mut command = Command::new(&service.bootstrap.docker_cli);
    command.args(["create", "--name", spec.container]);
    if spec.probe {
        command.args(["--label", "buz.sandboxd.probe=true"]);
    }
    command.args([
        "--label",
        "buz.sandboxd.managed=true",
        "--label",
        &format!("buz.sandboxd.instance={}", service.bootstrap.instance_id),
        "--user",
        spec.user,
        "--workdir",
        "/buz/workspace",
    ]);
    if let Some(mount) = spec.mount {
        command.args(["--mount", mount]);
    }
    command.args([
        "--read-only",
        "--tmpfs",
        "/tmp:rw,nosuid,nodev,size=128m",
        "--cap-drop",
        "ALL",
        "--security-opt",
        "no-new-privileges",
        "--pids-limit",
        &spec.limits.pids.to_string(),
        "--memory",
        &spec.limits.memory,
        "--cpus",
        &spec.limits.cpu,
        "--network",
        spec.network,
    ]);
    if let Some(runtime) = spec.runtime {
        command.arg("--runtime").arg(runtime);
    }
    command.args([
        "--entrypoint",
        "sh",
        spec.image,
        "-c",
        spec.entrypoint_command,
    ]);
    command
}

pub async fn probe_backend(
    service: &Service,
    applied: &AppliedConfig,
    descriptor: &BackendDescriptor,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        descriptor.image.contains("@sha256:"),
        "sandbox image must be pinned by sha256 digest"
    );
    // Probes exercise the default envelope: a profile is a cgroup setting,
    // not a capability, so probing every profile would multiply cost for
    // no additional signal.
    let (_, limits) = applied.profile("")?;

    let probe_name = format!("buz-sandbox-probe-{}", uuid::Uuid::new_v4());
    let create_result = async {
        let mut create = sandbox_create_command(
            service,
            SandboxCreateSpec {
                container: &probe_name,
                image: &descriptor.image,
                runtime: descriptor.runtime(),
                user: "10001:10001",
                mount: None,
                probe: true,
                entrypoint_command: "sleep 30",
                limits,
                network: applied.network(),
            },
        );
        create
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let output = tokio::time::timeout(Duration::from_secs(60), create.output())
            .await
            .map_err(|_| anyhow::anyhow!("sandbox probe create timed out"))??;
        anyhow::ensure!(
            output.status.success(),
            "sandbox runtime create failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
        let mut start = Command::new(&service.bootstrap.docker_cli);
        start
            .args(["start", &probe_name])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let output = tokio::time::timeout(Duration::from_secs(30), start.output())
            .await
            .map_err(|_| anyhow::anyhow!("sandbox probe start timed out"))??;
        anyhow::ensure!(
            output.status.success(),
            "sandbox runtime start failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
        let mut exec = Command::new(&service.bootstrap.docker_cli);
        exec.args([
            "exec",
            &probe_name,
            "sh",
            "-c",
            "command -v timeout >/dev/null",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
        let output = tokio::time::timeout(Duration::from_secs(15), exec.output())
            .await
            .map_err(|_| anyhow::anyhow!("sandbox probe exec timed out"))??;
        anyhow::ensure!(
            output.status.success(),
            "sandbox image must provide sh and timeout: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
        Ok(())
    }
    .await;
    let cleanup_result = destroy_container(&service.bootstrap.docker_cli, &probe_name).await;
    create_result?;
    cleanup_result?;
    Ok(())
}

pub async fn create_job(
    service: &Service,
    request: &Value,
    uid: u32,
    gid: u32,
) -> anyhow::Result<Value> {
    // Fail closed without an applied Manager document: there is no built-in
    // default image or envelope to fall back to.
    let applied = service.config.require()?;
    let mode = RunnerExecutionMode::parse(
        request
            .get("mode")
            .and_then(Value::as_str)
            .unwrap_or_default(),
    )
    .filter(|mode| mode.is_sandboxed())
    .ok_or_else(|| anyhow::anyhow!("unsupported sandbox mode"))?;
    let backends = applied.backends();
    let descriptor = backends
        .get(mode)
        .ok_or_else(|| anyhow::anyhow!("no backend for '{}' tier", mode.as_str()))?;
    anyhow::ensure!(
        descriptor.is_configured(),
        "'{}' tier is not configured on this sandbox host",
        mode.as_str()
    );
    let workspace = request
        .get("workspace")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let workspace = std::fs::canonicalize(workspace)?;
    let root = Path::new(&service.bootstrap.workspace_root);
    anyhow::ensure!(
        workspace.starts_with(root),
        "job workspace is outside the configured workspace root"
    );
    anyhow::ensure!(workspace.is_dir(), "job workspace is not a directory");
    // The Runner names a profile; the envelope (and the image/runtime) come
    // from the applied document. An unknown profile is an error, never a
    // silent downgrade to `default`.
    let requested_profile = request
        .get("profile")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let (profile_name, limits) = applied.profile(requested_profile)?;
    // Re-check host prerequisites at job start: the host may have drifted
    // since the last capability probe.
    validate_prerequisites(
        descriptor,
        &service.host.os,
        &service.host.kvm_path,
        &descriptor.oci_runtime,
    )?;
    let runtime = descriptor.runtime();
    let image = descriptor.image.as_str();
    let handle = uuid::Uuid::new_v4().to_string();
    let container = format!("buz-sandbox-{handle}");
    let workspace_arg = if let Some(volume) = service.bootstrap.workspace_volume.as_deref() {
        let relative = workspace
            .strip_prefix(root)
            .map_err(|_| anyhow::anyhow!("job workspace escaped its volume root"))?;
        let relative = relative.to_string_lossy();
        let subpath = safe_work_dir(&relative)?;
        anyhow::ensure!(
            !subpath.is_empty(),
            "job workspace cannot be the volume root"
        );
        format!("type=volume,source={volume},dst=/buz/workspace,volume-subpath={subpath}")
    } else {
        format!("type=bind,src={},dst=/buz/workspace", workspace.display())
    };
    let user = format!("{uid}:{gid}");
    let mut command = sandbox_create_command(
        service,
        SandboxCreateSpec {
            container: &container,
            image,
            runtime,
            user: &user,
            mount: Some(&workspace_arg),
            probe: false,
            entrypoint_command: "while :; do sleep 3600; done",
            limits,
            network: applied.network(),
        },
    );
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let create_result = run_status(
        command,
        Duration::from_secs(120),
        "create sandbox container",
    )
    .await;
    if let Err(error) = create_result {
        let _ = destroy_container(&service.bootstrap.docker_cli, &container).await;
        return Err(error);
    }
    let mut start = Command::new(&service.bootstrap.docker_cli);
    start
        .args(["start", &container])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Err(error) = run_status(start, Duration::from_secs(60), "start sandbox container").await
    {
        let _ = destroy_container(&service.bootstrap.docker_cli, &container).await;
        return Err(error);
    }
    let mut verify_image = Command::new(&service.bootstrap.docker_cli);
    verify_image
        .args([
            "exec",
            &container,
            "sh",
            "-c",
            "command -v timeout >/dev/null",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Err(error) = run_status(
        verify_image,
        Duration::from_secs(10),
        "verify sandbox image",
    )
    .await
    {
        let _ = destroy_container(&service.bootstrap.docker_cli, &container).await;
        return Err(anyhow::anyhow!(
            "sandbox image must provide sh and the timeout utility: {error}"
        ));
    }
    service.jobs.lock().await.insert(
        handle.clone(),
        JobRuntime {
            container,
            workspace,
        },
    );
    Ok(json!({
        "ok": true,
        "handle": handle,
        "profile": profile_name,
        "limits": limits,
        "config_version": applied.version,
    }))
}

async fn kill_exec_and_container(
    child: &mut tokio::process::Child,
    container: &str,
    docker_cli: &str,
    out_reader: &tokio::task::JoinHandle<anyhow::Result<()>>,
    err_reader: &tokio::task::JoinHandle<anyhow::Result<()>>,
) {
    let _ = child.start_kill();
    let _ = child.wait().await;
    out_reader.abort();
    err_reader.abort();
    let _ = destroy_container(docker_cli, container).await;
}

pub async fn exec_step_stream(
    service: &Service,
    request: &Value,
    stream: UnixStream,
) -> anyhow::Result<()> {
    let (mut disconnect_reader, stream_writer) = stream.into_split();
    let stream_writer = Arc::new(tokio::sync::Mutex::new(stream_writer));
    let handle = request
        .get("handle")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let job = service
        .jobs
        .lock()
        .await
        .get(handle)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("unknown sandbox handle"))?;
    let command = request
        .get("command")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let work_dir = request
        .get("work_dir")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let work_dir = safe_work_dir(work_dir)?;
    let container_workdir = if work_dir.is_empty() {
        "/buz/workspace".to_string()
    } else {
        format!("/buz/workspace/{work_dir}")
    };
    let timeout_seconds = request
        .get("timeout_seconds")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .min(MAX_EXEC_SECONDS);
    let env = request
        .get("env")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();

    let mut child = Command::new(&service.bootstrap.docker_cli);
    child.args(["exec", "--workdir", &container_workdir]);
    for (key, value) in env {
        if !valid_env_key(&key) {
            continue;
        }
        if let Some(value) = value.as_str() {
            child.arg("--env").arg(format!("{key}={value}"));
        }
    }
    let exec_command = if timeout_seconds == 0 {
        shell_quote(command)
    } else {
        // The job image contract requires the `timeout` utility. Short flags
        // (`-s TERM -k 5`) are the only form both GNU coreutils and busybox
        // applets accept; long flags (`--signal`) break busybox-based images
        // such as Alpine. A timed-out step exits with 124 and leaves later
        // steps alive.
        format!(
            "timeout -s TERM -k 5 {}s sh -elc {}",
            timeout_seconds,
            shell_quote(command)
        )
    };
    child
        .arg(&job.container)
        .args(["sh", "-elc", &exec_command])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = child.spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("missing docker exec stdout"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow::anyhow!("missing docker exec stderr"))?;
    let (output_tx, mut output_rx) = tokio::sync::mpsc::channel::<(String, String)>(32);
    let out_tx = output_tx.clone();
    let out_reader = tokio::spawn(async move { forward_output(stdout, "stdout", out_tx).await });
    let err_reader = tokio::spawn(async move { forward_output(stderr, "stderr", output_tx).await });

    let deadline = (timeout_seconds > 0)
        .then(|| tokio::time::Instant::now() + Duration::from_secs(timeout_seconds + 10));
    let mut status = None;
    let mut output_closed = false;
    loop {
        if status.is_some() && output_closed {
            break;
        }
        tokio::select! {
            result = child.wait(), if status.is_none() => {
                status = Some(result?);
            }
            output = output_rx.recv(), if !output_closed => {
                match output {
                    Some((stream_name, content)) => {
                        let frame = json!({
                            "event": "output",
                            "stream": stream_name,
                            "content": content,
                        });
                        tokio::select! {
                            result = tokio::time::timeout(Duration::from_secs(30), async {
                                let mut writer = stream_writer.lock().await;
                                write_response(&mut *writer, frame).await
                            }) => {
                                match result {
                                    Ok(Ok(())) => {}
                                    Ok(Err(error)) => {
                                        kill_exec_and_container(&mut child, &job.container, &service.bootstrap.docker_cli, &out_reader, &err_reader).await;
                                        return Err(error);
                                    }
                                    Err(_) => {
                                        kill_exec_and_container(&mut child, &job.container, &service.bootstrap.docker_cli, &out_reader, &err_reader).await;
                                        return Err(anyhow::anyhow!("sandbox output write timed out; job container was removed"));
                                    }
                                }
                            }
                            _ = async { tokio::time::sleep_until(deadline.unwrap_or_else(|| tokio::time::Instant::now() + Duration::from_secs(MAX_EXEC_SECONDS + 10))).await }, if deadline.is_some() && status.is_none() => {
                                kill_exec_and_container(&mut child, &job.container, &service.bootstrap.docker_cli, &out_reader, &err_reader).await;
                                return Err(anyhow::anyhow!("sandbox exec watchdog exceeded the step timeout; job container was removed"));
                            }
                            _ = disconnect_reader.read_u8() => {
                                kill_exec_and_container(&mut child, &job.container, &service.bootstrap.docker_cli, &out_reader, &err_reader).await;
                                return Ok(());
                            }
                        }
                    }
                    None => output_closed = true,
                }
            }
            _ = async { tokio::time::sleep_until(deadline.unwrap_or_else(|| tokio::time::Instant::now() + Duration::from_secs(MAX_EXEC_SECONDS + 10))).await }, if deadline.is_some() && status.is_none() => {
                kill_exec_and_container(&mut child, &job.container, &service.bootstrap.docker_cli, &out_reader, &err_reader).await;
                return Err(anyhow::anyhow!("sandbox exec watchdog exceeded the step timeout; job container was removed"));
            }
            _result = disconnect_reader.read_u8() => {
                kill_exec_and_container(&mut child, &job.container, &service.bootstrap.docker_cli, &out_reader, &err_reader).await;
                return Ok(());
            }
        }
    }
    let _ = out_reader.await?;
    let _ = err_reader.await?;
    let status = status.expect("process completion observed");
    let code = status.code().unwrap_or(-1);
    let mut writer = stream_writer.lock().await;
    tokio::time::timeout(
        Duration::from_secs(30),
        write_response(
            &mut *writer,
            json!({
                "event": "exit",
                "code": code,
                "timed_out": timeout_seconds > 0 && matches!(code, 124 | 137),
            }),
        ),
    )
    .await
    .map_err(|_| anyhow::anyhow!("sandbox exit frame write timed out"))??;
    Ok(())
}

async fn forward_output<R: AsyncRead + Unpin>(
    mut reader: R,
    stream_name: &'static str,
    sender: tokio::sync::mpsc::Sender<(String, String)>,
) -> anyhow::Result<()> {
    let mut buffer = [0u8; 8192];
    loop {
        let count = reader.read(&mut buffer).await?;
        if count == 0 {
            return Ok(());
        }
        if sender
            .send((
                stream_name.to_string(),
                String::from_utf8_lossy(&buffer[..count]).into_owned(),
            ))
            .await
            .is_err()
        {
            return Ok(());
        }
    }
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

pub async fn read_file(service: &Service, request: &Value) -> anyhow::Result<Value> {
    let handle = request
        .get("handle")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let job = service
        .jobs
        .lock()
        .await
        .get(handle)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("unknown sandbox handle"))?;
    let relative = request
        .get("path")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let relative = safe_work_dir(relative)?;
    let path = std::fs::canonicalize(job.workspace.join(relative))?;
    anyhow::ensure!(
        path.starts_with(&job.workspace),
        "requested file is outside job workspace"
    );
    let bytes = std::fs::read(path)?;
    anyhow::ensure!(
        bytes.len() <= MAX_REQUEST_BYTES,
        "requested file exceeds size limit"
    );
    let content = String::from_utf8(bytes)?;
    Ok(json!({"ok": true,"content":content}))
}

pub async fn destroy_job(service: &Service, request: &Value) -> anyhow::Result<Value> {
    let handle = request
        .get("handle")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let job = service.jobs.lock().await.get(handle).cloned();
    let Some(job) = job else {
        return Ok(json!({"ok": true}));
    };
    destroy_container(&service.bootstrap.docker_cli, &job.container).await?;
    service.jobs.lock().await.remove(handle);
    Ok(json!({"ok": true}))
}

pub fn safe_work_dir(path: &str) -> anyhow::Result<String> {
    let path = Path::new(path);
    anyhow::ensure!(
        !path.is_absolute(),
        "sandbox work directory must be relative"
    );
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => parts.push(part.to_string_lossy().to_string()),
            Component::CurDir => {}
            _ => anyhow::bail!("sandbox path traversal is not allowed"),
        }
    }
    Ok(parts.join("/"))
}

fn valid_env_key(key: &str) -> bool {
    let mut chars = key.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

async fn run_status(
    mut command: Command,
    timeout: Duration,
    operation: &str,
) -> anyhow::Result<()> {
    command.kill_on_drop(true);
    let status = tokio::time::timeout(timeout, command.status())
        .await
        .map_err(|_| anyhow::anyhow!("{operation} timed out"))??;
    anyhow::ensure!(
        status.success(),
        "{operation} failed with status {}",
        status.code().unwrap_or(-1)
    );
    Ok(())
}

/// Force-removes a sandbox container. Idempotent: a container that is already
/// gone (or already stopped) is treated as success.
async fn destroy_container(docker: &str, container: &str) -> anyhow::Result<()> {
    let mut command = Command::new(docker);
    command
        .args(["rm", "--force", container])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let output = tokio::time::timeout(Duration::from_secs(30), command.output())
        .await
        .map_err(|_| anyhow::anyhow!("remove sandbox container timed out"))??;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    if stderr.contains("No such container") || stderr.contains("is not running") {
        return Ok(());
    }
    anyhow::bail!(
        "remove sandbox container failed with status {}: {}",
        output.status.code().unwrap_or(-1),
        stderr.trim()
    )
}
