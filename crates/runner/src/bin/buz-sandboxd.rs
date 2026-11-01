//! `buz-sandboxd` — host-side job container/VM supervisor.
//!
//! Configuration model (design record: sandbox-host-configuration):
//!
//! * **Bootstrap** is host-local and comes from flags / environment: where to
//!   listen, where the job workspaces live, which Runner UID may connect, which
//!   Docker CLI to drive, this instance's identity, and how to reach the
//!   Manager that owns it. The Manager cannot know these facts.
//! * **Policy** (resource profiles, tier images/runtimes, network) comes
//!   exclusively from the Manager over HTTPS with the per-instance token. It
//!   is validated and applied atomically, then cached root-owned as
//!   last-known-good. There is no `sandboxd.toml` and no built-in default
//!   image or envelope: until a document is applied, job creation is refused
//!   while the health check keeps passing.

use std::path::PathBuf;

use clap::Parser;

use runner::sandbox::{self, Bootstrap, ConfigState};

#[derive(Parser)]
#[command(name = "buz-sandboxd", about = "Host-side job container/VM supervisor")]
struct Args {
    /// Run the Compose/systemd health check against the local socket and exit.
    #[arg(long)]
    healthcheck: bool,

    /// Unix socket the Runner connects to.
    #[arg(
        long,
        env = "BUZ_SANDBOXD_SOCKET",
        default_value = "/run/buz-sandboxd/sandboxd.sock"
    )]
    socket: String,

    /// Root under which job workspaces must live.
    #[arg(
        long,
        env = "BUZ_SANDBOXD_WORKSPACE_ROOT",
        default_value = "/var/lib/buz/workspaces"
    )]
    workspace_root: String,

    /// Docker named volume mounted at `workspace_root` (Compose deployments).
    #[arg(long, env = "BUZ_SANDBOXD_WORKSPACE_VOLUME")]
    workspace_volume: Option<String>,

    /// Docker CLI used to drive the daemon.
    #[arg(
        long,
        env = "BUZ_SANDBOXD_DOCKER_CLI",
        default_value = "/usr/bin/docker"
    )]
    docker_cli: String,

    /// Group that may connect to the socket (the Runner's GID).
    #[arg(long, env = "BUZ_SANDBOXD_SOCKET_GID", default_value_t = 10001)]
    socket_gid: u32,

    /// The only UID accepted on the socket (the Runner's UID). Required.
    #[arg(long, env = "BUZ_SANDBOXD_ALLOWED_RUNNER_UID")]
    allowed_runner_uid: Option<u32>,

    /// This sandboxd instance's identity; unique per Docker daemon.
    /// Defaults to `sandboxd-<host_id>` for the single-instance case.
    #[arg(long, env = "BUZ_SANDBOXD_INSTANCE_ID")]
    instance_id: Option<String>,

    /// Manager base URL (e.g. https://buz.example.com). Empty = offline/cache-only.
    #[arg(long, env = "BUZ_MANAGER_URL", default_value = "")]
    manager_url: String,

    /// Expected Manager control-plane identity; a response from a different
    /// Manager is rejected. Strongly recommended in production.
    #[arg(long, env = "BUZ_MANAGER_INSTANCE_ID", default_value = "")]
    manager_instance_id: String,

    /// File holding the per-instance bearer token (`buzsd_…`), mode 0400.
    /// Preferred over the environment so the credential never shows up in
    /// `docker inspect` or process listings.
    #[arg(long, env = "BUZ_SANDBOXD_TOKEN_FILE")]
    token_file: Option<PathBuf>,

    /// Token via environment (labs / tests). The file wins when both are set.
    #[arg(
        long,
        env = "BUZ_SANDBOXD_TOKEN",
        default_value = "",
        hide_env_values = true
    )]
    token: String,

    /// Accept invalid TLS certificates from the Manager. Labs only.
    #[arg(long, env = "BUZ_SANDBOXD_TLS_SKIP_VERIFY", default_value_t = false)]
    tls_skip_verify: bool,

    /// Directory for the root-owned last-known-good config cache.
    #[arg(
        long,
        env = "BUZ_SANDBOXD_STATE_DIR",
        default_value = "/var/lib/buz-sandboxd"
    )]
    state_dir: PathBuf,
}

fn read_token(args: &Args) -> anyhow::Result<String> {
    if let Some(path) = &args.token_file {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("cannot read token file {}: {e}", path.display()))?;
        let token = raw.trim().to_string();
        anyhow::ensure!(!token.is_empty(), "token file {} is empty", path.display());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(meta) = std::fs::metadata(path) {
                if meta.permissions().mode() & 0o077 != 0 {
                    tracing::warn!(
                        path = %path.display(),
                        "sandboxd token file is readable by group/others; use mode 0400"
                    );
                }
            }
        }
        return Ok(token);
    }
    Ok(args.token.trim().to_string())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    if args.healthcheck {
        return sandbox::healthcheck(&args.socket).await;
    }
    let _guard = shared::logging::init("buz_sandboxd", &shared::logging::LoggingConfig::default());

    // Host identity (env > machine-id > hostname) and this instance's id.
    let (host_id, host_source) = match shared::instance::resolve_host_id("BUZ_HOST_ID") {
        Ok(resolved) => resolved,
        Err(error) => {
            tracing::warn!(error = %error, "host id unresolved; reporting 'unknown'");
            ("unknown".to_string(), shared::instance::IdSource::Hostname)
        }
    };
    let instance_id = match &args.instance_id {
        Some(raw) => shared::instance::validate(raw)
            .map_err(|e| anyhow::anyhow!("invalid BUZ_SANDBOXD_INSTANCE_ID: {e}"))?,
        None => shared::instance::validate(&format!("sandboxd-{host_id}"))
            .map_err(|e| anyhow::anyhow!("cannot derive instance id from host id: {e}"))?,
    };
    if !args.manager_instance_id.trim().is_empty() {
        shared::instance::validate(&args.manager_instance_id)
            .map_err(|e| anyhow::anyhow!("invalid BUZ_MANAGER_INSTANCE_ID: {e}"))?;
    }

    let token = read_token(&args)?;
    let manager_url = args.manager_url.trim().trim_end_matches('/').to_string();
    if !manager_url.is_empty() {
        anyhow::ensure!(
            !token.is_empty(),
            "a Manager URL is configured but no sandboxd token was provided \
             (set BUZ_SANDBOXD_TOKEN_FILE to the file issued at enrollment)"
        );
        if manager_url.starts_with("http://") && !args.tls_skip_verify {
            tracing::warn!(
                "manager URL uses plain http — acceptable only on a host-local Compose network"
            );
        }
        if args.manager_instance_id.trim().is_empty() {
            tracing::warn!(
                "BUZ_MANAGER_INSTANCE_ID is not pinned; any Manager presenting a valid token will be trusted"
            );
        }
    } else {
        tracing::warn!(
            "no Manager URL configured: running from the cached configuration only (no updates)"
        );
    }

    let bootstrap = Bootstrap {
        instance_id: instance_id.clone(),
        host_id: host_id.clone(),
        socket: args.socket.clone(),
        workspace_root: args.workspace_root.clone(),
        workspace_volume: args
            .workspace_volume
            .clone()
            .filter(|v| !v.trim().is_empty()),
        docker_cli: args.docker_cli.clone(),
        socket_gid: args.socket_gid,
        allowed_runner_uid: args.allowed_runner_uid,
        manager_url,
        manager_instance_id: args.manager_instance_id.trim().to_string(),
        token,
        tls_skip_verify: args.tls_skip_verify,
        state_dir: args.state_dir.clone(),
    };

    // Last-known-good cache: lets the supervisor serve jobs through a Manager
    // outage. A cache from another Manager/host/instance is refused.
    let initial = match sandbox::manager_fetch::load_cache(&bootstrap) {
        Ok(Some(applied)) => {
            tracing::info!(
                version = applied.version,
                applied_at = %applied.applied_at,
                "loaded cached sandboxd configuration"
            );
            ConfigState::new(Some(applied))
        }
        Ok(None) => {
            tracing::info!("no cached sandboxd configuration; waiting for the Manager");
            ConfigState::new(None)
        }
        Err(error) => {
            tracing::warn!(error = %error, "ignoring unusable sandboxd configuration cache");
            ConfigState::new(None)
        }
    };

    tracing::info!(
        instance = %instance_id,
        host_id = %host_id,
        host_source = host_source.as_str(),
        "sandbox supervisor starting"
    );
    sandbox::serve(bootstrap, initial).await
}
