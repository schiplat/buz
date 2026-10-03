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

#[cfg(test)]
mod token_cache_tests {
    use super::{cache_token, load_cached_token};
    use crate::config::Config;
    use std::path::Path;

    fn config(root: &Path) -> Config {
        let mut config = Config::default();
        config.runner.workspace_dir = root.join("workspace").to_string_lossy().into_owned();
        config.runner.token_cache_path = root
            .join("state/runner-token")
            .to_string_lossy()
            .into_owned();
        config
    }

    #[test]
    fn missing_new_cache_falls_back_to_legacy_cache() {
        let root = tempfile::tempdir().unwrap();
        let config = config(root.path());
        let legacy = Path::new(&config.runner.workspace_dir).join("runner-token");
        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        std::fs::write(&legacy, "legacy-jwt\n").unwrap();

        assert_eq!(
            load_cached_token(&config).unwrap().as_deref(),
            Some("legacy-jwt")
        );
    }

    #[test]
    fn legacy_cache_migrates_after_new_cache_is_persisted() {
        let root = tempfile::tempdir().unwrap();
        let config = config(root.path());
        let legacy = Path::new(&config.runner.workspace_dir).join("runner-token");
        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        std::fs::write(&legacy, "legacy-jwt\n").unwrap();

        assert_eq!(
            load_cached_token(&config).unwrap().as_deref(),
            Some("legacy-jwt")
        );
        cache_token(&config, "legacy-jwt").unwrap();

        assert_eq!(
            load_cached_token(&config).unwrap().as_deref(),
            Some("legacy-jwt")
        );
        assert!(!legacy.exists());
    }

    #[test]
    fn cache_write_replaces_existing_token_and_uses_private_permissions() {
        let root = tempfile::tempdir().unwrap();
        let config = config(root.path());
        cache_token(&config, "first-jwt").unwrap();
        cache_token(&config, "second-jwt").unwrap();

        assert_eq!(
            load_cached_token(&config).unwrap().as_deref(),
            Some("second-jwt")
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let metadata = std::fs::metadata(&config.runner.token_cache_path).unwrap();
            assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
        }
    }

    #[cfg(unix)]
    #[test]
    fn insecure_new_cache_fails_without_falling_back_to_legacy() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().unwrap();
        let config = config(root.path());
        let legacy = Path::new(&config.runner.workspace_dir).join("runner-token");
        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        std::fs::write(&legacy, "legacy-jwt").unwrap();
        std::fs::create_dir_all(Path::new(&config.runner.token_cache_path).parent().unwrap())
            .unwrap();
        std::fs::write(&config.runner.token_cache_path, "insecure-jwt").unwrap();
        std::fs::set_permissions(
            &config.runner.token_cache_path,
            std::fs::Permissions::from_mode(0o644),
        )
        .unwrap();

        let error = load_cached_token(&config).unwrap_err().to_string();
        assert!(error.contains("mode 0600"), "{error}");
        assert!(legacy.exists());
    }

    #[test]
    fn non_regular_new_cache_fails_without_falling_back_to_legacy() {
        let root = tempfile::tempdir().unwrap();
        let config = config(root.path());
        let legacy = Path::new(&config.runner.workspace_dir).join("runner-token");
        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        std::fs::write(&legacy, "legacy-jwt").unwrap();
        std::fs::create_dir_all(Path::new(&config.runner.token_cache_path).parent().unwrap())
            .unwrap();
        std::fs::create_dir(Path::new(&config.runner.token_cache_path)).unwrap();

        let error = load_cached_token(&config).unwrap_err().to_string();
        assert!(error.contains("not a regular file"), "{error}");
        assert!(legacy.exists());
    }

    #[test]
    fn empty_cache_is_an_error() {
        let root = tempfile::tempdir().unwrap();
        let config = config(root.path());
        let path = Path::new(&config.runner.token_cache_path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "  \n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }

        let error = load_cached_token(&config).unwrap_err().to_string();
        assert!(error.contains("empty"), "{error}");
    }
}
