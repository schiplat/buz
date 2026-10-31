use anyhow::Context;
use clap::Parser;

use runner::cli::{print_completions, Cli, Command};
use runner::{client, config, identity, policy, sysinfo};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Some(Command::Version) => {
            println!("{}", shared::version::RUNNER_LONG_VERSION);
            return Ok(());
        }
        Some(Command::Completions { shell }) => {
            return print_completions(shell);
        }
        None => {}
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

    // GitHub Actions-style startup banner — plain stdout, always visible
    // regardless of log level, so operators can identify version + OS at a glance.
    println!("Current runner version: '{}'", shared::version::VERSION);
    println!("Operating System: {}", sysinfo::os_description());

    run_loop(config).await
}

async fn run_loop(mut config: config::Config) -> anyhow::Result<()> {
    let mut client_builder = reqwest::Client::builder();

    if config.runner.tls_skip_verify {
        client_builder = client_builder.danger_accept_invalid_certs(true);
        tracing::warn!("TLS certificate verification is DISABLED — do not use in production");
    }

    // Deployment identity: resolved once (env > state file > generated) and
    // stored back into config so register/heartbeat payloads carry it.
    identity::resolve_into(&mut config).unwrap_or_else(|error| {
        tracing::warn!(error = %error, "runner identity unresolved; register/heartbeat omit it");
    });

    let http_client = client_builder.build()?;
    let started_at = std::time::Instant::now();

    // Clock consistency with the Manager: validated before registering so a
    // mis-clocked Runner never claims work (and the heartbeat keeps re-checking).
    let clock_monitor = std::sync::Arc::new(client::clock::ClockMonitor::new(
        config.clock.clone(),
        config.runner.manager_url.clone(),
    ));
    clock_monitor.seed(client::clock::verify_at_startup(&http_client, &config).await?);

    let register = client::register::register_with_center(&http_client, &config).await?;
    let runner_token = register.token;
    // Persist before accepting work so restarts can prove this runner's identity.
    client::register::cache_token(&config, &runner_token)
        .context("failed to persist Runner credential; refusing to start job processing")?;
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
                clock_skew_seconds: clock_monitor.skew_secs(),
                execution_capabilities: None,
                sandboxd: None,
            },
        );
        tracing::info!(
            version = %info.runner_version,
            host = %info.hostname,
            arch = %info.cpu_arch,
            cores = info.cpu_cores,
            ram_mb = info.memory_total_mb,
            disk_gb = info.disk_total_gb,
            clock_skew_secs = info.clock_skew_seconds,
            "host inventory"
        );
    }

    // Shared one-shot restart signal: the heartbeat loop sets it when the Manager
    // serves a restart command; the poll loop then stops claiming new work,
    // drains in-flight jobs, and returns Ok(()) so we exit with code 0.
    let restart_requested = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

    let hb_config = config.clone();
    let hb_client = http_client.clone();
    let hb_token = runner_token.clone();
    let hb_policy = shared_policy.clone();
    let hb_restart = restart_requested.clone();
    let hb_clock = clock_monitor.clone();
    let heartbeat_handle = tokio::spawn(async move {
        client::heartbeat::run_heartbeat_loop(
            &hb_client, &hb_config, &hb_token, started_at, hb_policy, hb_restart, hb_clock,
        )
        .await
    });

    client::poll::run_poll_loop(
        &http_client,
        &config,
        &runner_token,
        shared_policy,
        restart_requested,
    )
    .await?;

    heartbeat_handle.abort();
    Ok(())
}
