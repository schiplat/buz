//! sandboxd configuration: host bootstrap (local, immutable for the process)
//! plus the Manager-managed policy document (fetched, validated, applied
//! atomically, cached). Replaces the former `conf/sandboxd.toml`.
//!
//! Bootstrap answers "where am I and who do I trust" — socket path, workspace
//! root/volume, Runner UID/GID, Docker CLI, instance/host identity, Manager
//! URL + token. The Manager cannot know these, so they stay local (env/CLI).
//!
//! Policy answers "what may I run and how big" — resource profiles, tier
//! images/runtimes, network. It comes exclusively from the Manager; there is
//! no built-in default profile or image. Until a document has been applied
//! (from the Manager or the last-known-good cache), `create_job` is refused.

use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use shared::protocol::RunnerExecutionMode;
use shared::sandbox_config::{ResourceProfile, SandboxHostDocument, DEFAULT_PROFILE};

use super::backend::TierBackends;

/// Host-local bootstrap, resolved once at startup. Everything here is a fact
/// about this machine or an operator-issued credential.
#[derive(Debug, Clone)]
pub struct Bootstrap {
    pub instance_id: String,
    pub host_id: String,
    pub socket: String,
    pub workspace_root: String,
    pub workspace_volume: Option<String>,
    pub docker_cli: String,
    pub socket_gid: u32,
    pub allowed_runner_uid: Option<u32>,
    /// Manager base URL (`https://buz.example.com`). Empty = offline mode:
    /// no fetch loop, cache only (tests and air-gapped debugging).
    pub manager_url: String,
    /// Expected Manager control-plane identity; a response from a different
    /// Manager is rejected rather than applied. Empty = not pinned.
    pub manager_instance_id: String,
    /// Per-instance bearer token (`buzsd_…`), read from a `0400` file.
    pub token: String,
    /// Accept invalid TLS certificates. Labs only.
    pub tls_skip_verify: bool,
    /// Directory for the root-owned last-known-good config cache.
    pub state_dir: PathBuf,
}

impl Bootstrap {
    pub fn cache_path(&self) -> PathBuf {
        self.state_dir.join("sandboxd-config.json")
    }
}

/// A validated, normalized document together with the version/hash it was
/// served under — what the job path consults.
#[derive(Debug, Clone, PartialEq)]
pub struct AppliedConfig {
    pub version: i64,
    pub sha256: String,
    pub manager_instance_id: String,
    pub applied_at: String,
    pub document: SandboxHostDocument,
}

impl AppliedConfig {
    /// Per-tier backend table from the document's `tiers` section. Missing
    /// tiers get an empty image so `is_configured()` reports them unavailable.
    pub fn backends(&self) -> TierBackends {
        let tier = |name: &str| {
            self.document
                .tiers
                .get(name)
                .map(|t| (t.image.clone(), t.runtime.clone()))
                .unwrap_or_default()
        };
        let (c_img, c_rt) = tier("container");
        let (s_img, s_rt) = tier("sandbox");
        let (v_img, v_rt) = tier("vm");
        TierBackends::from_tiers([
            (RunnerExecutionMode::Container, c_img, c_rt),
            (RunnerExecutionMode::Sandbox, s_img, s_rt),
            (RunnerExecutionMode::Vm, v_img, v_rt),
        ])
    }

    /// Resolve a profile by name. An empty name means `default`; an unknown
    /// name is an error — never a silent fall-back to `default`.
    pub fn profile<'a>(&'a self, name: &str) -> anyhow::Result<(&'a str, &'a ResourceProfile)> {
        let name = if name.trim().is_empty() {
            DEFAULT_PROFILE
        } else {
            name.trim()
        };
        let (key, profile) = self.document.profiles.get_key_value(name).ok_or_else(|| {
            anyhow::anyhow!("resource profile '{name}' is not offered by this sandbox host")
        })?;
        Ok((key.as_str(), profile))
    }

    pub fn network(&self) -> &str {
        &self.document.network
    }
}

/// Shared, atomically replaceable configuration state.
///
/// Readers take a cheap snapshot (`Arc` clone) and work against it for the
/// whole operation; writers swap the pointer. Running containers therefore
/// keep the limits/image they were created with, and a half-applied document
/// is never observable.
#[derive(Clone, Default)]
pub struct ConfigState {
    inner: Arc<RwLock<Option<Arc<AppliedConfig>>>>,
}

impl ConfigState {
    pub fn new(initial: Option<AppliedConfig>) -> Self {
        Self {
            inner: Arc::new(RwLock::new(initial.map(Arc::new))),
        }
    }

    /// Current applied configuration, if any document has been applied.
    pub fn snapshot(&self) -> Option<Arc<AppliedConfig>> {
        self.inner.read().expect("config lock poisoned").clone()
    }

    /// Require an applied configuration — the job path fails closed without one.
    pub fn require(&self) -> anyhow::Result<Arc<AppliedConfig>> {
        self.snapshot().ok_or_else(|| {
            anyhow::anyhow!(
                "sandbox host has no applied configuration yet (waiting for the Manager or a cached document)"
            )
        })
    }

    /// Atomically replace the applied configuration.
    pub fn replace(&self, next: AppliedConfig) {
        *self.inner.write().expect("config lock poisoned") = Some(Arc::new(next));
    }

    pub fn applied_version(&self) -> i64 {
        self.snapshot().map(|c| c.version).unwrap_or(0)
    }
}
