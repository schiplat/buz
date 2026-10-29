//! Runner-side client for the host-side sandbox supervisor (`buz-sandboxd`).
//!
//! This is the **client half** of the protocol; the server half lives in
//! `crate::sandbox` (shipped as the `buz-sandboxd` binary). The Runner remains
//! the control plane and never receives KVM/root access: a privileged host
//! service owns job containers/VMs behind a permissioned Unix socket. The wire
//! format is newline-delimited JSON, one request/response per connection so a
//! dropped request is scoped to one operation.

use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::time::Duration;

use crate::config::{Config, SandboxSection};
use shared::protocol::RunnerExecutionMode;

/// Runner → supervisor wire contract (newline-delimited JSON). Public so the
/// crate's integration tests can pin the exact shape the supervisor parses.
///
/// The Runner names a **tier** and a **resource profile**; the image, OCI
/// runtime, and envelope come from the sandboxd's applied Manager document.
/// The Runner never carries images, so the two halves cannot disagree.
#[derive(Debug, Serialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request<'a> {
    CreateJob {
        job_id: &'a str,
        mode: RunnerExecutionMode,
        workspace: &'a str,
        /// Resource profile name; empty selects `default`.
        profile: &'a str,
    },
    ExecStep {
        handle: &'a str,
        command: &'a str,
        work_dir: &'a str,
        env: &'a Value,
        timeout_seconds: u64,
    },
    ReadFile {
        handle: &'a str,
        path: &'a str,
    },
    DestroyJob {
        handle: &'a str,
    },
    /// Probe every tier in the supervisor's applied document.
    ProbeCapabilities {},
    /// Cheap applied-config/version view (30 s heartbeat cadence).
    Status {},
    /// Full applied document (fetched only when the hash changes).
    GetConfig {},
}

/// Supervisor → Runner wire contract.
#[derive(Debug, Deserialize)]
pub struct Response {
    pub ok: bool,
    #[serde(default)]
    pub handle: String,
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub error: String,
    #[serde(default)]
    pub capabilities: Option<shared::protocol::RunnerExecutionCapabilities>,
    /// Applied Manager document version (probe/status/get_config/create_job).
    #[serde(default)]
    pub config_version: i64,
    /// Effective profile name for a created job.
    #[serde(default)]
    pub profile: String,
    /// Everything else (status block, get_config document) — kept raw so the
    /// Runner forwards it without re-modelling every field.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

#[derive(Debug, Clone)]
pub struct SandboxHandle {
    id: String,
    mode: RunnerExecutionMode,
    profile: String,
    config_version: i64,
}

impl SandboxHandle {
    pub fn mode(&self) -> RunnerExecutionMode {
        self.mode
    }

    /// Effective resource profile the supervisor applied to this job.
    pub fn profile(&self) -> &str {
        &self.profile
    }

    /// Manager document version the job container was created under.
    pub fn config_version(&self) -> i64 {
        self.config_version
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum SandboxExecEvent {
    Output {
        #[serde(rename = "stream")]
        _stream: String,
        content: String,
    },
    Exit {
        code: i32,
        timed_out: bool,
    },
    Error {
        error: String,
    },
}

pub struct SandboxExecStream {
    receiver: tokio::sync::mpsc::Receiver<anyhow::Result<SandboxExecEvent>>,
    writer: Option<tokio::net::unix::OwnedWriteHalf>,
    reader: tokio::task::JoinHandle<()>,
}

impl SandboxExecStream {
    pub async fn recv(&mut self) -> Option<anyhow::Result<SandboxExecEvent>> {
        self.receiver.recv().await
    }

    pub fn abort(&mut self) {
        self.reader.abort();
        self.writer.take();
    }
}

pub async fn create_job(
    config: &Config,
    job_id: &str,
    workspace: &str,
    profile: &str,
) -> anyhow::Result<SandboxHandle> {
    let mode = config.execution_mode;
    anyhow::ensure!(mode.is_sandboxed(), "sandbox create requested in Host mode");
    let workspace_root =
        std::fs::canonicalize(&config.sandbox.workspace_root).map_err(|error| {
            anyhow::anyhow!(
                "sandbox workspace root '{}' is unavailable: {error}",
                config.sandbox.workspace_root
            )
        })?;
    let workspace_path = std::fs::canonicalize(workspace)?;
    anyhow::ensure!(
        workspace_path.starts_with(&workspace_root),
        "job workspace is outside configured sandbox workspace_root"
    );
    let workspace = workspace_path.to_string_lossy().into_owned();
    let response = request(
        &config.sandbox,
        Request::CreateJob {
            job_id,
            mode,
            workspace: &workspace,
            profile,
        },
    )
    .await?;
    anyhow::ensure!(
        !response.handle.is_empty(),
        "sandbox supervisor returned an empty job handle"
    );
    Ok(SandboxHandle {
        id: response.handle,
        mode,
        profile: response.profile,
        config_version: response.config_version,
    })
}

pub async fn exec_step(
    config: &SandboxSection,
    handle: &SandboxHandle,
    command: &str,
    work_dir: &str,
    env: &Value,
    timeout_seconds: u64,
) -> anyhow::Result<SandboxExecStream> {
    #[cfg(not(unix))]
    anyhow::bail!("sandbox supervisor Unix socket is only supported on Unix hosts");

    #[cfg(unix)]
    {
        let socket = Path::new(&config.supervisor_socket);
        let mut stream = tokio::time::timeout(
            Duration::from_secs(config.request_timeout_seconds.max(1)),
            UnixStream::connect(socket),
        )
        .await
        .map_err(|_| anyhow::anyhow!("sandbox supervisor connect timed out"))??;
        let request = Request::ExecStep {
            handle: &handle.id,
            command,
            work_dir,
            env,
            timeout_seconds,
        };
        let mut payload = serde_json::to_vec(&request)?;
        payload.push(b'\n');
        stream.write_all(&payload).await?;
        let (reader, writer) = stream.into_split();
        let (sender, receiver) = tokio::sync::mpsc::channel(32);
        let reader = tokio::spawn(async move {
            let mut reader = BufReader::new(reader);
            loop {
                let mut line = String::new();
                match reader.read_line(&mut line).await {
                    Ok(0) => break,
                    Ok(_) => match serde_json::from_str::<SandboxExecEvent>(&line) {
                        Ok(event) => {
                            let finished = matches!(
                                event,
                                SandboxExecEvent::Exit { .. } | SandboxExecEvent::Error { .. }
                            );
                            if sender.send(Ok(event)).await.is_err() || finished {
                                return;
                            }
                        }
                        Err(error) => {
                            let _ = sender.send(Err(error.into())).await;
                            return;
                        }
                    },
                    Err(error) => {
                        let _ = sender.send(Err(error.into())).await;
                        return;
                    }
                }
            }
            let _ = sender
                .send(Err(anyhow::anyhow!(
                    "sandbox supervisor closed exec stream before exit"
                )))
                .await;
        });
        Ok(SandboxExecStream {
            receiver,
            writer: Some(writer),
            reader,
        })
    }
}

/// Asks the supervisor to exercise every tier this Runner has configured.
pub async fn probe_capabilities(
    config: &SandboxSection,
) -> anyhow::Result<shared::protocol::RunnerExecutionCapabilities> {
    let response = request(config, Request::ProbeCapabilities {}).await?;
    response
        .capabilities
        .ok_or_else(|| anyhow::anyhow!("sandbox supervisor omitted execution capabilities"))
}

/// Supervisor status block (identity, applied version/hash, profiles, load).
pub async fn status(config: &SandboxSection) -> anyhow::Result<Value> {
    let response = request(config, Request::Status {}).await?;
    Ok(Value::Object(response.extra))
}

/// Full applied document plus version/hash metadata.
pub async fn get_config(config: &SandboxSection) -> anyhow::Result<Value> {
    let response = request(config, Request::GetConfig {}).await?;
    Ok(Value::Object(response.extra))
}

pub async fn read_job_file(
    config: &SandboxSection,
    handle: &SandboxHandle,
    path: &str,
) -> anyhow::Result<String> {
    let response = request(
        config,
        Request::ReadFile {
            handle: &handle.id,
            path,
        },
    )
    .await?;
    Ok(response.content)
}

pub async fn destroy_job(config: &SandboxSection, handle: &SandboxHandle) -> anyhow::Result<()> {
    let response = request(config, Request::DestroyJob { handle: &handle.id }).await?;
    anyhow::ensure!(response.ok, "sandbox supervisor failed to destroy job");
    Ok(())
}

async fn request(config: &SandboxSection, request: Request<'_>) -> anyhow::Result<Response> {
    request_with_timeout(
        config,
        request,
        Duration::from_secs(config.request_timeout_seconds.max(1)),
    )
    .await
}

async fn request_with_timeout(
    config: &SandboxSection,
    request: Request<'_>,
    timeout: Duration,
) -> anyhow::Result<Response> {
    #[cfg(not(unix))]
    anyhow::bail!("sandbox supervisor Unix socket is only supported on Unix hosts");

    #[cfg(unix)]
    {
        let socket = Path::new(&config.supervisor_socket);
        let mut stream = tokio::time::timeout(timeout, UnixStream::connect(socket))
            .await
            .map_err(|_| anyhow::anyhow!("sandbox supervisor connect timed out"))??;
        let mut payload = serde_json::to_vec(&request)?;
        payload.push(b'\n');
        stream.write_all(&payload).await?;
        stream.shutdown().await?;
        let mut line = String::new();
        tokio::time::timeout(timeout, BufReader::new(stream).read_line(&mut line))
            .await
            .map_err(|_| anyhow::anyhow!("sandbox supervisor request timed out"))??;
        anyhow::ensure!(
            !line.is_empty(),
            "sandbox supervisor closed without a response"
        );
        let response: Response = serde_json::from_str(&line)?;
        anyhow::ensure!(response.ok, "sandbox supervisor: {}", response.error);
        Ok(response)
    }
}
