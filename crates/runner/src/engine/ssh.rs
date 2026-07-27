use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use russh::client;
use russh::keys::load_secret_key;
use russh::ChannelMsg;
use tokio::time::timeout;

/// Result of an SSH command execution.
#[derive(Debug)]
pub struct SshOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: u32,
}

/// Connection parameters for an SSH remote host.
pub struct SshConnectionParams {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub credential_type: String,
    pub credential_ref: String,
}

/// Execute a command on a remote host via SSH.
pub async fn execute(
    command: &str,
    conn: &SshConnectionParams,
    cmd_timeout: Option<Duration>,
) -> anyhow::Result<SshOutput> {
    let default_timeout = cmd_timeout.unwrap_or(Duration::from_secs(300));

    let result = timeout(default_timeout, execute_inner(command, conn)).await;

    match result {
        Ok(inner) => inner,
        Err(_elapsed) => anyhow::bail!(
            "SSH command timed out after {}s",
            default_timeout.as_secs()
        ),
    }
}

async fn execute_inner(
    command: &str,
    conn: &SshConnectionParams,
) -> anyhow::Result<SshOutput> {
    let config = client::Config::default();
    let config = Arc::new(config);

    let addr = format!("{}:{}", conn.host, conn.port);

    tracing::info!(
        "SSH connecting to {}@{} (credential_type={})",
        conn.username,
        addr,
        conn.credential_type,
    );

    let mut handle = client::connect(config, &addr, NoAuthHandler)
        .await
        .map_err(|e| anyhow::anyhow!("SSH connection to {addr} failed: {e}"))?;

    let authenticated = match conn.credential_type.as_str() {
        "key_path" => {
            let key_pair = load_secret_key(&conn.credential_ref, None).map_err(|e| {
                anyhow::anyhow!(
                    "failed to load SSH key from {}: {e}",
                    conn.credential_ref
                )
            })?;
            handle
                .authenticate_publickey(&conn.username, Arc::new(key_pair))
                .await
                .map_err(|e| anyhow::anyhow!("SSH publickey auth failed: {e}"))?
        }
        "vault" => {
            // Phase 1: credential_ref is treated as a plaintext password.
            // Future: real Vault lookup replaces this.
            handle
                .authenticate_password(&conn.username, &conn.credential_ref)
                .await
                .map_err(|e| anyhow::anyhow!("SSH password auth failed: {e}"))?
        }
        "agent" => {
            anyhow::bail!("SSH agent authentication is not yet implemented");
        }
        other => anyhow::bail!("unsupported credential type: {other}"),
    };

    if !authenticated {
        anyhow::bail!(
            "SSH authentication rejected for {}@{}",
            conn.username,
            addr
        );
    }

    tracing::info!("SSH authenticated to {}@{}", conn.username, addr);

    // Open a session channel and exec the command
    let mut channel = handle
        .channel_open_session()
        .await
        .map_err(|e| anyhow::anyhow!("failed to open SSH session channel: {e}"))?;

    channel
        .exec(true, command)
        .await
        .map_err(|e| anyhow::anyhow!("SSH exec failed: {e}"))?;

    // Read stdout/stderr and exit status
    let mut stdout = String::new();
    let mut stderr = String::new();
    let mut exit_code: Option<u32> = None;

    loop {
        let Some(msg) = channel.wait().await else {
            break;
        };
        match msg {
            ChannelMsg::Data { ref data } => {
                stdout.push_str(&String::from_utf8_lossy(data));
            }
            ChannelMsg::ExtendedData { ref data, .. } => {
                stderr.push_str(&String::from_utf8_lossy(data));
            }
            ChannelMsg::ExitStatus {
                exit_status: status,
            } => {
                exit_code = Some(status);
                // Don't break immediately — more data may still arrive
            }
            ChannelMsg::Eof => {
                // Server indicates no more data
                break;
            }
            _ => {}
        }
    }

    Ok(SshOutput {
        stdout,
        stderr,
        exit_code: exit_code.unwrap_or(0),
    })
}

// Minimal handler that accepts any server key in Phase 1.
// Production hardening should add known_hosts verification.
struct NoAuthHandler;

#[async_trait]
impl client::Handler for NoAuthHandler {
    type Error = anyhow::Error;

    async fn check_server_key(
        &mut self,
        _server_public_key: &russh::keys::ssh_key::PublicKey,
    ) -> Result<bool, Self::Error> {
        // Phase 1: accept all keys
        Ok(true)
    }
}
