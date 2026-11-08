//! Conservative, anonymous Docker image prefetching for immutable Docker Hub
//! official-library images. Only anonymous pulls are issued here; private
//! images stay on the original step-time Docker path.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::Value;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::config::Config;
use crate::engine;

pub const MAX_CONCURRENT_PULLS: usize = 3;

/// A best-effort collection of anonymous digest pulls. The anonymous Docker
/// config is unique to this job and contains no credentials.
pub struct ImagePrefetch {
    tasks: JoinSet<(String, anyhow::Result<()>)>,
    pending: HashSet<String>,
    completed: HashMap<String, anyhow::Result<()>>,
    _anonymous_config: Arc<AnonymousDockerConfig>,
}

impl ImagePrefetch {
    pub fn start(tasks: &[Value], config: &Config, job_id: &str) -> Option<Self> {
        let candidates = candidates(tasks, config);
        Self::start_with_candidates(candidates, job_id, |image, config_dir| async move {
            engine::docker::pull_image(&image, Some(&config_dir)).await
        })
    }

    pub fn start_with_candidates<F, Fut>(
        candidates: Vec<String>,
        job_id: &str,
        pull: F,
    ) -> Option<Self>
    where
        F: Fn(String, String) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = anyhow::Result<()>> + Send + 'static,
    {
        if candidates.is_empty() {
            return None;
        }

        let anonymous_config = Arc::new(match AnonymousDockerConfig::create(job_id) {
            Ok(config_dir) => config_dir,
            Err(error) => {
                tracing::warn!(
                    "Docker image prefetch disabled: cannot create anonymous config: {error}"
                );
                return None;
            }
        });
        let semaphore = Arc::new(Semaphore::new(MAX_CONCURRENT_PULLS));
        let mut pulls = JoinSet::new();
        let mut pending = HashSet::new();
        let pull = Arc::new(pull);
        for image in candidates {
            pending.insert(image.clone());
            let semaphore = Arc::clone(&semaphore);
            let anonymous_config = Arc::clone(&anonymous_config);
            let config_dir = anonymous_config.path().to_string_lossy().to_string();
            let pull = Arc::clone(&pull);
            pulls.spawn(async move {
                let _config_guard = anonymous_config;
                let _permit = semaphore.acquire_owned().await;
                let result = pull(image.clone(), config_dir).await;
                (image, result)
            });
        }

        Some(Self {
            tasks: pulls,
            pending,
            completed: HashMap::new(),
            _anonymous_config: anonymous_config,
        })
    }

    pub async fn abort_and_drain(&mut self) {
        self.tasks.abort_all();
        while self.tasks.join_next().await.is_some() {}
        self.pending.clear();
    }

    /// Wait for the pull associated with this Docker step, if it was eligible.
    /// Failures are returned to the caller for warning-only handling.
    pub async fn wait_for(&mut self, image: &str) -> Option<anyhow::Result<()>> {
        let image = image.trim();
        if !self.pending.contains(image) {
            return None;
        }
        while !self.completed.contains_key(image) {
            let joined = self.tasks.join_next().await?;
            match joined {
                Ok((finished_image, result)) => {
                    self.pending.remove(&finished_image);
                    self.completed.insert(finished_image, result);
                }
                Err(error) => {
                    let message = format!("prefetch task failed: {error}");
                    let missing: Vec<String> = self.pending.drain().collect();
                    for missing_image in missing {
                        self.completed
                            .insert(missing_image, Err(anyhow::anyhow!(message.clone())));
                    }
                }
            }
        }
        self.completed.remove(image)
    }
}

/// Return unique eligible images. Only canonical Docker Hub official-library
/// references pinned to an explicit sha256 digest are safe for anonymous early
/// pulls; all tags, private namespaces and other registries remain on the
/// original step-time Docker path.
pub fn candidates(tasks: &[Value], config: &Config) -> Vec<String> {
    if !config.engine.docker_enabled {
        return Vec::new();
    }

    let mut seen = HashSet::new();
    let mut images = Vec::new();
    for step in tasks {
        if step.get("exec_type").and_then(Value::as_str) != Some("docker") {
            continue;
        }
        let image = step.get("image").and_then(Value::as_str).unwrap_or("");
        if !is_public_library_digest(image) {
            continue;
        }
        let volumes: Vec<String> = step
            .get("volumes")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        let capabilities: Vec<String> = step
            .get("capabilities")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        let privileged = step
            .get("privileged")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if engine::docker::preflight(config.engine.docker_enabled, image).is_err()
            || engine::docker::validate_step_policy(
                &config.runner,
                &capabilities,
                &volumes,
                privileged,
            )
            .is_err()
        {
            continue;
        }
        if seen.insert(image.to_string()) {
            images.push(image.to_string());
        }
    }
    images
}

/// Strict subset of Docker references: an unqualified official image name,
/// optionally prefixed with `docker.io/library/`, pinned to sha256 and nothing
/// else. Tag+digest forms are rejected rather than partially parsed.
pub fn is_public_library_digest(reference: &str) -> bool {
    if reference.is_empty()
        || reference.trim() != reference
        || reference.chars().any(char::is_whitespace)
    {
        return false;
    }
    let Some((name, digest)) = reference.split_once('@') else {
        return false;
    };
    if digest.len() != "sha256:".len() + 64 || !digest.starts_with("sha256:") {
        return false;
    }
    if !digest[7..].bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return false;
    }

    let image_name = if let Some(name) = name.strip_prefix("docker.io/library/") {
        name
    } else if name.contains('/') || name.contains(':') {
        return false;
    } else {
        name
    };
    if image_name.is_empty() || image_name.len() > 255 {
        return false;
    }
    let bytes = image_name.as_bytes();
    if !bytes[0].is_ascii_lowercase() && !bytes[0].is_ascii_digit() {
        return false;
    }
    if !bytes[bytes.len() - 1].is_ascii_lowercase() && !bytes[bytes.len() - 1].is_ascii_digit() {
        return false;
    }
    bytes.iter().all(|byte| {
        byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(*byte, b'.' | b'_' | b'-')
    })
}

struct AnonymousDockerConfig(PathBuf);

impl AnonymousDockerConfig {
    fn create(_job_id: &str) -> std::io::Result<Self> {
        let root = std::env::temp_dir().join("buz-docker-prefetch");
        std::fs::create_dir_all(&root)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))?;
        }
        let path = root.join(format!("{}-{}", std::process::id(), uuid::Uuid::new_v4()));
        std::fs::create_dir(&path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))?;
        }
        Ok(Self(path))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for AnonymousDockerConfig {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_dir_all(&self.0) {
            if error.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(
                    "Failed to remove anonymous Docker config {:?}: {error}",
                    self.0
                );
            }
        }
    }
}
