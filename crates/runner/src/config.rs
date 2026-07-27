use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub manager: ManagerConfig,
    #[serde(default)]
    pub node: NodeConfig,
    #[serde(default)]
    pub engine: EngineConfig,
    #[serde(default)]
    pub vault: VaultConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ManagerConfig {
    #[serde(default = "default_manager_url")]
    pub url: String,
    #[serde(default)]
    pub register_token: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct NodeConfig {
    #[serde(default = "default_node_name")]
    pub name: String,
    #[serde(default = "default_labels")]
    pub labels: Vec<String>,
    #[serde(default = "default_work_dir")]
    pub work_dir: String,
}

impl Default for ManagerConfig {
    fn default() -> Self {
        Self {
            url: default_manager_url(),
            register_token: String::new(),
        }
    }
}

impl Default for NodeConfig {
    fn default() -> Self {
        Self {
            name: default_node_name(),
            labels: default_labels(),
            work_dir: default_work_dir(),
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
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            local_enabled: true,
            ssh_enabled: true,
            docker_enabled: false,
            wasm_enabled: false,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct VaultConfig {
    pub url: Option<String>,
    pub token: Option<String>,
}

fn default_labels() -> Vec<String> { vec!["default".into()] }
fn default_work_dir() -> String { "/tmp/buz-runner".into() }
fn default_manager_url() -> String { "http://127.0.0.1:8080".into() }
fn default_node_name() -> String { "buz-runner".into() }
fn default_true() -> bool { true }
fn default_false() -> bool { false }

impl NodeConfig {
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
             Get your token from Buz Manager → Nodes → New Node."
        );
    }

    Ok(cfg)
}
