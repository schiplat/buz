use serde::Deserialize;

use shared::logging::LoggingConfig;
use shared::protocol::RunnerExecutionMode;

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Config {
    /// Local runner identity + Manager connection (`[runner]`).
    #[serde(default)]
    pub runner: RunnerSection,
    #[serde(default)]
    pub engine: EngineConfig,
    #[serde(default)]
    pub clock: ClockSection,
    #[serde(default)]
    pub logging: LoggingConfig,
    /// Manager-selected job-wide executor; older policy snapshots default to Host.
    #[serde(default)]
    pub execution_mode: RunnerExecutionMode,
    /// Machine-local sandbox runtime settings. Never sent to Manager.
    #[serde(default)]
    pub sandbox: SandboxSection,
}

/// `[sandbox]` — host-local wiring to the sandbox supervisor.
///
/// Only *where* to reach the supervisor; *what* to run (tier images, runtimes,
/// resource profiles) is the Manager's applied document — see the
/// sandbox-host-configuration design record. Old configs' `container_image` /
/// `sandbox_runtime` / `firecracker_*` keys are ignored on load.
#[derive(Debug, Clone, Deserialize)]
pub struct SandboxSection {
    /// Local socket exposed by the privileged, host-side buz-sandboxd service.
    #[serde(default = "default_sandbox_socket")]
    pub supervisor_socket: String,
    /// Workspace root accepted by the host-side supervisor.
    #[serde(default = "default_workspace_dir")]
    pub workspace_root: String,
    #[serde(default = "default_sandbox_timeout")]
    pub request_timeout_seconds: u64,
    /// Observational copy of sandboxd's applied configuration. Written (0600,
    /// atomic) only when the applied hash changes; never read by sandboxd.
    #[serde(default = "default_sandbox_snapshot_path")]
    pub snapshot_path: String,
}

fn default_sandbox_snapshot_path() -> String {
    "./state/sandboxd.snapshot.json".into()
}

impl Default for SandboxSection {
    fn default() -> Self {
        Self {
            supervisor_socket: default_sandbox_socket(),
            workspace_root: default_workspace_dir(),
            request_timeout_seconds: default_sandbox_timeout(),
            snapshot_path: default_sandbox_snapshot_path(),
        }
    }
}

fn default_sandbox_socket() -> String {
    "/run/buz-sandboxd/sandboxd.sock".into()
}

fn default_sandbox_timeout() -> u64 {
    30
}

/// `[clock]` — Runner↔Manager clock consistency.
///
/// The Runner must not drift from the Manager clock: it stamps local config
/// backups and all its timing must line up with Manager-side job logs. At
/// startup (and on every heartbeat) the Runner compares its clock with the
/// Manager's `Date` header; a gross skew aborts startup instead of silently
/// producing off-timeline output.
#[derive(Debug, Clone, Deserialize)]
pub struct ClockSection {
    /// Run the startup check and keep re-checking on heartbeats.
    #[serde(default = "default_clock_enabled")]
    pub enabled: bool,
    /// Warn when the Runner clock is at least this far behind the Manager.
    #[serde(default = "default_clock_warn_threshold")]
    pub warn_threshold_seconds: i64,
    /// Hard limit: skew at or beyond this aborts startup (see `fail_on_skew`).
    #[serde(default = "default_clock_max_skew")]
    pub max_skew_seconds: i64,
    /// `true` (default): refuse to start when the skew exceeds
    /// `max_skew_seconds`. `false`: log an error and continue.
    #[serde(default = "default_true")]
    pub fail_on_skew: bool,
}

impl Default for ClockSection {
    fn default() -> Self {
        Self {
            enabled: default_clock_enabled(),
            warn_threshold_seconds: default_clock_warn_threshold(),
            max_skew_seconds: default_clock_max_skew(),
            fail_on_skew: true,
        }
    }
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
    /// Machine-local Docker bind-mount policy; never overlaid by Manager policy.
    /// Omitted keeps existing deployments compatible. New templates opt into strict.
    #[serde(default)]
    pub docker_mount_policy: DockerMountPolicy,
    /// Authorize access to the locally configured daemon socket. A capability is
    /// a request, not a grant; even legacy mode checks this for new requests.
    #[serde(default)]
    pub docker_socket_allowed: bool,
    #[serde(default = "default_docker_socket_path")]
    pub docker_socket_path: String,
    /// Local persistent file for the Manager-issued Runner JWT.
    #[serde(default = "default_token_cache_path")]
    pub token_cache_path: String,
    /// This Runner installation's deployment identity — resolved at startup
    /// (`BUZ_RUNNER_INSTANCE_ID` > persisted state file > generated) and sent
    /// on register/heartbeat. Normally left empty in TOML; setting it here is
    /// ignored in favour of the env variable.
    #[serde(default)]
    pub instance_id: String,
    /// Host machine identity — resolved at startup (`BUZ_HOST_ID` >
    /// machine-id > hostname) and sent on register/heartbeat. Normally left
    /// empty in TOML; setting it here is ignored in favour of the env variable.
    #[serde(default)]
    pub host_id: String,
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
            docker_mount_policy: DockerMountPolicy::Legacy,
            docker_socket_allowed: false,
            docker_socket_path: default_docker_socket_path(),
            token_cache_path: default_token_cache_path(),
            instance_id: String::new(),
            host_id: String::new(),
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
    /// Machine-local: scope the Docker CLI config to the job workspace
    /// (`per_job`, default) or keep the legacy host-global `~/.docker`
    /// (`shared`). Not UI-managed — never overridden by Manager policy.
    #[serde(default)]
    pub docker_config_isolation: DockerConfigIsolation,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum SshHostKeyPolicy {
    Insecure,
    #[default]
    KnownHosts,
}

/// Where the Docker CLI writes/reads `config.json` for job steps.
///
/// `per_job` (default) scopes credentials to `{workspace}/{job_id}/.docker`:
/// logins (core or marketplace template) never touch the host-global
/// `~/.docker/config.json`, and the directory is removed when the job ends —
/// success, failure, or cancel. `shared` restores the legacy host-global
/// behavior for environments that rely on a pre-configured credential store.
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum DockerConfigIsolation {
    #[default]
    PerJob,
    Shared,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            shell_enabled: true,
            ssh_enabled: true,
            docker_enabled: false,
            wasm_enabled: false,
            // Secure by default: verify host keys against known_hosts
            // (~/.ssh/known_hosts when no explicit path is set).
            ssh_host_key_policy: SshHostKeyPolicy::KnownHosts,
            ssh_known_hosts_path: None,
            docker_config_isolation: DockerConfigIsolation::PerJob,
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
fn default_docker_socket_path() -> String {
    "/var/run/docker.sock".into()
}
fn default_token_cache_path() -> String {
    "./state/runner-token".into()
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
fn default_clock_enabled() -> bool {
    true
}
fn default_clock_warn_threshold() -> i64 {
    60
}
fn default_clock_max_skew() -> i64 {
    300
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum DockerMountPolicy {
    /// Preserve explicit bind mounts for existing Runner deployments.
    #[default]
    Legacy,
    /// Permit only Runner-owned workspace and explicitly authorized socket mounts.
    Strict,
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
