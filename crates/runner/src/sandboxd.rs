//! Host-side job sandbox supervisor.
//!
//! Run this service under an operator-managed account with access to the
//! selected container daemon/runtime. The Runner connects over a permissioned
//! Unix socket and never receives daemon, root, or KVM access itself.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::process::Command;
use tokio::sync::Mutex;

use shared::protocol::{ExecutionBackendStatus, RunnerExecutionCapabilities, RunnerExecutionMode};

const MAX_REQUEST_BYTES: usize = 1024 * 1024;
const MAX_EXEC_SECONDS: u64 = 24 * 60 * 60;

pub(super) async fn cleanup_stale_containers(config: &SandboxdConfig) -> anyhow::Result<()> {
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

#[derive(Debug, Clone, Deserialize)]
pub struct SandboxdConfig {
    #[serde(default = "default_instance_id")]
    pub instance_id: String,
    #[serde(default = "default_socket")]
    pub socket: String,
    #[serde(default = "default_workspace_root")]
    pub workspace_root: String,
    #[serde(default = "default_docker_cli")]
    pub docker_cli: String,
    #[serde(default)]
    pub container_image: String,
    #[serde(default)]
    pub firecracker_guest_image: String,
    #[serde(default)]
    pub firecracker_runtime: String,
    #[serde(default = "default_socket_gid")]
    pub socket_gid: u32,
    #[serde(default)]
    pub workspace_volume: Option<String>,
    #[serde(default)]
    pub allowed_runner_uid: Option<u32>,
    #[serde(default = "default_container_network")]
    pub container_network: String,
    #[serde(default = "default_memory_limit")]
    pub memory_limit: String,
    #[serde(default = "default_cpu_limit")]
    pub cpu_limit: String,
}

fn default_instance_id() -> String {
    "default".into()
}

fn default_socket() -> String {
    "/run/buz-sandboxd/sandboxd.sock".into()
}
fn default_workspace_root() -> String {
    "/var/lib/buz/workspaces".into()
}
fn default_docker_cli() -> String {
    "/usr/bin/docker".into()
}
fn default_socket_gid() -> u32 {
    10001
}
fn default_container_network() -> String {
    "bridge".into()
}
fn default_memory_limit() -> String {
    "2g".into()
}
fn default_cpu_limit() -> String {
    "2".into()
}

#[derive(Clone)]
pub(super) struct Service {
    pub(super) config: Arc<SandboxdConfig>,
    pub(super) jobs: Arc<Mutex<HashMap<String, JobRuntime>>>,
}

#[derive(Clone)]
pub(super) struct JobRuntime {
    container: String,
    workspace: PathBuf,
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
pub async fn serve(config: SandboxdConfig) -> anyhow::Result<()> {
    anyhow::ensure!(
        config.allowed_runner_uid.is_some(),
        "sandboxd allowed_runner_uid must be configured"
    );
    let workspace_root = Path::new(&config.workspace_root);
    std::fs::create_dir_all(workspace_root)?;
    let root = std::fs::canonicalize(workspace_root)?;
    anyhow::ensure!(
        !config.instance_id.trim().is_empty(),
        "sandboxd instance_id cannot be empty"
    );
    let socket = Path::new(&config.socket);
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
    cleanup_stale_containers(&config).await?;
    let listener = UnixListener::bind(socket)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{chown, PermissionsExt};
        if let Some(parent) = socket.parent() {
            chown(parent, Some(0), Some(config.socket_gid))?;
            std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o2770))?;
        }
        chown(socket, Some(0), Some(config.socket_gid))?;
        std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o660))?;
    }
    tracing::info!(socket = %config.socket, workspace_root = %root.display(), "sandbox supervisor listening");
    let service = Service {
        config: Arc::new(SandboxdConfig {
            workspace_root: root.to_string_lossy().into_owned(),
            ..config
        }),
        jobs: Arc::new(Mutex::new(HashMap::new())),
    };
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
        .config
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
        "create_job" => create_job(service, &request, uid, gid).await,
        "probe_capabilities" => probe_capabilities(service, &request).await,
        "read_file" => read_file(service, &request).await,
        "destroy_job" => destroy_job(service, &request).await,
        _ => anyhow::bail!("unsupported sandbox operation"),
    }
}

pub(super) fn validate_vm_prerequisites(
    os: &str,
    kvm_path: &Path,
    requested_runtime: &str,
    configured_runtime: &str,
) -> anyhow::Result<()> {
    anyhow::ensure!(os == "linux", "VM mode requires a Linux host");
    anyhow::ensure!(
        kvm_path.exists(),
        "VM mode requires an available /dev/kvm device"
    );
    anyhow::ensure!(
        !requested_runtime.trim().is_empty() && requested_runtime == configured_runtime,
        "configured Firecracker-compatible OCI runtime does not match sandboxd policy"
    );
    Ok(())
}

pub(super) async fn probe_capabilities(
    service: &Service,
    request: &Value,
) -> anyhow::Result<Value> {
    let container_image = request
        .get("container_image")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let vm_image = request
        .get("vm_image")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let requested_runtime = request
        .get("firecracker_runtime")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let mut capabilities = RunnerExecutionCapabilities::default();
    let checked_at = chrono::Utc::now().to_rfc3339();

    match probe_backend(service, RunnerExecutionMode::Container, container_image, "").await {
        Ok(()) => capabilities.container = ExecutionBackendStatus::Supported,
        Err(error) => {
            capabilities.container = ExecutionBackendStatus::Unsupported;
            capabilities.container_reason = Some(error.to_string());
        }
    }

    let vm_result = async {
        validate_vm_prerequisites(
            std::env::consts::OS,
            Path::new("/dev/kvm"),
            requested_runtime,
            &service.config.firecracker_runtime,
        )?;
        probe_backend(
            service,
            RunnerExecutionMode::Vm,
            vm_image,
            requested_runtime,
        )
        .await
    }
    .await;
    match vm_result {
        Ok(()) => capabilities.vm = ExecutionBackendStatus::Supported,
        Err(error) => {
            capabilities.vm = ExecutionBackendStatus::Unsupported;
            capabilities.vm_reason = Some(error.to_string());
        }
    }
    capabilities.checked_at = Some(checked_at);
    Ok(json!({"ok": true, "capabilities": capabilities}))
}

pub(super) async fn probe_backend(
    service: &Service,
    mode: RunnerExecutionMode,
    image: &str,
    runtime: &str,
) -> anyhow::Result<()> {
    let expected = match mode {
        RunnerExecutionMode::Container => &service.config.container_image,
        RunnerExecutionMode::Vm => &service.config.firecracker_guest_image,
        RunnerExecutionMode::Host => anyhow::bail!("Host mode does not use a sandbox backend"),
    };
    anyhow::ensure!(
        !expected.trim().is_empty(),
        "sandbox image is not configured"
    );
    anyhow::ensure!(
        image == expected,
        "sandbox image does not match sandboxd configuration"
    );
    anyhow::ensure!(
        image.contains("@sha256:"),
        "sandbox image must be pinned by sha256 digest"
    );

    let probe_name = format!("buz-sandbox-probe-{}", uuid::Uuid::new_v4());
    let create_result = async {
        let mut create = Command::new(&service.config.docker_cli);
        create.args([
            "create",
            "--name",
            &probe_name,
            "--label",
            "buz.sandboxd.probe=true",
            "--label",
            "buz.sandboxd.managed=true",
            "--label",
            &format!("buz.sandboxd.instance={}", service.config.instance_id),
            "--user",
            "10001:10001",
            "--workdir",
            "/buz/workspace",
            "--read-only",
            "--tmpfs",
            "/tmp:rw,nosuid,nodev,size=128m",
            "--cap-drop",
            "ALL",
            "--security-opt",
            "no-new-privileges",
            "--pids-limit",
            "256",
            "--memory",
            &service.config.memory_limit,
            "--cpus",
            &service.config.cpu_limit,
            "--network",
            &service.config.container_network,
        ]);
        if mode == RunnerExecutionMode::Vm {
            create.arg("--runtime").arg(runtime);
        }
        create
            .args(["--entrypoint", "sh", image, "-c", "sleep 30"])
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
        let mut start = Command::new(&service.config.docker_cli);
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
        let mut exec = Command::new(&service.config.docker_cli);
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
    let cleanup_result = remove_container(&service.config.docker_cli, &probe_name).await;
    create_result?;
    cleanup_result?;
    Ok(())
}

async fn create_job(
    service: &Service,
    request: &Value,
    uid: u32,
    gid: u32,
) -> anyhow::Result<Value> {
    let mode = match request.get("mode").and_then(Value::as_str) {
        Some("container") => RunnerExecutionMode::Container,
        Some("vm") => RunnerExecutionMode::Vm,
        _ => anyhow::bail!("unsupported sandbox mode"),
    };
    let workspace = request
        .get("workspace")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let workspace = std::fs::canonicalize(workspace)?;
    let root = Path::new(&service.config.workspace_root);
    anyhow::ensure!(
        workspace.starts_with(root),
        "job workspace is outside the configured workspace root"
    );
    anyhow::ensure!(workspace.is_dir(), "job workspace is not a directory");
    let requested_image = request
        .get("image")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let allowed_image = match mode {
        RunnerExecutionMode::Container => &service.config.container_image,
        RunnerExecutionMode::Vm => &service.config.firecracker_guest_image,
        RunnerExecutionMode::Host => unreachable!(),
    };
    anyhow::ensure!(
        requested_image == allowed_image,
        "requested sandbox image does not match host policy"
    );
    let runtime = if mode == RunnerExecutionMode::Vm {
        let value = service.config.firecracker_runtime.trim();
        anyhow::ensure!(
            !value.is_empty(),
            "Firecracker runtime is not configured on sandboxd"
        );
        Some(value)
    } else {
        None
    };
    anyhow::ensure!(
        !service.config.container_network.trim().is_empty(),
        "sandbox network policy is empty"
    );
    anyhow::ensure!(
        !service.config.memory_limit.trim().is_empty(),
        "sandbox memory_limit is empty"
    );
    anyhow::ensure!(
        !service.config.cpu_limit.trim().is_empty(),
        "sandbox cpu_limit is empty"
    );
    let handle = uuid::Uuid::new_v4().to_string();
    let container = format!("buz-sandbox-{handle}");
    let workspace_arg = if let Some(volume) = service.config.workspace_volume.as_deref() {
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
    let mut command = Command::new(&service.config.docker_cli);
    command.args([
        "create",
        "--name",
        &container,
        "--label",
        "buz.sandboxd.managed=true",
        "--label",
        &format!("buz.sandboxd.instance={}", service.config.instance_id),
        "--user",
        &user,
        "--workdir",
        "/buz/workspace",
        "--mount",
        &workspace_arg,
        "--read-only",
        "--tmpfs",
        "/tmp:rw,nosuid,nodev,size=128m",
        "--cap-drop",
        "ALL",
        "--security-opt",
        "no-new-privileges",
        "--pids-limit",
        "256",
        "--memory",
        &service.config.memory_limit,
        "--cpus",
        &service.config.cpu_limit,
        "--network",
        &service.config.container_network,
    ]);
    if let Some(runtime) = runtime {
        command.arg("--runtime").arg(runtime);
    }
    command
        .args([
            "--entrypoint",
            "sh",
            requested_image,
            "-c",
            "while :; do sleep 3600; done",
        ])
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
        let _ = remove_container(&service.config.docker_cli, &container).await;
        return Err(error);
    }
    let mut start = Command::new(&service.config.docker_cli);
    start
        .args(["start", &container])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Err(error) = run_status(start, Duration::from_secs(60), "start sandbox container").await
    {
        let _ = remove_container(&service.config.docker_cli, &container).await;
        return Err(error);
    }
    let mut verify_image = Command::new(&service.config.docker_cli);
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
        let _ = remove_container(&service.config.docker_cli, &container).await;
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
    Ok(json!({"ok": true, "handle": handle}))
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
    let _ = kill_container(docker_cli, container).await;
}

async fn exec_step_stream(
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

    let mut child = Command::new(&service.config.docker_cli);
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
        // The job image contract requires the POSIX `timeout` utility. A
        // timed-out step exits normally with 124 and leaves later steps alive.
        format!(
            "timeout --signal=TERM --kill-after=5s {}s sh -elc {}",
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
                                        kill_exec_and_container(&mut child, &job.container, &service.config.docker_cli, &out_reader, &err_reader).await;
                                        return Err(error);
                                    }
                                    Err(_) => {
                                        kill_exec_and_container(&mut child, &job.container, &service.config.docker_cli, &out_reader, &err_reader).await;
                                        return Err(anyhow::anyhow!("sandbox output write timed out; job container was removed"));
                                    }
                                }
                            }
                            _ = async { tokio::time::sleep_until(deadline.unwrap_or_else(|| tokio::time::Instant::now() + Duration::from_secs(MAX_EXEC_SECONDS + 10))).await }, if deadline.is_some() && status.is_none() => {
                                kill_exec_and_container(&mut child, &job.container, &service.config.docker_cli, &out_reader, &err_reader).await;
                                return Err(anyhow::anyhow!("sandbox exec watchdog exceeded the step timeout; job container was removed"));
                            }
                            _ = disconnect_reader.read_u8() => {
                                kill_exec_and_container(&mut child, &job.container, &service.config.docker_cli, &out_reader, &err_reader).await;
                                return Ok(());
                            }
                        }
                    }
                    None => output_closed = true,
                }
            }
            _ = async { tokio::time::sleep_until(deadline.unwrap_or_else(|| tokio::time::Instant::now() + Duration::from_secs(MAX_EXEC_SECONDS + 10))).await }, if deadline.is_some() && status.is_none() => {
                kill_exec_and_container(&mut child, &job.container, &service.config.docker_cli, &out_reader, &err_reader).await;
                return Err(anyhow::anyhow!("sandbox exec watchdog exceeded the step timeout; job container was removed"));
            }
            _result = disconnect_reader.read_u8() => {
                kill_exec_and_container(&mut child, &job.container, &service.config.docker_cli, &out_reader, &err_reader).await;
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

async fn read_file(service: &Service, request: &Value) -> anyhow::Result<Value> {
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

async fn destroy_job(service: &Service, request: &Value) -> anyhow::Result<Value> {
    let handle = request
        .get("handle")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let job = service.jobs.lock().await.get(handle).cloned();
    let Some(job) = job else {
        return Ok(json!({"ok": true}));
    };
    remove_container(&service.config.docker_cli, &job.container).await?;
    service.jobs.lock().await.remove(handle);
    Ok(json!({"ok": true}))
}

fn safe_work_dir(path: &str) -> anyhow::Result<String> {
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

async fn kill_container(docker: &str, container: &str) -> anyhow::Result<()> {
    let mut command = Command::new(docker);
    command
        .args(["rm", "--force", container])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    run_status(command, Duration::from_secs(30), "kill sandbox container").await
}

async fn remove_container(docker: &str, container: &str) -> anyhow::Result<()> {
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

#[cfg(all(test, unix))]
#[path = "sandboxd_tests.rs"]
mod tests;
