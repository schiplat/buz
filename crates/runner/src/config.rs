use serde::Deserialize;

use shared::logging::LoggingConfig;

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Config {
    /// Local runner identity + Manager connection (`[runner]`).
    #[serde(default)]
    pub runner: RunnerSection,
    #[serde(default)]
    pub engine: EngineConfig,
    #[serde(default)]
    pub logging: LoggingConfig,
}

/// `[runner]` — name, Manager URL/token, workspace.
#[derive(Debug, Clone, Deserialize)]
pub struct RunnerSection {
    #[serde(default = "default_runner_name")]
    pub name: String,
    #[serde(default = "default_manager_url")]
    pub manager_url: String,
    #[serde(default)]
    pub register_token: String,
    /// Dev-only: allow empty `register_token` when Manager runs with
    /// `[standalone] enabled = true` (loopback + pre-authorized name).
    #[serde(default)]
    pub standalone: bool,
    /// Allow invalid TLS certificates (self-signed / internal CA). Labs only.
    #[serde(default)]
    pub tls_skip_verify: bool,
    #[serde(default = "default_workspace_dir")]
    pub workspace_dir: String,
    /// Deprecated — labels are UI-managed. Kept for deserialize compatibility.
    #[serde(default)]
    #[allow(dead_code)]
    pub labels: Vec<String>,
    /// Deprecated — controlled from Manager UI.
    #[serde(default)]
    pub clean_workspace_after_job: bool,
    /// Interface names preferred when advertising IPs (multi-NIC).
    #[serde(default)]
    pub prefer_interfaces: Vec<String>,
}

impl Default for RunnerSection {
    fn default() -> Self {
        Self {
            name: default_runner_name(),
            manager_url: default_manager_url(),
            register_token: String::new(),
            standalone: false,
            tls_skip_verify: false,
            workspace_dir: default_workspace_dir(),
            labels: Vec::new(),
            clean_workspace_after_job: false,
            prefer_interfaces: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct EngineConfig {
    /// Deprecated — controlled from Manager UI.
    #[serde(default = "default_true")]
    pub shell_enabled: bool,
    #[serde(default = "default_true")]
    pub ssh_enabled: bool,
    #[serde(default = "default_false")]
    pub docker_enabled: bool,
    #[serde(default = "default_false")]
    pub wasm_enabled: bool,
    #[serde(default)]
    pub ssh_host_key_policy: SshHostKeyPolicy,
    #[serde(default)]
    pub ssh_known_hosts_path: Option<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum SshHostKeyPolicy {
    #[default]
    Insecure,
    KnownHosts,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            shell_enabled: true,
            ssh_enabled: true,
            docker_enabled: false,
            wasm_enabled: false,
            ssh_host_key_policy: SshHostKeyPolicy::Insecure,
            ssh_known_hosts_path: None,
        }
    }
}

impl EngineConfig {
    pub fn ensure_enabled(&self, exec_type: &str) -> anyhow::Result<()> {
        let (enabled, flag) = match exec_type {
            "local" | "shell" => (self.shell_enabled, "shell_enabled"),
            "ssh" => (self.ssh_enabled, "ssh_enabled"),
            "docker" => (self.docker_enabled, "docker_enabled"),
            "wasm" => (self.wasm_enabled, "wasm_enabled"),
            _ => return Ok(()),
        };
        if !enabled {
            anyhow::bail!(
                "{exec_type} engine is disabled on this runner \
                 (enable {flag} in Manager UI → Runners)."
            );
        }
        Ok(())
    }
}

fn default_workspace_dir() -> String {
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

impl RunnerSection {
    pub fn version(&self) -> &str {
        env!("CARGO_PKG_VERSION")
    }
}

pub fn load(config_path: Option<&str>) -> anyhow::Result<Config> {
    let config_path = config_path.unwrap_or("conf/runner.toml");

    let raw = std::fs::read_to_string(config_path).unwrap_or_default();
    if raw.contains("[manager]") {
        anyhow::bail!(
            "obsolete [manager] section in {config_path}. Move fields under [runner]:\n\
             \n\
             [runner]\n\
             name = \"...\"\n\
             manager_url = \"...\"\n\
             register_token = \"...\"\n\
             workspace_dir = \"...\"\n\
             # tls_skip_verify = false\n\
             \n\
             [engine]\n\
             # ssh_known_hosts_path = \"...\"\n\
             \n\
             [logging]\n\
             level = \"info\"\n\
             # file = \"./logs/runner.log\""
        );
    }

    let config = config::Config::builder()
        .add_source(config::File::with_name(config_path).required(false))
        .add_source(config::Environment::with_prefix("BUZ").separator("__"))
        .build()?;

    let mut cfg: Config = config.try_deserialize()?;

    if let Ok(url) = std::env::var("BUZ_MANAGER_URL") {
        cfg.runner.manager_url = url;
    }
    if let Ok(token) = std::env::var("BUZ_REGISTER_TOKEN") {
        cfg.runner.register_token = token;
    }
    if let Ok(v) = std::env::var("BUZ_STANDALONE") {
        let v = v.trim().to_ascii_lowercase();
        if matches!(v.as_str(), "1" | "true" | "yes" | "on") {
            cfg.runner.standalone = true;
        }
    }

    if cfg.runner.register_token.is_empty() && !cfg.runner.standalone {
        anyhow::bail!(
            "register_token is required under [runner] (or BUZ_REGISTER_TOKEN).\n  \
             Get the token from Buz Manager → Runners → New Runner.\n  \
             Dev standalone (same-container Manager): set standalone = true under [runner]."
        );
    }

    // Log file: omit → ./logs/runner.log; set "" / "-" / "none" → stdout only.
    match cfg.logging.file.as_deref().map(str::trim) {
        None => cfg.logging.file = Some("./logs/runner.log".into()),
        Some("") | Some("-") | Some("none") => cfg.logging.file = None,
        Some(_) => {}
    }

    Ok(cfg)
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;
