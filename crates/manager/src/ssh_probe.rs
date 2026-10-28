//! SSH connectivity probe for Host form "Test" button.
//!
//! Runs on the Manager host (not a Runner). Host-key verification is
//! intentionally insecure for this probe — the goal is to validate address,
//! port, username, and credentials from the Manager's network path.
//!
//! `credential_type = agent` cannot be probed here (agent lives on the Runner).

use std::sync::Arc;
use std::time::{Duration, Instant};

use russh::client;
use russh::client::AuthResult;
use russh::keys::key::PrivateKeyWithHashAlg;
use russh::keys::load_secret_key;
use russh::keys::ssh_key;
use russh::keys::HashAlg;
use tokio::time::timeout;

#[derive(Debug, Clone)]
pub struct ProbeParams {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub credential_type: String,
    pub credential_value: String,
    pub credential_passphrase: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ProbeResult {
    pub ok: bool,
    pub latency_ms: u64,
    pub message: String,
}

/// Attempt SSH connect + auth (+ trivial `true` command). Soft-fails into [`ProbeResult`].
pub async fn probe(params: ProbeParams) -> ProbeResult {
    let started = Instant::now();
    let result = timeout(Duration::from_secs(15), probe_inner(&params)).await;
    let latency_ms = started.elapsed().as_millis() as u64;

    match result {
        Ok(Ok(())) => ProbeResult {
            ok: true,
            latency_ms,
            message: format!(
                "SSH OK — authenticated as {}@{}:{} and ran a probe command ({latency_ms} ms). \
                 Note: this test runs from the Manager host; jobs use a Runner's network path.",
                params.username, params.host, params.port
            ),
        },
        Ok(Err(e)) => ProbeResult {
            ok: false,
            latency_ms,
            message: e.to_string(),
        },
        Err(_) => ProbeResult {
            ok: false,
            latency_ms,
            message: "SSH probe timed out after 15s (host unreachable or auth hung)".into(),
        },
    }
}

async fn probe_inner(params: &ProbeParams) -> anyhow::Result<()> {
    let cred = params.credential_type.trim().to_ascii_lowercase();
    if cred == "agent" {
        anyhow::bail!(
            "SSH Agent auth cannot be tested from the Manager.\n\
             → Agent keys live on the Runner (`SSH_AUTH_SOCK` / ssh-add).\n\
             → Save the host, then run an SSH pipeline step on a Runner to verify."
        );
    }

    if params.host.trim().is_empty() {
        anyhow::bail!("host address is required");
    }
    if params.username.trim().is_empty() {
        anyhow::bail!("username is required");
    }
    if params.port == 0 {
        anyhow::bail!("port must be between 1 and 65535");
    }

    // Validate credentials before opening a TCP connection.
    match cred.as_str() {
        "key_path" if params.credential_value.trim().is_empty() => {
            anyhow::bail!(
                "private key is required for SSH Key auth.\n\
                 → Paste the key in the form, or (when editing) leave blank only if a key is already stored — then retry Test."
            );
        }
        "vault" | "password" if params.credential_value.is_empty() => {
            anyhow::bail!(
                "password is required for Password auth.\n\
                 → Enter the password, or (when editing) leave blank only if already stored — then retry Test."
            );
        }
        "key_path" | "vault" | "password" => {}
        other => anyhow::bail!("unsupported credential type: {other}"),
    }

    let config = Arc::new(client::Config::default());
    let addr = format!("{}:{}", params.host.trim(), params.port);

    let mut handle = client::connect(
        config,
        &addr,
        AcceptAllHostKeys {
            host: params.host.clone(),
            port: params.port,
        },
    )
    .await
    .map_err(|e| anyhow::anyhow!("SSH connection to {addr} failed: {e}"))?;

    let authenticated = match cred.as_str() {
        "key_path" => {
            let tmp = std::env::temp_dir().join(format!("buz_host_probe_{}", uuid::Uuid::new_v4()));
            std::fs::write(&tmp, &params.credential_value)
                .map_err(|e| anyhow::anyhow!("failed to write temp key file: {e}"))?;
            let path = tmp.to_string_lossy().to_string();
            let cleanup = tmp.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_secs(5)).await;
                let _ = std::fs::remove_file(cleanup);
            });
            let key =
                load_secret_key(&path, params.credential_passphrase.as_deref()).map_err(|e| {
                    anyhow::anyhow!(
                        "failed to parse SSH private key: {e}\n\
                         → Check the key format and passphrase."
                    )
                })?;
            authenticate_with_private_key(&mut handle, params.username.trim(), &key, &addr).await?
        }
        "vault" | "password" => handle
            .authenticate_password(params.username.trim(), &params.credential_value)
            .await
            .map_err(|e| {
                anyhow::anyhow!(
                    "SSH password authentication rejected for {}@{}: {e}",
                    params.username,
                    addr
                )
            })?,
        _ => unreachable!("credential type validated above"),
    };

    if !authenticated.success() {
        anyhow::bail!(
            "SSH server rejected authentication for {}@{} (type '{}')",
            params.username,
            addr,
            params.credential_type
        );
    }

    let mut channel = handle
        .channel_open_session()
        .await
        .map_err(|e| anyhow::anyhow!("SSH session open failed: {e}"))?;
    channel
        .exec(true, "true")
        .await
        .map_err(|e| anyhow::anyhow!("SSH exec failed: {e}"))?;

    // Drain until channel close — ExitStatus may arrive after Eof.
    let mut exit_code: Option<u32> = None;
    loop {
        match channel.wait().await {
            Some(russh::ChannelMsg::ExitStatus { exit_status }) => {
                exit_code = Some(exit_status);
            }
            Some(russh::ChannelMsg::Eof | russh::ChannelMsg::Close) => {}
            None => break,
            _ => {}
        }
    }
    match exit_code {
        Some(0) => {}
        Some(status) => anyhow::bail!("probe command exited with status {status}"),
        None => anyhow::bail!("probe channel closed without an exit status"),
    }

    Ok(())
}

struct AcceptAllHostKeys {
    host: String,
    port: u16,
}

impl client::Handler for AcceptAllHostKeys {
    type Error = anyhow::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &ssh_key::PublicKey,
    ) -> Result<bool, Self::Error> {
        tracing::info!(
            "host probe accepting host key for {}:{} ({})",
            self.host,
            self.port,
            server_public_key.fingerprint(HashAlg::Sha256)
        );
        Ok(true)
    }
}

async fn authenticate_with_private_key(
    handle: &mut russh::client::Handle<AcceptAllHostKeys>,
    username: &str,
    key: &russh::keys::PrivateKey,
    addr: &str,
) -> anyhow::Result<AuthResult> {
    let alg = key.algorithm();
    if !alg.clone().is_rsa() {
        let key_with_hash = PrivateKeyWithHashAlg::new(Arc::new(key.clone()), None);
        return handle
            .authenticate_publickey(username, key_with_hash)
            .await
            .map_err(|e| anyhow::anyhow!("public-key authentication failed: {e}"));
    }

    let preferred = match handle.best_supported_rsa_hash().await {
        Ok(Some(Some(hash))) => Some(hash),
        _ => None,
    };
    let chain = rsa_fallback_chain(preferred);
    for hash_alg in &chain {
        let key_with_hash = PrivateKeyWithHashAlg::new(Arc::new(key.clone()), *hash_alg);
        match handle.authenticate_publickey(username, key_with_hash).await {
            Ok(result) if result.success() => return Ok(result),
            Ok(_) | Err(_) => continue,
        }
    }
    anyhow::bail!("all RSA signature algorithms were rejected by {addr}")
}

fn rsa_fallback_chain(preferred: Option<HashAlg>) -> Vec<Option<HashAlg>> {
    let all = [Some(HashAlg::Sha512), Some(HashAlg::Sha256), None];
    let mut chain = Vec::with_capacity(4);
    if let Some(pref) = preferred {
        chain.push(Some(pref));
    }
    for c in all {
        if !chain.contains(&c) {
            chain.push(c);
        }
    }
    chain
}
