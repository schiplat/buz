use clap::Parser;
use manager::cli::{gen_runner_token, plugin_validate, Cli, Command, PipelineCommand};

use axum::{routing::get, Router};
use std::net::SocketAddr;
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;

use manager::{api, config, db, frontend, pipeline_plan, rbac, services};

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
        Some(Command::PluginValidate { paths }) => {
            return plugin_validate(&paths);
        }
        Some(Command::Pipeline {
            command:
                PipelineCommand::Plan {
                    pipeline,
                    plugin_index,
                    reviews,
                    require_reviewed,
                    offline,
                },
        }) => {
            return pipeline_plan::run(
                &pipeline,
                &plugin_index,
                reviews.as_deref(),
                require_reviewed,
                offline,
            )
            .await;
        }
        None => {}
    }

    let config = config::load(Some(&cli.config))?;
    let _log_guard = shared::logging::init("buz_manager", &config.logging);

    let db_pool = db::init(&config).await?;
    db::run_migrations(&db_pool, &config.auth.jwt_secret).await?;
    rbac::bootstrap(&db_pool).await?;
    api::runners::ensure_standalone_auto_approve(&db_pool, &config).await?;
    api::version::mark_started();

    tracing::info!(
        host = %config.server.host,
        port = config.server.port,
        "{} starting",
        shared::version::long_version("manager"),
    );

    // Clock-skew guard for signed config URLs: a slow Manager clock produces
    // URLs that are already expired when the Runner downloads them.
    services::clock::init(&config.clock);

    // Step-index settings for `uses:` resolution (index dir + review gate).
    // Admin-edited overrides in platform_settings take precedence over
    // manager.toml and are re-applied at runtime by the settings API.
    let settings_overrides = services::platform_settings::fetch_overrides(&db_pool).await;
    let require_reviewed = settings_overrides
        .get(services::platform_settings::KEY_PLUGIN_REVIEW_REQUIRED)
        .and_then(|v| v.parse::<bool>().ok())
        .unwrap_or(config.plugin_index.require_reviewed);
    let local_dir_override = settings_overrides
        .get(services::platform_settings::KEY_PLUGIN_INDEX_LOCAL_DIR)
        .cloned();
    std::fs::create_dir_all(&config.storage.data_dir).ok();

    // One-time layout migration: the local plugin directory default was
    // relocated from `<yaml_scan_dir>/steps` (original) to the data root
    // (v1.10.3+). Rename an existing legacy directory so existing
    // deployments keep resolving their templates.
    {
        let data_root = std::path::Path::new(&config.storage.data_dir);
        let target = data_root.join("plugins");
        let mut legacy: Option<std::path::PathBuf> = None;
        for candidate in [
            std::path::Path::new(&config.pipeline.yaml_scan_dir).join("steps"),
            std::path::Path::new(&config.pipeline.yaml_scan_dir).join("plugins"),
        ] {
            // Relative scan roots resolve against the manager working dir.
            let candidate = if candidate.is_absolute() {
                candidate
            } else {
                std::env::current_dir().unwrap_or_default().join(candidate)
            };
            if candidate.is_dir() {
                legacy = Some(candidate);
                break;
            }
        }
        if let Some(legacy) = legacy {
            std::fs::create_dir_all(data_root).ok();
            if !target.exists() {
                match std::fs::rename(&legacy, &target) {
                    Ok(()) => tracing::info!(
                        from = %legacy.display(),
                        to = %target.display(),
                        "migrated legacy local plugin directory into the data root"
                    ),
                    Err(e) => tracing::warn!(
                        from = %legacy.display(),
                        to = %target.display(),
                        error = %e,
                        "could not migrate the legacy local plugin directory — resolve manually"
                    ),
                }
            }
        }
    }

    let sync_timeout_secs = settings_overrides
        .get(services::platform_settings::KEY_PLUGIN_SYNC_TIMEOUT)
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(config.plugin_index.sync_timeout_seconds);
    let reviewed_manifest_override = settings_overrides
        .get(services::platform_settings::KEY_PLUGIN_REVIEWED_MANIFEST)
        .cloned();
    // Empty config value → the reviews.json beside the plugin directory
    // (data root): a buz-plugins clone in the data volume provides both.
    let reviewed_manifest = services::plugin_resolver::effective_reviewed_manifest(
        &config.storage.data_dir,
        &config.plugin_index.reviewed_manifest,
        reviewed_manifest_override.as_deref(),
    )
    .to_string_lossy()
    .into_owned();
    tracing::info!(
        require_signed_index = config.plugin_index.require_signed_index,
        public_key_configured = !config.plugin_index.index_public_key.is_empty(),
        "index signature defaults (manager.toml; the dashboard can override)"
    );
    services::plugin_resolver::init_signature_defaults(
        config.plugin_index.require_signed_index,
        config.plugin_index.index_public_key.clone(),
    );
    services::plugin_resolver::init(services::plugin_resolver::PluginIndexSettings {
        reviewed_manifest: reviewed_manifest.clone(),
        require_reviewed,
    });
    if require_reviewed != config.plugin_index.require_reviewed
        || local_dir_override.is_some()
        || reviewed_manifest_override.is_some()
    {
        services::plugin_resolver::apply_runtime_overrides(
            Some(require_reviewed),
            local_dir_override,
            Some(reviewed_manifest),
        );
    }

    // Startup sync of plugin sources (design §6.8: startup + manual refresh;
    // no background timer by default). Failures are logged, never fatal.
    {
        let pool = db_pool.clone();
        tokio::spawn(async move {
            let synced = services::plugin_pool::sync_all_enabled(
                &pool,
                &services::plugin_pool::http_client(sync_timeout_secs),
                shared::version::VERSION,
            )
            .await;
            if synced > 0 {
                tracing::info!(sources = synced, "plugin sources synced at startup");
            }
        });
    }
    // Review-gate posture: the gate is silent by default, so say so loudly at
    // boot — an operator who never configured a manifest must know unreviewed
    // templates resolve freely (dashboard repeats this as a banner).
    if config.plugin_index.reviewed_manifest.trim().is_empty() {
        tracing::warn!(
            "step review gate is NOT enabled: [plugin_index] reviewed_manifest is empty — \
             every `uses:` template resolves without review checks; production deployments \
             should set reviewed_manifest to a reviews.json and require_reviewed = true"
        );
    } else if !config.plugin_index.require_reviewed {
        tracing::info!(
            manifest = %config.plugin_index.reviewed_manifest,
            "step review gate is advisory-only (require_reviewed = false): unreviewed \
             templates resolve with a log warning instead of being rejected"
        );
    } else {
        tracing::info!(
            manifest = %config.plugin_index.reviewed_manifest,
            "step review gate is strict (require_reviewed = true): templates absent from \
             the reviewed manifest are rejected at resolution time"
        );
    }
    tracing::info!(
        enabled = config.clock.enabled,
        warn_threshold_seconds = config.clock.warn_threshold_seconds,
        cache_ttl_seconds = config.clock.cache_ttl_seconds,
        "clock: skew guard configured (probes the OSS endpoint Date header before signing config URLs)"
    );

    services::runner_monitor::spawn_monitor(db_pool.clone(), config.runner.clone());

    // Shared log broadcaster: cleanup needs the same instance the log upload
    // path writes into so expired debug chunks are dropped from memory too.
    let log_broadcast = manager::services::log_broadcast::LogBroadcaster::new();
    services::debug_log_cleanup::spawn_debug_log_cleanup(db_pool.clone(), log_broadcast.clone());

    services::pipeline_scanner::spawn_scanner(
        db_pool.clone(),
        config.pipeline.yaml_scan_dir.clone(),
        services::plugin_resolver::effective_local_dir(
            &config.storage.data_dir,
            &config.plugin_index.local_dir,
        )?,
        config.pipeline.yaml_scan_interval_seconds,
        sync_timeout_secs,
    );

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let app_state = manager::state::AppState {
        pool: db_pool.clone(),
        config: config.clone(),
        log_broadcast,
    };

    services::cron_scheduler::spawn_scheduler(app_state.clone(), 30);
    services::github_fetch_scheduler::spawn_fetch_scheduler(app_state.clone(), 60);
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
                .route("/health", get(api::version::health))
                .route("/api/v1/openapi.json", get(api::openapi::openapi_json))
                .with_state(app_state.clone())
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
                .route("/health", get(api::version::health))
                .route("/api/v1/openapi.json", get(api::openapi::openapi_json))
                .with_state(app_state.clone())
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
            .serve(app.into_make_service_with_connect_info::<SocketAddr>())
            .await?;
    } else {
        let listener = tokio::net::TcpListener::bind(&addr).await?;
        tracing::info!(port = config.server.port, "Web UI http ready");
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await?;
    }

    Ok(())
}
