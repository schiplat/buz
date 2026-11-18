use serde::Deserialize;

use shared::logging::LoggingConfig;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub server: ServerConfig,
    #[serde(default)]
    pub database: DatabaseConfig,
    /// `[cluster] mode` — single (default): this process is the only Manager
    /// serving its storage and takes an exclusive lock on it. `multi` is
    /// parsed for forward compatibility but refused at startup by this build.
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
    /// Explicit connection target. `sqlite:<path>` is the default build;
    /// `postgres:` requires a binary built with the `postgres` feature (the
    /// startup gate fails closed otherwise). Empty/absent keeps the legacy
    /// `[database] path` working as the compatibility alias.
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

    /// The SQLite file this config points at: the `sqlite:<path>` url when
    /// set, otherwise the legacy `[database] path`. Callers that open the
    /// file directly (instance lock, key rotation, the SQLite→PG migration)
    /// must use this — reading `path` alone ignores the url override and can
    /// operate on a stale database. Only meaningful for
    /// [`StorageKind::Sqlite`].
    pub fn sqlite_path(&self) -> String {
        match &self.url {
            Some(url) if url.trim().to_lowercase().starts_with("sqlite:") => url.trim()
                [("sqlite:".len())..]
                .trim_start_matches("//")
                .to_string(),
            _ => self.path.clone(),
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
/// serving its storage and holds an exclusive lock on it. `multi` is parsed
/// for forward compatibility but **refused at startup by this build**; the
/// startup gate keeps every deployment on the single-writer model.
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
    /// `[auth.oidc]` — generic OIDC single sign-on (any standards-compliant
    /// provider). Disabled by default; local login is unaffected either way.
    #[serde(default)]
    pub oidc: OidcConfig,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            jwt_secret: String::new(),
            jwt_ttl_hours: default_jwt_ttl_hours(),
            runner_register_token_ttl_hours: default_token_ttl_hours(),
            oidc: OidcConfig::default(),
        }
    }
}

/// `[auth.oidc]` — generic OIDC provider configuration. There is nothing
/// provider-specific here: the issuer drives discovery
/// (`{issuer}/.well-known/openid-configuration`) and everything else is
/// standard protocol.
#[derive(Debug, Clone, Deserialize)]
pub struct OidcConfig {
    #[serde(default)]
    pub enabled: bool,
    /// HTTPS origin without a trailing slash (plain http allowed only for
    /// localhost development). Drives discovery; must equal the IdP's
    /// `issuer` claim.
    #[serde(default)]
    pub issuer: String,
    #[serde(default)]
    pub client_id: String,
    /// Inline client secret. Resolution order when empty: the
    /// `BUZ_OIDC_CLIENT_SECRET` env var, then `client_secret_file`.
    #[serde(default)]
    pub client_secret: String,
    /// Read-once alternative to `client_secret` for deployments that keep
    /// secrets in files rather than configs.
    #[serde(default)]
    pub client_secret_file: Option<String>,
    /// Absolute callback URL, character-for-character equal to the redirect
    /// URI registered at the provider.
    #[serde(default)]
    pub redirect_uri: String,
    #[serde(default = "default_oidc_scopes")]
    pub scopes: Vec<String>,
    /// Create a plain Buz user on first sign-in of an unbound identity
    /// (no teams, no admin flags). When false, unbound identities are
    /// refused with a message pointing at an administrator.
    #[serde(default = "default_oidc_auto_provision")]
    pub auto_provision: bool,
}

impl Default for OidcConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            issuer: String::new(),
            client_id: String::new(),
            client_secret: String::new(),
            client_secret_file: None,
            redirect_uri: String::new(),
            scopes: default_oidc_scopes(),
            auto_provision: default_oidc_auto_provision(),
        }
    }
}

fn default_oidc_scopes() -> Vec<String> {
    vec!["openid".into(), "profile".into(), "email".into()]
}

fn default_oidc_auto_provision() -> bool {
    true
}

impl OidcConfig {
    /// Resolve the client secret: inline value, then the
    /// `BUZ_OIDC_CLIENT_SECRET` env var, then `client_secret_file`.
    pub fn resolved_client_secret(&self) -> Result<String, String> {
        if !self.client_secret.trim().is_empty() {
            return Ok(self.client_secret.trim().to_string());
        }
        if let Ok(from_env) = std::env::var(OIDC_CLIENT_SECRET_ENV) {
            if !from_env.trim().is_empty() {
                return Ok(from_env.trim().to_string());
            }
        }
        if let Some(path) = self.client_secret_file.as_deref() {
            if !path.trim().is_empty() {
                let from_file = std::fs::read_to_string(path.trim())
                    .map_err(|e| format!("cannot read client_secret_file {path}: {e}"))?;
                return Ok(from_file.trim().to_string());
            }
        }
        Err("no client secret: set [auth.oidc] client_secret, \
             BUZ_OIDC_CLIENT_SECRET, or client_secret_file"
            .into())
    }

    /// Validate everything the sign-in flow needs. `Err` carries the first
    /// problem; the caller disables SSO only (never the server).
    pub fn validate(&self) -> Result<(), String> {
        if !self.enabled {
            return Ok(());
        }
        let issuer = self.issuer.trim();
        if issuer.is_empty() {
            return Err("[auth.oidc] issuer is required when enabled".into());
        }
        if issuer.ends_with('/') {
            return Err("[auth.oidc] issuer must not end with '/'".into());
        }
        let is_local_http = issuer.starts_with("http://127.0.0.1")
            || issuer.starts_with("http://localhost")
            || issuer.starts_with("http://[::1]");
        if !issuer.starts_with("https://") && !is_local_http {
            return Err("[auth.oidc] issuer must be https (http allowed only for \
                        localhost development)"
                .into());
        }
        if self.client_id.trim().is_empty() {
            return Err("[auth.oidc] client_id is required when enabled".into());
        }
        self.resolved_client_secret()?;
        let redirect = self.redirect_uri.trim();
        if redirect.is_empty() {
            return Err("[auth.oidc] redirect_uri is required when enabled".into());
        }
        if !redirect.starts_with("https://") && !redirect.starts_with("http://") {
            return Err("[auth.oidc] redirect_uri must be an absolute http(s) URL".into());
        }
        if !self.scopes.iter().any(|s| s == "openid") {
            return Err("[auth.oidc] scopes must include 'openid'".into());
        }
        Ok(())
    }
}

/// Environment fallback for the OIDC client secret (`[auth.oidc]`).
pub const OIDC_CLIENT_SECRET_ENV: &str = "BUZ_OIDC_CLIENT_SECRET";

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
