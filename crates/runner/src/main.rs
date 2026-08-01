use anyhow::Context;
use clap::{Parser, Subcommand};

mod client;
mod config;
mod engine;
mod masker;
mod policy;
mod secret;
mod sysinfo;

/// Buz Runner — lightweight cross‑network task execution agent.
#[derive(Parser)]
#[command(
    name = "buz-runner",
    version = shared::version::VERSION,
    about = "Buz Runner — task execution agent"
)]
struct Cli {
    /// Path to TOML configuration file
    #[arg(short, long, global = true, default_value = "conf/runner.toml")]
    config: String,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Print version information and exit
    Version,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    if matches!(cli.command, Some(Command::Version)) {
        println!("{}", shared::version::RUNNER_LONG_VERSION);
        return Ok(());
    }

    let config = config::load(Some(&cli.config)).context("failed to load config")?;
    let _log_guard = shared::logging::init("buz_runner", &config.logging);

    tracing::info!(
        target: "buz_runner",
        runner = %config.runner.name,
        manager = %config.runner.manager_url,
        "{} starting",
        shared::version::long_version("runner"),
    );

    run_loop(config).await
}

async fn run_loop(config: config::Config) -> anyhow::Result<()> {
    let mut client_builder = reqwest::Client::builder();

    if config.runner.tls_skip_verify {
        client_builder = client_builder.danger_accept_invalid_certs(true);
        tracing::warn!("TLS certificate verification is DISABLED — do not use in production");
    }

    let http_client = client_builder.build()?;
    let started_at = std::time::Instant::now();

    let register = client::register::register_with_center(&http_client, &config).await?;
    let runner_token = register.token;
    let shared_policy = policy::new_shared(register.policy);
    tracing::info!(
        max_concurrent = policy::max_concurrent(&shared_policy),
        "registered with Manager"
    );

    {
        let info = sysinfo::collect_system_info(
            started_at,
            &sysinfo::SysInfoOpts {
                manager_url: config.runner.manager_url.clone(),
                prefer_interfaces: config.runner.prefer_interfaces.clone(),
            },
        );
        tracing::info!(
            version = %info.runner_version,
            host = %info.hostname,
            arch = %info.cpu_arch,
            cores = info.cpu_cores,
            ram_mb = info.memory_total_mb,
            disk_gb = info.disk_total_gb,
            "host inventory"
        );
    }

    let hb_config = config.clone();
    let hb_client = http_client.clone();
    let hb_token = runner_token.clone();
    let hb_policy = shared_policy.clone();
    let heartbeat_handle = tokio::spawn(async move {
        client::heartbeat::run_heartbeat_loop(
            &hb_client, &hb_config, &hb_token, started_at, hb_policy,
        )
        .await
    });

    client::poll::run_poll_loop(&http_client, &config, &runner_token, shared_policy).await?;

    heartbeat_handle.abort();
    Ok(())
}
