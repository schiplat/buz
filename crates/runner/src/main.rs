use anyhow::Context;
use clap::Parser;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

mod client;
mod config;
mod engine;
mod masker;
mod secret;
mod sysinfo;

/// Buz Runner — lightweight cross‑network task execution agent.
#[derive(Parser)]
#[command(
    name = "buz-runner",
    version = shared::version::VERSION,
    about = "Buz Runner — task execution agent",
    long_version = shared::version::VERSION
)]
struct Cli {
    /// Path to TOML configuration file
    #[arg(short, long, default_value = "conf/runner.toml")]
    config: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "buz_runner=info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let config = config::load(Some(&cli.config)).context("failed to load config")?;
    tracing::info!(
        "{} starting: {} ({})",
        shared::version::long_version("runner"),
        config.node.name,
        config.node.version()
    );

    run_loop(config).await
}

async fn run_loop(config: config::Config) -> anyhow::Result<()> {
    let mut client_builder = reqwest::Client::builder();

    if config.manager.tls_skip_verify {
        client_builder = client_builder.danger_accept_invalid_certs(true);
        tracing::warn!("TLS certificate verification is DISABLED — do not use this in production");
    }

    let http_client = client_builder.build()?;
    let started_at = std::time::Instant::now();

    let runner_token = client::register::register_with_center(&http_client, &config).await?;
    tracing::info!("Registered with Manager, got token");

    // Log initial system info once for visibility
    {
        let info = sysinfo::collect_system_info(
            started_at,
            &sysinfo::SysInfoOpts {
                manager_url: config.manager.url.clone(),
                prefer_interfaces: config.node.prefer_interfaces.clone(),
            },
        );
        tracing::info!(
            "System: v{} | {} ({}) | {} cores | {} MB RAM | {} GB disk",
            info.runner_version, info.hostname, info.cpu_arch,
            info.cpu_cores, info.memory_total_mb, info.disk_total_gb,
        );
    }

    let hb_config = config.clone();
    let hb_client = http_client.clone();
    let hb_token = runner_token.clone();
    let heartbeat_handle = tokio::spawn(async move {
        client::heartbeat::run_heartbeat_loop(&hb_client, &hb_config, &hb_token, started_at).await
    });

    client::poll::run_poll_loop(&http_client, &config, &runner_token).await?;

    heartbeat_handle.abort();
    Ok(())
}
