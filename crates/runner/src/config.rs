use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub manager: ManagerConfig,
    /// Local runner identity (name, labels, workspace). TOML section: `[runner]`.
    #[serde(default)]
    pub runner: RunnerIdentity,
    #[serde(default)]
    pub engine: EngineConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ManagerConfig {
    #[serde(default = "default_manager_url")]
    pub url: String,
    #[serde(default)]
    pub register_token: String,
    /// Allow invalid TLS certificates (self-signed / internal CA).
    /// Set to true only for development or when using a self-signed cert.
    #[serde(default)]
    pub tls_skip_verify: bool,
}

/// Identity and workspace settings for this runner process (`[runner]` in runner.toml).
#[derive(Debug, Clone, Deserialize)]
pub struct RunnerIdentity {
    #[serde(default = "default_runner_name")]
    pub name: String,
    #[serde(default = "default_labels")]
    pub labels: Vec<String>,
    #[serde(default = "default_work_dir")]
    pub work_dir: String,
    /// Clean the work_dir after each job completes.
    /// Recommended for shared runners to prevent data leakage between jobs.
    #[serde(default)]
    pub clean_workspace_after_job: bool,
    /// Ephemeral mode: clean workspace after each job AND exit the runner process.
    /// Useful for container-based runners that are destroyed after one job.
    #[serde(default)]
    pub ephemeral: bool,
    /// Interface names whose IPs are advertised first in Manager UI (multi-NIC VMs).
    /// Example: `["enp26s0"]`. When empty, IPs on the same subnet as `manager.url` are preferred.
    #[serde(default)]
    pub prefer_interfaces: Vec<String>,
}

impl Default for ManagerConfig {
    fn default() -> Self {
        Self {
            url: default_manager_url(),
            register_token: String::new(),
            tls_skip_verify: false,
        }
    }
}

impl Default for RunnerIdentity {
    fn default() -> Self {
        Self {
            name: default_runner_name(),
            labels: default_labels(),
            work_dir: default_work_dir(),
            clean_workspace_after_job: false,
            ephemeral: false,
            prefer_interfaces: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct EngineConfig {
    #[serde(default = "default_true")]
    pub local_enabled: bool,
    #[serde(default = "default_true")]
    pub ssh_enabled: bool,
    #[serde(default = "default_false")]
    pub docker_enabled: bool,
    #[serde(default = "default_false")]
    pub wasm_enabled: bool,
    /// SSH host-key verification policy (see [`SshHostKeyPolicy`]).
    #[serde(default)]
    pub ssh_host_key_policy: SshHostKeyPolicy,
    /// Path to OpenSSH `known_hosts` when policy is `known_hosts`.
    /// Empty → `~/.ssh/known_hosts`.
    #[serde(default)]
    pub ssh_known_hosts_path: Option<String>,
}

/// How the runner verifies the remote SSH server host key.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum SshHostKeyPolicy {
    /// Accept any server key (convenient for labs; MITM-vulnerable).
    #[default]
    Insecure,
    /// Require the server key to match an entry in `known_hosts`.
    KnownHosts,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            local_enabled: true,
            ssh_enabled: true,
            docker_enabled: false,
            wasm_enabled: false,
            ssh_host_key_policy: SshHostKeyPolicy::Insecure,
            ssh_known_hosts_path: None,
        }
    }
}

impl EngineConfig {
    /// Reject steps whose engine is disabled in `runner.toml`.
    pub fn ensure_enabled(&self, exec_type: &str) -> anyhow::Result<()> {
        let (enabled, flag) = match exec_type {
            "local" => (self.local_enabled, "local_enabled"),
            "ssh" => (self.ssh_enabled, "ssh_enabled"),
            "docker" => (self.docker_enabled, "docker_enabled"),
            "wasm" => (self.wasm_enabled, "wasm_enabled"),
            _ => return Ok(()),
        };
        if !enabled {
            anyhow::bail!(
                "{exec_type} engine is disabled on this runner \
                 (set engine.{flag} = true in runner.toml)."
            );
        }
        Ok(())
    }
}

fn default_labels() -> Vec<String> {
    vec!["default".into()]
}
fn default_work_dir() -> String {
    "/tmp/buz-runner".into()
}
fn default_manager_url() -> String {
    "http://127.0.0.1:8080".into()
}
fn default_runner_name() -> String {
    "buz-runner".into()
}
fn default_true() -> bool {
    true
}
fn default_false() -> bool {
    false
}

impl RunnerIdentity {
    pub fn version(&self) -> &str {
        env!("CARGO_PKG_VERSION")
    }
}

pub fn load(config_path: Option<&str>) -> anyhow::Result<Config> {
    let config_path = config_path.unwrap_or("conf/runner.toml");

    let config = config::Config::builder()
        .add_source(config::File::with_name(config_path).required(false))
        .add_source(config::Environment::with_prefix("BUZ").separator("__"))
        .build()?;

    let mut cfg: Config = config.try_deserialize()?;

    if let Ok(url) = std::env::var("BUZ_MANAGER_URL") {
        cfg.manager.url = url;
    }
    if let Ok(token) = std::env::var("BUZ_REGISTER_TOKEN") {
        cfg.manager.register_token = token;
    }

    if cfg.manager.register_token.is_empty() {
        anyhow::bail!(
            "register_token is required. Set it in conf/runner.toml [manager] section \
             or via BUZ_REGISTER_TOKEN env var.\n  \
             Get your token from Buz Manager → Runners → New Runner."
        );
    }

    Ok(cfg)
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;
