use clap::Parser;

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
    about = "Buz Manager — orchestration control plane",
    long_version = shared::version::VERSION
)]
struct Cli {
    /// Path to TOML configuration file
    #[arg(short, long, default_value = "conf/manager.toml")]
    config: String,

    /// Optional disk UI override (default: assets embedded in this binary).
    #[arg(short = 'f', long, env = "BUZ_FRONTEND_DIR")]
    frontend: Option<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
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
