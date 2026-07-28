use serde::Deserialize;

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
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: default_host(),
            port: default_port(),
            tls_cert: None,
            tls_key: None,
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
    #[serde(default = "default_admin_user")]
    pub admin_user: String,
    #[serde(default = "default_admin_password")]
    pub admin_password: String,
    #[serde(default = "default_token_ttl_hours")]
    pub runner_register_token_ttl_hours: i64,
    #[serde(default = "default_jwt_ttl_hours")]
    pub jwt_ttl_hours: i64,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            jwt_secret: String::new(),
            admin_user: default_admin_user(),
            admin_password: default_admin_password(),
            runner_register_token_ttl_hours: default_token_ttl_hours(),
            jwt_ttl_hours: default_jwt_ttl_hours(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct RunnerConfig {
    #[serde(default = "default_heartbeat_timeout")]
    pub heartbeat_timeout_seconds: i64,
    #[serde(default = "default_long_poll_timeout")]
    pub long_poll_timeout_seconds: i64,
}

impl Default for RunnerConfig {
    fn default() -> Self {
        Self {
            heartbeat_timeout_seconds: default_heartbeat_timeout(),
            long_poll_timeout_seconds: default_long_poll_timeout(),
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
fn default_admin_user() -> String {
    "admin".into()
}
fn default_admin_password() -> String {
    "admin".into()
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
fn default_long_poll_timeout() -> i64 {
    45
}
fn default_yaml_scan_dir() -> String {
    "./pipelines".into()
}
fn default_yaml_scan_interval() -> u64 {
    30
}

// -- loader --

pub fn load(config_path: Option<&str>) -> anyhow::Result<Config> {
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
    if let Ok(pass) = std::env::var("BUZ_ADMIN_PASSWORD") {
        cfg.auth.admin_password = pass;
    }

    // Ensure JWT secret is not default in production
    if cfg.auth.jwt_secret.is_empty() || cfg.auth.jwt_secret == "changeme" {
        tracing::warn!(
            "JWT secret is not set or is using default value. Set BUZ_JWT_SECRET in production."
        );
        cfg.auth.jwt_secret = "buz-dev-secret-change-in-production".into();
    }

    Ok(cfg)
}
