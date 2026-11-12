use serde::Deserialize;

use shared::logging::LoggingConfig;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub server: ServerConfig,
    #[serde(default)]
    pub database: DatabaseConfig,
    /// `[cluster] mode` — single (default): this process is the only Manager
    /// serving its storage and takes an exclusive lock on it; multi: several
    /// Managers serve active-active, which requires the Postgres backend.
    #[serde(default)]
    pub cluster: ClusterConfig,
    #[serde(default)]
    pub auth: AuthConfig,
    #[serde(default)]
    pub runner: RunnerConfig,
    #[serde(default)]
    pub pipeline: PipelineConfig,
    #[serde(default)]
    pub logging: LoggingConfig,
    /// Dev-only: loopback tokenless register for pre-authorized runner names.
    /// Production must leave `enabled = false` (default).
    #[serde(default)]
    pub standalone: StandaloneConfig,
    /// `[plugin_index]` — where `uses:` marketplace references resolve.
    /// The pre-rename `[step_index]` section is NOT accepted: it would parse as
    /// an unknown field and be silently ignored, so deployments must rename it.
    #[serde(default)]
    pub plugin_index: PluginIndexConfig,
    #[serde(default)]
    pub storage: StorageConfig,
    /// Public plugin-marketplace URL linked from the Dashboard's YAML editor.
    /// Empty → the built-in default. Override for internal-mirror or
    /// air-gapped deployments (env: `BUZ_MARKETPLACE_URL`).
    #[serde(default)]
    pub marketplace_url: String,
}

/// `[plugin_index]` — where `uses:` marketplace references resolve (ADR §6).
#[derive(Debug, Clone, Deserialize)]
pub struct PluginIndexConfig {
    /// Local step-template directory (`[plugin_index.local]` in the ADR).
    /// Empty → `<storage.data_dir>/plugins`.
    #[serde(default)]
    pub local_dir: String,
    /// Path to a `reviews.json` manifest (ADR §6.7.4): templates whose
    /// content sha256 is listed are marked officially reviewed. Empty → no
    /// review checking (all resolutions count as unreviewed, allowed).
    #[serde(default)]
    pub reviewed_manifest: String,
    /// Strict mode: reject resolving templates whose hash is not in the
    /// reviewed manifest. Only meaningful with `reviewed_manifest` set.
    #[serde(default)]
    pub require_reviewed: bool,
    /// Per-request timeout for fetching remote source indexes/templates.
    /// Single source of truth: platform_settings::DEFAULT_STEP_SYNC_TIMEOUT_SECS
    /// is the serde default; the settings UI can override per deployment.
    #[serde(default = "default_sync_timeout_seconds")]
    pub sync_timeout_seconds: u64,
    /// Base64 Ed25519 public key the Manager trusts for source indexes.
    /// Empty → no trust anchor; signatures are not checked.
    #[serde(default)]
    pub index_public_key: String,
    /// Strict mode: a sync must carry a signature that verifies against
    /// `index_public_key`, otherwise the source is refused.
    #[serde(default)]
    pub require_signed_index: bool,
}

fn default_sync_timeout_seconds() -> u64 {
    crate::services::platform_settings::DEFAULT_STEP_SYNC_TIMEOUT_SECS
}

impl Default for PluginIndexConfig {
    fn default() -> Self {
        Self {
            local_dir: String::new(),
            reviewed_manifest: String::new(),
            require_reviewed: false,
            sync_timeout_seconds:
                crate::services::platform_settings::DEFAULT_STEP_SYNC_TIMEOUT_SECS,
            index_public_key: String::new(),
            require_signed_index: false,
        }
    }
}

/// `[storage]` — where the manager keeps mutable data (SQLite db, plugin
/// library). Single anchor for every data-path default in the config.
#[derive(Debug, Clone, Deserialize)]
pub struct StorageConfig {
    #[serde(default = "default_data_dir")]
    pub data_dir: String,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            data_dir: default_data_dir(),
        }
    }
}

fn default_data_dir() -> String {
    "./data".into()
}

/// `[standalone]` — same-container / loopback Runner pairing (dev only).
#[derive(Debug, Clone, Deserialize)]
pub struct StandaloneConfig {
    #[serde(default)]
    pub enabled: bool,
    /// Runner names created at boot when missing (= Manager authorization).
    #[serde(default = "default_standalone_auto_approve")]
    pub auto_approve: Vec<String>,
}

impl Default for StandaloneConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            auto_approve: default_standalone_auto_approve(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerConfig {
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    /// Path to TLS certificate PEM file (enables HTTPS when both cert and key are set)
    #[serde(default)]
    pub tls_cert: Option<String>,
    /// Path to TLS private key PEM file
    #[serde(default)]
    pub tls_key: Option<String>,
    /// Public UI origin for deep links (e.g. notify cards → Job Detail).
    /// Example: `https://buz.example.com` — no trailing slash required.
    /// Empty → notify cards omit the Job Detail button.
    #[serde(default)]
    pub public_base_url: String,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: default_host(),
            port: default_port(),
            tls_cert: None,
            tls_key: None,
            public_base_url: String::new(),
        }
    }
}

impl ServerConfig {
    pub fn tls_enabled(&self) -> bool {
        self.tls_cert.is_some() && self.tls_key.is_some()
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct DatabaseConfig {
    #[serde(default = "default_db_path")]
    pub path: String,
    /// Explicit connection target — `sqlite:<path>` today; `postgres:<…>` is
    /// refused at startup until the PG backend lands. Empty/absent keeps the
    /// legacy `[database] path` working (it stays the compatibility alias).
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default = "default_max_connections")]
    pub max_connections: u32,
}

/// Which backend the configured database target points at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageKind {
    Sqlite,
    Postgres,
}

impl DatabaseConfig {
    /// `url` wins over the legacy `path`; a scheme we do not serve is an error
    /// surfaced by the startup gate, not silently treated as SQLite.
    pub fn storage_kind(&self) -> Result<StorageKind, String> {
        match &self.url {
            None => Ok(StorageKind::Sqlite),
            Some(url) => {
                let scheme = url.trim().to_lowercase();
                if scheme.starts_with("sqlite:") {
                    Ok(StorageKind::Sqlite)
                } else if scheme.starts_with("postgres:") || scheme.starts_with("postgresql:") {
                    Ok(StorageKind::Postgres)
                } else {
                    Err(format!(
                        "unsupported database url scheme: {url} (expected sqlite: or postgres:)"
                    ))
                }
            }
        }
    }
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            path: default_db_path(),
            url: None,
            max_connections: default_max_connections(),
        }
    }
}

/// `[cluster] mode` — `single` (default): this process is the only Manager
/// serving its storage and holds an exclusive lock on it; `multi`: several
/// Managers serve active-active against one Postgres backend.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ClusterMode {
    #[default]
    Single,
    Multi,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ClusterConfig {
    #[serde(default)]
    pub mode: ClusterMode,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AuthConfig {
    #[serde(default)]
    pub jwt_secret: String,
    #[serde(default = "default_jwt_ttl_hours")]
    pub jwt_ttl_hours: i64,
    #[serde(default = "default_token_ttl_hours")]
    pub runner_register_token_ttl_hours: i64,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            jwt_secret: String::new(),
            jwt_ttl_hours: default_jwt_ttl_hours(),
            runner_register_token_ttl_hours: default_token_ttl_hours(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct RunnerConfig {
    #[serde(default = "default_heartbeat_timeout")]
    pub heartbeat_timeout_seconds: i64,
}

impl Default for RunnerConfig {
    fn default() -> Self {
        Self {
            heartbeat_timeout_seconds: default_heartbeat_timeout(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct PipelineConfig {
    #[serde(default = "default_yaml_scan_dir")]
    pub yaml_scan_dir: String,
    #[serde(default = "default_yaml_scan_interval")]
    pub yaml_scan_interval_seconds: u64,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            yaml_scan_dir: default_yaml_scan_dir(),
            yaml_scan_interval_seconds: default_yaml_scan_interval(),
        }
    }
}

// -- default value functions --

fn default_host() -> String {
    "0.0.0.0".into()
}
fn default_port() -> u16 {
    8080
}
fn default_db_path() -> String {
    "./data/buz.db".into()
}
fn default_max_connections() -> u32 {
    8
}
fn default_token_ttl_hours() -> i64 {
    24
}
fn default_jwt_ttl_hours() -> i64 {
    24
}
fn default_heartbeat_timeout() -> i64 {
    90
}
fn default_yaml_scan_dir() -> String {
    "./pipelines".into()
}
fn default_yaml_scan_interval() -> u64 {
    30
}
fn default_standalone_auto_approve() -> Vec<String> {
    vec!["standalone-runner".into()]
}

/// Minimum accepted length for a production JWT secret (bytes as chars).
pub const MIN_JWT_SECRET_LEN: usize = 32;

/// Returns `true` when the secret is empty, too short, or a known template/demo value.
pub fn is_insecure_jwt_secret(secret: &str) -> bool {
    let s = secret.trim();
    if s.is_empty() || s.len() < MIN_JWT_SECRET_LEN {
        return true;
    }
    let lower = s.to_ascii_lowercase();
    lower.contains("changeme")
        || lower.contains("change_me")
        || lower.contains("change-me")
        || lower.contains("demo-only")
        || lower.contains("replace_with")
        || lower.contains("replace-with")
        || lower == "buz-dev-secret-change-in-production"
}

fn allow_insecure_jwt_from_env() -> bool {
    match std::env::var("BUZ_ALLOW_INSECURE_JWT") {
        Ok(v) => {
            let v = v.trim().to_ascii_lowercase();
            matches!(v.as_str(), "1" | "true" | "yes" | "on")
        }
        Err(_) => false,
    }
}

/// Refuse weak / placeholder JWT secrets unless `BUZ_ALLOW_INSECURE_JWT` is set.
pub fn validate_jwt_secret(secret: &str) -> anyhow::Result<()> {
    if !is_insecure_jwt_secret(secret) {
        return Ok(());
    }
    if allow_insecure_jwt_from_env() {
        eprintln!(
            "warning: JWT secret is empty, shorter than {MIN_JWT_SECRET_LEN} chars, or looks like a \
             template/demo value. Continuing because BUZ_ALLOW_INSECURE_JWT is set — never use this in production."
        );
        return Ok(());
    }
    anyhow::bail!(
        "refusing to start: auth.jwt_secret is missing, shorter than {MIN_JWT_SECRET_LEN} characters, \
         or a known placeholder (CHANGE_ME / changeme / demo-only / …). \
         Set a strong secret, e.g. `openssl rand -hex 32`, via auth.jwt_secret or BUZ_JWT_SECRET. \
         Local/demo only: export BUZ_ALLOW_INSECURE_JWT=1"
    )
}

// -- loader --

/// Load configuration for the server: JWT secret validation is enforced.
pub fn load(config_path: Option<&str>) -> anyhow::Result<Config> {
    let cfg = load_unchecked(config_path)?;
    validate_jwt_secret(&cfg.auth.jwt_secret)?;
    Ok(cfg)
}

/// Load configuration **without** JWT secret validation.
///
/// Used by CLI subcommands (`gen-runner-token`) that only need to reach the
/// local database. The JWT secret is required to decrypt stored secrets, but
/// runner registration itself does not need it.
pub fn load_unchecked(config_path: Option<&str>) -> anyhow::Result<Config> {
    let config_path = config_path.unwrap_or("conf/manager.toml");

    let config = config::Config::builder()
        .add_source(config::File::with_name(config_path).required(false))
        .add_source(config::Environment::with_prefix("BUZ").separator("__"))
        .build()?;

    // The pre-rename section is not supported. Deserialization ignores it, so
    // say so loudly: a silently dropped review gate is worse than a startup
    // warning, and this keeps the "no compatibility" decision from becoming a
    // silent data-loss trap.
    // Printed rather than logged: this runs before the log subscriber exists.
    if config.get_table("step_index").is_ok() {
        eprintln!(
            "warning: {config_path} has a [step_index] section — it is IGNORED since the plugin \
rename; rename it to [plugin_index] (same keys) to keep local_dir / reviewed_manifest / \
require_reviewed"
        );
    }

    let mut cfg: Config = config.try_deserialize()?;

    // Override auth defaults from env if set
    if let Ok(secret) = std::env::var("BUZ_JWT_SECRET") {
        cfg.auth.jwt_secret = secret;
    }

    Ok(cfg)
}
