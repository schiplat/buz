use serde::Deserialize;

use shared::logging::LoggingConfig;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub server: ServerConfig,
    #[serde(default)]
    pub database: DatabaseConfig,
    #[serde(default)]
    pub auth: AuthConfig,
    #[serde(default)]
    pub runner: RunnerConfig,
    #[serde(default)]
    pub pipeline: PipelineConfig,
    #[serde(default)]
    pub clock: ClockConfig,
    #[serde(default)]
    pub logging: LoggingConfig,
    /// Dev-only: loopback tokenless register for pre-authorized runner names.
    /// Production must leave `enabled = false` (default).
    #[serde(default)]
    pub standalone: StandaloneConfig,
    #[serde(default)]
    pub step_index: StepIndexConfig,
    /// Public plugin-marketplace URL linked from the Dashboard's YAML editor.
    /// Empty → the built-in default. Override for internal-mirror or
    /// air-gapped deployments (env: `BUZ_MARKETPLACE_URL`).
    #[serde(default)]
    pub marketplace_url: String,
}

/// `[step_index]` — where `uses:` marketplace references resolve (ADR §6).
#[derive(Debug, Clone, Deserialize)]
pub struct StepIndexConfig {
    /// Local step-template directory (`[step_index.local]` in the ADR).
    /// Empty → `<pipeline.yaml_scan_dir>/steps`.
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
}

fn default_sync_timeout_seconds() -> u64 {
    crate::services::platform_settings::DEFAULT_STEP_SYNC_TIMEOUT_SECS
}

impl Default for StepIndexConfig {
    fn default() -> Self {
        Self {
            local_dir: String::new(),
            reviewed_manifest: String::new(),
            require_reviewed: false,
            sync_timeout_seconds:
                crate::services::platform_settings::DEFAULT_STEP_SYNC_TIMEOUT_SECS,
        }
    }
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
    #[serde(default = "default_max_connections")]
    pub max_connections: u32,
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            path: default_db_path(),
            max_connections: default_max_connections(),
        }
    }
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

/// `[clock]` — Manager host clock-skew guard.
///
/// `config_source` URLs are signed with an **absolute** `Expires`, so a Manager
/// clock that is behind real time produces URLs that are already expired when
/// the Runner downloads them (OSS answers `403 AccessDenied: Request has
/// expired`). The guard samples the OSS endpoint's `Date` header before signing
/// and refuses with an actionable error instead of dispatching a doomed URL.
#[derive(Debug, Clone, Deserialize)]
pub struct ClockConfig {
    /// Probe the remote time source before signing config URLs.
    #[serde(default = "default_clock_enabled")]
    pub enabled: bool,
    /// Warn when the Manager clock is at least this many seconds behind remote
    /// time. `0` disables the warning.
    #[serde(default = "default_clock_warn_threshold")]
    pub warn_threshold_seconds: i64,
    /// Reuse a probe result for this long (seconds). `0` probes every dispatch.
    #[serde(default = "default_clock_cache_ttl")]
    pub cache_ttl_seconds: u64,
    /// Hosts the guard is allowed to probe, e.g. `["*.aliyuncs.com"]`.
    ///
    /// The probe target is derived from a team-scoped `config_source` secret
    /// (`https://{bucket}.{endpoint}/`), so without an allowlist any secret
    /// owner could aim the Manager's HTTP client at an arbitrary internal host
    /// (SSRF). A derived host that matches none of these patterns is **never
    /// dialed**; the guard is skipped and signing proceeds unguarded. Empty
    /// list disables probing entirely.
    #[serde(default = "default_clock_allowed_hosts")]
    pub allowed_hosts: Vec<String>,
}

impl Default for ClockConfig {
    fn default() -> Self {
        Self {
            enabled: default_clock_enabled(),
            warn_threshold_seconds: default_clock_warn_threshold(),
            cache_ttl_seconds: default_clock_cache_ttl(),
            allowed_hosts: default_clock_allowed_hosts(),
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
fn default_clock_enabled() -> bool {
    true
}
fn default_clock_warn_threshold() -> i64 {
    60
}
fn default_clock_cache_ttl() -> u64 {
    300
}
/// Default probe allowlist: Aliyun OSS domains only (`*.aliyuncs.com`).
fn default_clock_allowed_hosts() -> Vec<String> {
    vec!["*.aliyuncs.com".to_string()]
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

    let mut cfg: Config = config.try_deserialize()?;

    // Override auth defaults from env if set
    if let Ok(secret) = std::env::var("BUZ_JWT_SECRET") {
        cfg.auth.jwt_secret = secret;
    }

    Ok(cfg)
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;

#[cfg(test)]
mod sync_timeout_default_tests {
    use super::*;

    /// A manager.toml whose [step_index] section exists but omits the field
    /// must still get the semantic default (60), not the numeric zero from a
    /// derived Default.
    #[test]
    fn missing_sync_timeout_in_toml_gets_semantic_default() {
        let cfg: Config = config::Config::builder()
            .add_source(config::File::from_str(
                "[step_index]\n",
                config::FileFormat::Toml,
            ))
            .build()
            .expect("build config")
            .try_deserialize()
            .expect("deserialize config");
        assert_eq!(cfg.step_index.sync_timeout_seconds, 60);
    }

    #[test]
    fn no_step_index_section_at_all_gets_semantic_default() {
        // Same code path as config::load (config-rs, not toml::from_str).
        let cfg: Config = config::Config::builder()
            .add_source(config::File::from_str(
                r#"
[server]
host = "127.0.0.1"
"#,
                config::FileFormat::Toml,
            ))
            .build()
            .expect("build config")
            .try_deserialize()
            .expect("deserialize config");
        assert_eq!(cfg.step_index.sync_timeout_seconds, 60);
    }
}
