use clap::Parser;

use axum::Router;
use tower_http::cors::{Any, CorsLayer};
use tower_http::services::{ServeDir, ServeFile};
use tower_http::trace::TraceLayer;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

mod api;
mod config;
mod db;
mod middleware;
mod services;

/// Buz Manager — lightweight cross‑network orchestration control plane.
#[derive(Parser)]
#[command(name = "buz-manager", version, about)]
struct Cli {
    /// Path to TOML configuration file
    #[arg(short, long, default_value = "conf/manager.toml")]
    config: String,

    /// Path to frontend static files
    #[arg(short = 'f', long, env = "BUZ_FRONTEND_DIR", default_value = "../frontend/dist")]
    frontend: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| "buz_manager=info,tower_http=info".into()))
        .with(tracing_subscriber::fmt::layer())
        .init();

    let config = config::load(Some(&cli.config))?;
    let db_pool = db::init(&config).await?;
    db::run_migrations(&db_pool).await?;

    tracing::info!("Buz Manager starting on {}:{}", config.server.host, config.server.port);

    // Start background runner-health monitor
    services::runner_monitor::spawn_monitor(db_pool.clone(), config.runner.clone());

    // Start pipeline YAML scanner
    services::pipeline_scanner::spawn_scanner(
        db_pool.clone(),
        config.pipeline.yaml_scan_dir.clone(),
        config.pipeline.yaml_scan_interval_seconds,
    );

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let frontend_dir = &cli.frontend;
    let index_html = format!("{frontend_dir}/index.html");

    let app = Router::new()
        .nest("/api/v1", api::router(db_pool.clone(), config.clone()))
        .fallback_service(
            ServeDir::new(frontend_dir).fallback(ServeFile::new(&index_html)),
        )
        .layer(TraceLayer::new_for_http())
        .layer(cors);

    let addr = format!("{}:{}", config.server.host, config.server.port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("Web UI: http://localhost:{port}", port = config.server.port);

    axum::serve(listener, app).await?;

    Ok(())
}
