use anyhow::Context;
use reqwest::Client;
use shared::protocol::{RegisterRequest, RegisterResponse, RunnerPolicyConfig};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use crate::config::Config;

pub struct RegisterResult {
    pub token: String,
    pub policy: RunnerPolicyConfig,
}

/// Persistent cache for the Manager-issued Runner JWT.
fn token_cache_path(config: &Config) -> PathBuf {
    PathBuf::from(&config.runner.token_cache_path)
}

fn legacy_token_cache_path(config: &Config) -> PathBuf {
    Path::new(&config.runner.workspace_dir).join("runner-token")
}

/// Read the new cache first, falling back only when it does not exist.
pub fn load_cached_token(config: &Config) -> anyhow::Result<Option<String>> {
    let path = token_cache_path(config);
    if let Some(token) = read_token(&path, true)? {
        return Ok(Some(token));
    }

    let legacy = legacy_token_cache_path(config);
    if legacy == path {
        return Ok(None);
    }
    read_token(&legacy, false)
}

fn read_token(path: &Path, require_private_permissions: bool) -> anyhow::Result<Option<String>> {
    let mut file = match std::fs::OpenOptions::new().read(true).open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        anyhow::bail!(
            "runner credential path is not a regular file: {}",
            path.display()
        );
    }
    #[cfg(unix)]
    if require_private_permissions {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            anyhow::bail!(
                "runner credential file must have mode 0600: {}",
                path.display()
            );
        }
    }
    let mut token = String::new();
    file.read_to_string(&mut token)?;
    let token = token.trim().to_string();
    if token.is_empty() {
        anyhow::bail!("runner credential file is empty: {}", path.display());
    }
    Ok(Some(token))
}

/// Atomically persist a JWT with owner-only permissions on Unix.
pub fn cache_token(config: &Config, token: &str) -> anyhow::Result<()> {
    let token = token.trim();
    if token.is_empty() {
        anyhow::bail!("refusing to cache an empty Runner credential");
    }
    let path = token_cache_path(config);
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("runner-token");
    let temp_path = parent.join(format!(".{file_name}.{}", uuid::Uuid::new_v4().simple()));
    let result = (|| -> anyhow::Result<()> {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temp_path)?;
        file.write_all(token.as_bytes())?;
        file.sync_all()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        std::fs::rename(&temp_path, &path)?;
        std::fs::File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp_path);
    } else if let Err(error) = remove_legacy_cache(config) {
        tracing::warn!(error = %error, "could not remove legacy runner token cache after migration");
    }
    result
}

fn remove_legacy_cache(config: &Config) -> anyhow::Result<()> {
    let legacy = legacy_token_cache_path(config);
    if legacy != token_cache_path(config) {
        match std::fs::remove_file(&legacy) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

pub async fn register_with_center(
    client: &Client,
    config: &Config,
) -> anyhow::Result<RegisterResult> {
    let req = RegisterRequest {
        name: config.runner.name.clone(),
        register_token: config.runner.register_token.clone(),
        // Labels are configured in Manager UI; agent must not overwrite them.
        labels: Vec::new(),
        version: Some(config.runner.version().into()),
    };

    // Present the previous JWT (if any) so re-registration passes the identity
    // check even though the one-shot register_token is long consumed.
    let cached = load_cached_token(config).context("failed to load cached Runner credential")?;

    let url = format!("{}/api/v1/runners/register", config.runner.manager_url);
    let mut builder = client.post(&url).json(&req);
    if let Some(bearer) = cached.as_deref() {
        builder = builder.bearer_auth(bearer);
    }
    let resp = builder.send().await.map_err(|e| {
        super::friendly_reqwest_error(e, &config.runner.manager_url, "Registration failed")
    })?;

    let status = resp.status();
    let body = resp.text().await?;
    if !status.is_success() {
        tracing::warn!(%status, "register rejected");
        anyhow::bail!("registration failed: {body}");
    }
    tracing::debug!(%status, "register ok");

    // Parse the full ApiResponse envelope first, then extract data.
    let json: serde_json::Value = serde_json::from_str(&body)?;

    let data = json
        .get("data")
        .and_then(|d| serde_json::from_value::<RegisterResponse>(d.clone()).ok());

    let register_resp = data.ok_or_else(|| {
        anyhow::anyhow!("empty or invalid data in registration response: {}", json)
    })?;

    let mut policy = register_resp.policy;
    policy.max_concurrent_jobs = policy.max_concurrent_jobs.max(1);

    tracing::info!(
        runner_id = %register_resp.runner_id,
        max_concurrent = policy.max_concurrent_jobs,
        shell = policy.shell_enabled,
        ssh = policy.ssh_enabled,
        docker = policy.docker_enabled,
        wasm = policy.wasm_enabled,
        "runner registered"
    );

    Ok(RegisterResult {
        token: register_resp.token,
        policy,
    })
}
