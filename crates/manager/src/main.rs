use clap::{Parser, Subcommand};

use axum::Router;
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;

mod api;
mod audit;
mod config;
mod crypto;
mod db;
mod frontend;
mod middleware;
mod password;
mod rbac;
mod services;
mod ssh_probe;
mod state;

/// Buz Manager — lightweight cross‑network orchestration control plane.
#[derive(Parser)]
#[command(
    name = "buz-manager",
    version = shared::version::VERSION,
    about = "Buz Manager — orchestration control plane"
)]
struct Cli {
    /// Path to TOML configuration file
    #[arg(short, long, global = true, default_value = "conf/manager.toml")]
    config: String,

    /// Optional disk UI override (default: assets embedded in this binary).
    #[arg(short = 'f', long, env = "BUZ_FRONTEND_DIR")]
    frontend: Option<String>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Print version information and exit
    Version,

    /// Create a runner locally and print its one-shot register token
    /// (useful for automating runner provisioning without the web UI).
    GenRunnerToken {
        /// Runner name (must match the name used in runner.toml)
        name: String,

        /// Comma-separated labels, e.g. "linux-x64,docker"
        #[arg(long)]
        labels: Option<String>,

        /// Platform-admin credential: a login JWT or an API token (Access ->
        /// API tokens). Falls back to the BUZ_ADMIN_TOKEN environment variable.
        #[arg(long, env = "BUZ_ADMIN_TOKEN")]
        token: String,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Some(Command::Version) => {
            println!("{}", shared::version::MANAGER_LONG_VERSION);
            return Ok(());
        }
        Some(Command::GenRunnerToken {
            name,
            labels,
            token,
        }) => {
            return gen_runner_token(&cli.config, &name, labels.as_deref(), &token).await;
        }
        None => {}
    }

    let config = config::load(Some(&cli.config))?;
    let _log_guard = shared::logging::init("buz_manager", &config.logging);

    let db_pool = db::init(&config).await?;
    db::run_migrations(&db_pool, &config.auth.jwt_secret).await?;
    rbac::bootstrap(&db_pool).await?;

    tracing::info!(
        host = %config.server.host,
        port = config.server.port,
        "{} starting",
        shared::version::long_version("manager"),
    );

    services::runner_monitor::spawn_monitor(db_pool.clone(), config.runner.clone());

    services::pipeline_scanner::spawn_scanner(
        db_pool.clone(),
        config.pipeline.yaml_scan_dir.clone(),
        config.pipeline.yaml_scan_interval_seconds,
    );

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let app_state = crate::state::AppState {
        pool: db_pool.clone(),
        config: config.clone(),
        log_broadcast: crate::services::log_broadcast::LogBroadcaster::new(),
    };

    services::cron_scheduler::spawn_scheduler(app_state.clone(), 30);
    services::scheduler::start_scheduler(db_pool.clone());

    let app = match cli
        .frontend
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(dir) => {
            tracing::info!(path = %dir, "serving UI from disk");
            Router::new()
                .nest("/api/v1", api::router(app_state.clone()))
                .fallback_service(frontend::disk_fallback(dir))
                .layer(TraceLayer::new_for_http())
                .layer(cors)
        }
        None => {
            tracing::info!(
                files = frontend::embedded_file_count(),
                "serving UI from embedded assets"
            );
            Router::new()
                .nest("/api/v1", api::router(app_state.clone()))
                .fallback(frontend::embedded_handler)
                .layer(TraceLayer::new_for_http())
                .layer(cors)
        }
    };

    let addr = format!("{}:{}", config.server.host, config.server.port);

    if config.server.tls_enabled() {
        use axum_server::tls_rustls::RustlsConfig;

        let tls_config = RustlsConfig::from_pem_file(
            config.server.tls_cert.as_ref().unwrap(),
            config.server.tls_key.as_ref().unwrap(),
        )
        .await
        .map_err(|e| anyhow::anyhow!("failed to load TLS certificate: {e}"))?;

        tracing::info!(port = config.server.port, "Web UI https ready");
        axum_server::bind_rustls(addr.parse().unwrap(), tls_config)
            .serve(app.into_make_service())
            .await?;
    } else {
        let listener = tokio::net::TcpListener::bind(&addr).await?;
        tracing::info!(port = config.server.port, "Web UI http ready");
        axum::serve(listener, app).await?;
    }

    Ok(())
}

/// `gen-runner-token` — create a runner in the local database and print its
/// one-shot register token for automation (e.g. provisioning runner.toml).
///
/// Requires a platform-admin JWT (via `--token` or `BUZ_ADMIN_TOKEN`): only
/// someone who can log into the Manager web UI as a platform admin may
/// provision runners.
async fn gen_runner_token(
    config_path: &str,
    name: &str,
    labels: Option<&str>,
    admin_token: &str,
) -> anyhow::Result<()> {
    if admin_token.trim().is_empty() {
        anyhow::bail!(
            "an admin token is required: pass --token <JWT|API-token> or set BUZ_ADMIN_TOKEN \
             (obtain one by logging into the web UI, from the auth/login API, \
             or create an API token in Access -> API tokens)"
        );
    }

    let config = config::load_unchecked(Some(config_path))?;
    let pool = db::init(&config).await?;
    db::run_migrations(&pool, &config.auth.jwt_secret).await?;

    // Verify the caller is a platform admin before touching the runners table.
    rbac::verify_cli_admin_token(&pool, &config.auth.jwt_secret, admin_token)
        .await
        .map_err(|e| anyhow::anyhow!("token verification failed: {e}"))?;

    let labels = labels.map(|l| {
        l.split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect()
    });

    let req = shared::protocol::CreateRunnerRequest {
        name: name.to_string(),
        labels,
        version: Some(shared::version::VERSION.to_string()),
        max_concurrent_jobs: None,
        clean_workspace_after_job: None,
        shell_enabled: None,
        ssh_enabled: None,
        docker_enabled: None,
        wasm_enabled: None,
        ssh_host_key_policy: None,
    };

    let created = match api::runners::create_runner_record(&pool, &req).await {
        Ok(c) => c,
        Err(e)
            if e.to_string()
                .contains("UNIQUE constraint failed: runners.name") =>
        {
            anyhow::bail!(
                "runner '{name}' already exists — pick a different name or delete it in the web UI"
            );
        }
        Err(e) => return Err(anyhow::anyhow!("failed to create runner: {e}")),
    };

    println!("name: {}", created.name);
    println!("register_token: {}", created.register_token);
    Ok(())
}
