use std::sync::Arc;
use std::time::Duration;

use crate::config::SshHostKeyPolicy;
use russh::client;
use russh::client::AuthResult;
use russh::keys::agent::client::AgentClient;
use russh::keys::agent::AgentIdentity;
use russh::keys::check_known_hosts_path;
use russh::keys::key::PrivateKeyWithHashAlg;
use russh::keys::load_secret_key;
use russh::keys::ssh_key;
use russh::keys::HashAlg;
use russh::ChannelMsg;
use tokio::io::{AsyncRead, AsyncWrite};
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
    /// Resolved credential value (private key or password).
    /// - For "vault" type: used as the password directly.
    /// - For "key_path" type: written to a temp file and used as the key path.
    /// - For "agent" type: unused (auth uses the runner's SSH agent via `SSH_AUTH_SOCK`).
    pub credential_value: Option<String>,
    /// Optional passphrase for encrypted private keys.
    pub credential_passphrase: Option<String>,
    /// Host-key verification policy (from runner config).
    pub host_key_policy: crate::config::SshHostKeyPolicy,
    /// Optional path to known_hosts (None → ~/.ssh/known_hosts when policy requires it).
    pub known_hosts_path: Option<String>,
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
        Err(_elapsed) => {
            anyhow::bail!("SSH command timed out after {}s", default_timeout.as_secs())
        }
    }
}

async fn execute_inner(command: &str, conn: &SshConnectionParams) -> anyhow::Result<SshOutput> {
    let config = client::Config::default();
    let config = Arc::new(config);

    let addr = format!("{}:{}", conn.host, conn.port);

    tracing::info!(
        "SSH connecting to {}@{} (credential_type={}, credential_value_len={})",
        conn.username,
        addr,
        conn.credential_type,
        conn.credential_value.as_ref().map(|v| v.len()).unwrap_or(0),
    );

    let mut handle = client::connect(
        config,
        &addr,
        HostKeyHandler {
            host: conn.host.clone(),
            port: conn.port,
            policy: conn.host_key_policy.clone(),
            known_hosts_path: conn.known_hosts_path.clone(),
        },
    )
    .await
    .map_err(|e| anyhow::anyhow!("SSH connection to {addr} failed: {e}"))?;

    let authenticated = match conn.credential_type.as_str() {
        "key_path" => {
            let key_data = conn.credential_value.as_deref().ok_or_else(|| {
                anyhow::anyhow!(
                    "SSH step requires a key, but no credential is configured for host '{}'.\n\
                     → Go to Manager UI → Infrastructure → Hosts → edit this host → paste your private key in the textarea.",
                    conn.host
                )
            })?;

            // Sanity-check: key should look like a valid PEM/OpenSSH key
            let first_50 = if key_data.len() > 50 { &key_data[..50] } else { key_data };
            tracing::info!("SSH key for {}@{}: first 50 chars = {:?}", conn.username, addr, first_50);

            let tmp_path = std::env::temp_dir()
                .join(format!("buz_ssh_key_{}", uuid::Uuid::new_v4()));
            std::fs::write(&tmp_path, key_data).map_err(|e| {
                anyhow::anyhow!("failed to write SSH key to temp file {}: {e}", tmp_path.display())
            })?;
            let path_str = tmp_path.to_string_lossy().to_string();
            let path_for_cleanup = tmp_path.clone();
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                let _ = std::fs::remove_file(path_for_cleanup);
            });
            let key_pair = load_secret_key(
                &path_str,
                conn.credential_passphrase.as_deref(),
            ).map_err(|e| {
                anyhow::anyhow!(
                    "failed to parse SSH private key for host '{}': {e}\n\
                     → Make sure you pasted a valid private key (not a public key) in the Host form.\n\
                     → If your key requires a passphrase, enter it in the 'Key Passphrase' field.",
                    conn.host
                )
            })?;

            // Log key type and fingerprint for diagnostics
            let key_alg = key_pair.algorithm();
            let key_type_name = key_type_display(&key_alg);
            tracing::info!(
                "SSH key loaded — type={key_type_name}, SHA256 fingerprint: {}",
                key_pair.fingerprint(HashAlg::Sha256)
            );

            authenticate_with_private_key(
                &mut handle,
                &conn.username,
                &key_pair,
                &addr,
            ).await.map_err(|e| anyhow::anyhow!(
                "SSH key authentication rejected for {}@{}: {e}\n\
                 → Key type: {key_type_name}\n\
                 → The key was loaded but the server refused it.\n\
                 → Check that the public key matches ~/.ssh/authorized_keys on the target server.\n\
                 → On the target server, run: cat ~/.ssh/authorized_keys",
                conn.username, addr
            ))?
        }
        "vault" => {
            let password = conn.credential_value.as_deref().ok_or_else(|| {
                anyhow::anyhow!(
                    "SSH step requires a password, but no credential is configured for host '{}'.\n\
                     → Go to Manager UI → Infrastructure → Hosts → edit this host → paste the password in the textarea.",
                    conn.host
                )
            })?;
            handle
                .authenticate_password(&conn.username, password)
                .await
                .map_err(|e| anyhow::anyhow!(
                    "SSH password authentication rejected for {}@{}: {e}\n\
                     → The password was provided but the server refused it. Check the password or try key-based auth.",
                    conn.username, addr
                ))?
        }
        "agent" => {
            authenticate_with_agent(&mut handle, &conn.username, &addr)
                .await
                .map_err(|e| {
                    anyhow::anyhow!(
                        "SSH agent authentication failed for {}@{}: {e}\n\
                         → Ensure the runner process has SSH_AUTH_SOCK set and keys loaded (ssh-add -l).\n\
                         → On the target server, confirm the matching public key is in ~/.ssh/authorized_keys.",
                        conn.username, addr
                    )
                })?
        }
        other => anyhow::bail!("unsupported credential type: {other}"),
    };

    if !authenticated.success() {
        anyhow::bail!(
            "SSH server rejected authentication for {}@{} — credential type '{}' was not accepted by the remote host.",
            conn.username,
            addr,
            conn.credential_type
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

    let exit_code = exit_code.unwrap_or_else(|| {
        tracing::warn!(
            "SSH channel for {addr} closed without an exit status — treating as failure"
        );
        1
    });

    Ok(SshOutput {
        stdout,
        stderr,
        exit_code,
    })
}

/// Human-readable name for a key algorithm.
fn key_type_display(alg: &ssh_key::Algorithm) -> &'static str {
    let cloned = alg.clone();
    if cloned.is_rsa() {
        return "RSA";
    }
    let cloned = alg.clone();
    if cloned.is_ed25519() {
        return "Ed25519";
    }
    let cloned = alg.clone();
    if cloned.is_ecdsa() {
        return "ECDSA";
    }
    "unknown"
}

/// Generic private-key authentication that works for any key type.
///
/// # RSA fallback chain
///
/// RSA keys need a signature hash algorithm (`rsa-sha2-512`, `rsa-sha2-256`,
/// or legacy `ssh-rsa`). This function negotiates the best one and falls back
/// if the server rejects it:
///
/// 1. Server-preferred hash (via `server-sig-algs` extension, RFC 8308)
/// 2. `rsa-sha2-512`
/// 3. `rsa-sha2-256`
/// 4. Legacy `ssh-rsa` (SHA-1, last resort)
///
/// # Non-RSA keys
///
/// Ed25519 and ECDSA keys don't involve hash negotiation — authentication
/// is attempted once directly.
async fn authenticate_with_private_key(
    handle: &mut russh::client::Handle<HostKeyHandler>,
    username: &str,
    key: &russh::keys::PrivateKey,
    addr: &str,
) -> anyhow::Result<AuthResult> {
    let alg = key.algorithm();
    let is_rsa = alg.clone().is_rsa();

    if !is_rsa {
        // Ed25519, ECDSA, etc. — no hash negotiation needed
        tracing::info!(
            "SSH authenticating with {key_type} (no hash negotiation)",
            key_type = key_type_display(&alg),
        );
        let key_with_hash = PrivateKeyWithHashAlg::new(Arc::new(key.clone()), None);
        return handle
            .authenticate_publickey(username, key_with_hash)
            .await
            .map_err(|e| {
                anyhow::anyhow!(
                    "{key_type} authentication failed: {e}",
                    key_type = key_type_display(&alg)
                )
            });
    }

    // ---- RSA: build a fallback hash chain ----

    // 1. Ask the server which hash it prefers (RFC 8308)
    let server_pref = handle.best_supported_rsa_hash().await;
    let preferred = match &server_pref {
        Ok(Some(Some(hash))) => {
            tracing::info!("SSH server prefers {:?} for RSA signatures", hash);
            Some(*hash)
        }
        Ok(Some(None)) => {
            tracing::warn!("SSH server explicitly does NOT support rsa-sha2-*; will try anyway then fall back to ssh-rsa");
            None
        }
        Ok(None) | Err(_) => {
            tracing::info!(
                "SSH server did not advertise server-sig-algs, will try all RSA hash types"
            );
            None
        }
    };

    // 2. Build a deduplicated fallback chain
    let fallback_chain = build_rsa_fallback_chain(preferred);

    // 3. Try each candidate in order
    for (attempt, hash_alg) in fallback_chain.iter().enumerate() {
        let label = match hash_alg {
            Some(HashAlg::Sha512) => "rsa-sha2-512",
            Some(HashAlg::Sha256) => "rsa-sha2-256",
            None => "ssh-rsa (SHA-1, legacy)",
            _ => "other",
        };

        tracing::info!("SSH RSA auth attempt {}: trying {}", attempt + 1, label,);

        let key_with_hash = PrivateKeyWithHashAlg::new(Arc::new(key.clone()), *hash_alg);

        match handle.authenticate_publickey(username, key_with_hash).await {
            Ok(result) if result.success() => {
                tracing::info!("SSH RSA auth succeeded with {label}");
                return Ok(result);
            }
            Ok(result) => {
                // partial_success or explicit failure — server may allow another attempt
                tracing::debug!("SSH RSA auth {label} returned {:?}, trying next", result);
            }
            Err(e) => {
                tracing::debug!("SSH RSA auth {label} failed with error: {e}, trying next");
            }
        }
    }

    // All candidates exhausted
    anyhow::bail!(
        "all RSA signature algorithms were rejected by {addr}.\n\
         → Tried: {chain}\n\
         → The server may have disabled RSA authentication entirely.\n\
         → Check /etc/ssh/sshd_config on the target: ensure PubkeyAcceptedKeyTypes includes rsa-sha2-512 and/or rsa-sha2-256.\n\
         → Alternatively, use an Ed25519 key (recommended): ssh-keygen -t ed25519",
        chain = fallback_chain.iter()
            .map(hash_label)
            .collect::<Vec<_>>()
            .join(" → ")
    )
}

/// Build a deduplicated fallback chain for RSA hash algorithms.
///
/// If the server gave a preferred hash, that goes first.
/// Then we try Sha512, Sha256, and finally None (legacy ssh-rsa).
fn build_rsa_fallback_chain(preferred: Option<HashAlg>) -> Vec<Option<HashAlg>> {
    let all: &[Option<HashAlg>] = &[
        Some(HashAlg::Sha512),
        Some(HashAlg::Sha256),
        None, // legacy ssh-rsa
    ];

    let mut chain: Vec<Option<HashAlg>> = Vec::with_capacity(4);

    // Server-preferred goes first
    if let Some(pref) = preferred {
        chain.push(Some(pref));
    }

    // Append remaining candidates, skipping duplicates
    for candidate in all {
        if !chain.contains(candidate) {
            chain.push(*candidate);
        }
    }

    chain
}

/// Human-readable label for an RSA hash algorithm.
fn hash_label(hash_alg: &Option<HashAlg>) -> &'static str {
    match hash_alg {
        Some(HashAlg::Sha512) => "rsa-sha2-512",
        Some(HashAlg::Sha256) => "rsa-sha2-256",
        Some(_) => "other",
        None => "ssh-rsa",
    }
}

/// Label for an SSH agent identity in logs / errors.
fn format_agent_identity_label(index: usize, comment: &str) -> String {
    if comment.is_empty() {
        format!("#{}", index + 1)
    } else {
        comment.to_string()
    }
}

/// Authenticate via the local SSH agent (`SSH_AUTH_SOCK` on Unix / OpenSSH on Windows).
async fn authenticate_with_agent(
    handle: &mut russh::client::Handle<HostKeyHandler>,
    username: &str,
    addr: &str,
) -> anyhow::Result<AuthResult> {
    #[cfg(unix)]
    {
        let mut agent = AgentClient::connect_env().await.map_err(|e| {
            anyhow::anyhow!(
                "cannot connect to SSH agent: {e}\n\
                 → Set SSH_AUTH_SOCK (e.g. start ssh-agent and export the socket path).\n\
                 → Then load keys with: ssh-add ~/.ssh/id_ed25519"
            )
        })?;
        return authenticate_with_agent_client(handle, username, addr, &mut agent).await;
    }

    #[cfg(windows)]
    {
        if let Ok(sock) = std::env::var("SSH_AUTH_SOCK") {
            match AgentClient::connect_named_pipe(&sock).await {
                Ok(mut agent) => {
                    return authenticate_with_agent_client(handle, username, addr, &mut agent)
                        .await;
                }
                Err(e) => {
                    tracing::debug!(
                        "SSH_AUTH_SOCK named pipe '{sock}' unavailable ({e}), trying Pageant"
                    );
                }
            }
        }
        let mut agent = AgentClient::connect_pageant().await.map_err(|e| {
            anyhow::anyhow!(
                "cannot connect to SSH agent (Pageant / named pipe): {e}\n\
                 → Start Pageant or Windows OpenSSH Authentication Agent, and load keys with ssh-add."
            )
        })?;
        return authenticate_with_agent_client(handle, username, addr, &mut agent).await;
    }

    #[cfg(not(any(unix, windows)))]
    {
        let _ = (handle, username, addr);
        anyhow::bail!("SSH agent authentication is not supported on this platform");
    }
}

/// Try each identity offered by the agent until one succeeds.
async fn authenticate_with_agent_client<R>(
    handle: &mut russh::client::Handle<HostKeyHandler>,
    username: &str,
    addr: &str,
    agent: &mut AgentClient<R>,
) -> anyhow::Result<AuthResult>
where
    R: AsyncRead + AsyncWrite + Unpin + Send,
{
    let identities = agent
        .request_identities()
        .await
        .map_err(|e| anyhow::anyhow!("failed to list identities from SSH agent: {e}"))?;

    if identities.is_empty() {
        anyhow::bail!(
            "SSH agent has no identities loaded.\n\
             → Run: ssh-add -l\n\
             → Add a key: ssh-add ~/.ssh/id_ed25519"
        );
    }

    tracing::info!(
        "SSH agent connected — {} identities available for {}@{}",
        identities.len(),
        username,
        addr
    );

    let mut last_detail = String::from("no identities were accepted");

    for (idx, identity) in identities.into_iter().enumerate() {
        let label = format_agent_identity_label(idx, identity.comment());

        let result = match identity {
            AgentIdentity::PublicKey { key, .. } => {
                let key_type = key_type_display(&key.algorithm());
                tracing::info!(
                    "SSH agent auth attempt {}: public key ({key_type}, {label})",
                    idx + 1
                );
                authenticate_pubkey_via_agent(handle, username, key, agent).await
            }
            AgentIdentity::Certificate { certificate, .. } => {
                let key_type = key_type_display(&certificate.public_key().algorithm());
                tracing::info!(
                    "SSH agent auth attempt {}: certificate ({key_type}, {label})",
                    idx + 1
                );
                authenticate_cert_via_agent(handle, username, certificate, agent).await
            }
        };

        match result {
            Ok(auth) if auth.success() => {
                tracing::info!("SSH agent auth succeeded with identity {label}");
                return Ok(auth);
            }
            Ok(auth) => {
                last_detail = format!("identity {label} rejected by server ({auth:?})");
                tracing::debug!("{last_detail}");
            }
            Err(e) => {
                last_detail = format!("identity {label} failed: {e}");
                tracing::debug!("{last_detail}");
            }
        }
    }

    anyhow::bail!(
        "all SSH agent identities were rejected by {addr}.\n\
         → Last attempt: {last_detail}\n\
         → Confirm the matching public key is in ~/.ssh/authorized_keys on the target."
    )
}

/// Public-key auth via agent signer, with RSA hash fallback when needed.
async fn authenticate_pubkey_via_agent<R>(
    handle: &mut russh::client::Handle<HostKeyHandler>,
    username: &str,
    key: ssh_key::PublicKey,
    agent: &mut AgentClient<R>,
) -> anyhow::Result<AuthResult>
where
    R: AsyncRead + AsyncWrite + Unpin + Send,
{
    let is_rsa = key.algorithm().is_rsa();

    if !is_rsa {
        return handle
            .authenticate_publickey_with(username, key, None, agent)
            .await
            .map_err(|e| anyhow::anyhow!("{e}"));
    }

    let preferred = preferred_rsa_hash(handle).await;
    let chain = build_rsa_fallback_chain(preferred);

    for hash_alg in &chain {
        match handle
            .authenticate_publickey_with(username, key.clone(), *hash_alg, agent)
            .await
        {
            Ok(result) if result.success() => {
                tracing::info!("SSH agent RSA auth succeeded with {}", hash_label(hash_alg));
                return Ok(result);
            }
            Ok(result) => {
                tracing::debug!(
                    "SSH agent RSA {} returned {:?}, trying next",
                    hash_label(hash_alg),
                    result
                );
            }
            Err(e) => {
                tracing::debug!(
                    "SSH agent RSA {} error: {e}, trying next",
                    hash_label(hash_alg)
                );
            }
        }
    }

    anyhow::bail!(
        "agent RSA key rejected for all hash algorithms ({})",
        chain.iter().map(hash_label).collect::<Vec<_>>().join(" → ")
    )
}

/// Certificate auth via agent signer, with RSA hash fallback when needed.
async fn authenticate_cert_via_agent<R>(
    handle: &mut russh::client::Handle<HostKeyHandler>,
    username: &str,
    cert: ssh_key::Certificate,
    agent: &mut AgentClient<R>,
) -> anyhow::Result<AuthResult>
where
    R: AsyncRead + AsyncWrite + Unpin + Send,
{
    let is_rsa = cert.public_key().algorithm().is_rsa();

    if !is_rsa {
        return handle
            .authenticate_certificate_with(username, cert, None, agent)
            .await
            .map_err(|e| anyhow::anyhow!("{e}"));
    }

    let preferred = preferred_rsa_hash(handle).await;
    let chain = build_rsa_fallback_chain(preferred);

    for hash_alg in &chain {
        match handle
            .authenticate_certificate_with(username, cert.clone(), *hash_alg, agent)
            .await
        {
            Ok(result) if result.success() => {
                tracing::info!(
                    "SSH agent RSA certificate auth succeeded with {}",
                    hash_label(hash_alg)
                );
                return Ok(result);
            }
            Ok(result) => {
                tracing::debug!(
                    "SSH agent RSA cert {} returned {:?}, trying next",
                    hash_label(hash_alg),
                    result
                );
            }
            Err(e) => {
                tracing::debug!(
                    "SSH agent RSA cert {} error: {e}, trying next",
                    hash_label(hash_alg)
                );
            }
        }
    }

    anyhow::bail!(
        "agent RSA certificate rejected for all hash algorithms ({})",
        chain.iter().map(hash_label).collect::<Vec<_>>().join(" → ")
    )
}

/// Best RSA signature hash advertised by the server (if any).
async fn preferred_rsa_hash(handle: &mut russh::client::Handle<HostKeyHandler>) -> Option<HashAlg> {
    match handle.best_supported_rsa_hash().await {
        Ok(Some(Some(hash))) => {
            tracing::info!("SSH server prefers {:?} for RSA signatures", hash);
            Some(hash)
        }
        Ok(Some(None)) => {
            tracing::warn!(
                "SSH server explicitly does NOT support rsa-sha2-*; will try anyway then fall back to ssh-rsa"
            );
            None
        }
        Ok(None) | Err(_) => {
            tracing::info!(
                "SSH server did not advertise server-sig-algs, will try all RSA hash types"
            );
            None
        }
    }
}

/// SSH server host-key verification.
struct HostKeyHandler {
    host: String,
    port: u16,
    policy: SshHostKeyPolicy,
    known_hosts_path: Option<String>,
}

impl client::Handler for HostKeyHandler {
    type Error = anyhow::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &ssh_key::PublicKey,
    ) -> Result<bool, Self::Error> {
        match self.policy {
            SshHostKeyPolicy::Insecure => {
                tracing::warn!(
                    "SSH host key check disabled (policy=insecure) for {}:{}; accepting {}",
                    self.host,
                    self.port,
                    server_public_key.fingerprint(HashAlg::Sha256),
                );
                Ok(true)
            }
            SshHostKeyPolicy::KnownHosts => verify_against_known_hosts(
                &self.host,
                self.port,
                server_public_key,
                self.known_hosts_path.as_deref(),
            ),
        }
    }
}

/// Verify `server_public_key` against an OpenSSH known_hosts file.
fn verify_against_known_hosts(
    host: &str,
    port: u16,
    server_public_key: &ssh_key::PublicKey,
    known_hosts_path: Option<&str>,
) -> anyhow::Result<bool> {
    let path = resolve_known_hosts_path(known_hosts_path)?;
    // russh compares PublicKey with PartialEq, which includes the comment field.
    // known_hosts parsing yields empty comments, so strip before check to avoid
    // false "host key changed" when the presented key carries a label.
    let mut key_for_check = server_public_key.clone();
    key_for_check.set_comment("");
    match check_known_hosts_path(host, port, &key_for_check, &path) {
        Ok(true) => {
            tracing::info!(
                "SSH host key OK for {host}:{port} ({})",
                server_public_key.fingerprint(HashAlg::Sha256)
            );
            Ok(true)
        }
        Ok(false) => anyhow::bail!(
            "SSH host key for {host}:{port} was not found in {}\n\
             → Fingerprint: {}\n\
             → Add the host with: ssh-keyscan -p {port} {host} >> {}\n\
             → Or set SSH host key policy to \"insecure\" in Manager UI → Runners (lab only)",
            path.display(),
            server_public_key.fingerprint(HashAlg::Sha256),
            path.display(),
        ),
        Err(russh::keys::Error::KeyChanged { line }) => anyhow::bail!(
            "SSH HOST KEY CHANGED for {host}:{port} (known_hosts line {line})\n\
             → Fingerprint now: {}\n\
             → File: {}\n\
             → This may indicate a MITM attack or a reinstalled server. Investigate before updating known_hosts.",
            server_public_key.fingerprint(HashAlg::Sha256),
            path.display(),
        ),
        Err(e) => anyhow::bail!("failed to read known_hosts {}: {e}", path.display()),
    }
}

fn resolve_known_hosts_path(configured: Option<&str>) -> anyhow::Result<std::path::PathBuf> {
    if let Some(p) = configured.map(str::trim).filter(|s| !s.is_empty()) {
        return Ok(std::path::PathBuf::from(p));
    }
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map_err(|_| {
            anyhow::anyhow!("HOME/USERPROFILE not set; set engine.ssh_known_hosts_path explicitly")
        })?;
    Ok(std::path::PathBuf::from(home)
        .join(".ssh")
        .join("known_hosts"))
}

#[cfg(test)]
#[path = "ssh_tests.rs"]
mod tests;
