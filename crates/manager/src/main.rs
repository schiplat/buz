use clap::{Parser, Subcommand};

use axum::{routing::get, Router};
use std::net::SocketAddr;
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;

mod api;
mod audit;
mod config;
mod crypto;
mod db;
mod frontend;
mod image_registry;
mod middleware;
mod password;
mod pipeline_plan;
mod rbac;
mod services;
mod ssh_probe;
mod state;

#[cfg(test)]
#[path = "main_tests.rs"]
mod main_tests;

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

    /// Validate buz-plugin step template YAML files (marketplace ADR §5.1).
    ///
    /// Intended for buz-plugins repo CI: exits non-zero when any file fails
    /// so golden tests can gate merges. Works without a Manager config or
    /// database — pass spec files or directories containing *.yaml/*.yml.
    StepValidate {
        /// Step spec files or directories (directories are scanned recursively
        /// for *.yaml / *.yml; hidden paths and symlinks are skipped)
        #[arg(value_name = "PATH")]
        paths: Vec<String>,
    },

    /// Work with a pipeline definition without starting Manager services.
    Pipeline {
        #[command(subcommand)]
        command: PipelineCommand,
    },
}

#[derive(Subcommand)]
enum PipelineCommand {
    /// Resolve marketplace steps and print a non-executing plan.
    Plan {
        /// Pipeline YAML file to inspect
        #[arg(value_name = "PIPELINE")]
        pipeline: std::path::PathBuf,
        /// Directory containing local step-template YAML files
        #[arg(long, value_name = "DIR")]
        step_index: std::path::PathBuf,
        /// Optional reviewed-template manifest (reviews.json)
        #[arg(long, value_name = "FILE")]
        reviews: Option<std::path::PathBuf>,
        /// Fail if any resolved template is absent from --reviews
        #[arg(long)]
        require_reviewed: bool,
        /// Skip remote Docker/OCI image availability checks
        #[arg(long)]
        offline: bool,
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
        Some(Command::StepValidate { paths }) => {
            return step_validate(&paths);
        }
        Some(Command::Pipeline {
            command:
                PipelineCommand::Plan {
                    pipeline,
                    step_index,
                    reviews,
                    require_reviewed,
                    offline,
                },
        }) => {
            return pipeline_plan::run(
                &pipeline,
                &step_index,
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
        .get(services::platform_settings::KEY_STEP_REVIEW_REQUIRED)
        .and_then(|v| v.parse::<bool>().ok())
        .unwrap_or(config.step_index.require_reviewed);
    let local_dir_override = settings_overrides
        .get(services::platform_settings::KEY_STEP_INDEX_LOCAL_DIR)
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
        .get(services::platform_settings::KEY_STEP_SYNC_TIMEOUT)
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(config.step_index.sync_timeout_seconds);
    let reviewed_manifest_override = settings_overrides
        .get(services::platform_settings::KEY_STEP_REVIEWED_MANIFEST)
        .cloned();
    // Empty config value → the reviews.json beside the plugin directory
    // (data root): a buz-plugins clone in the data volume provides both.
    let reviewed_manifest = services::step_resolver::effective_reviewed_manifest(
        &config.storage.data_dir,
        &config.step_index.reviewed_manifest,
        reviewed_manifest_override.as_deref(),
    )
    .to_string_lossy()
    .into_owned();
    services::step_resolver::init(services::step_resolver::StepIndexSettings {
        reviewed_manifest: reviewed_manifest.clone(),
        require_reviewed,
    });
    if require_reviewed != config.step_index.require_reviewed
        || local_dir_override.is_some()
        || reviewed_manifest_override.is_some()
    {
        services::step_resolver::apply_runtime_overrides(
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
            let synced = services::step_pool::sync_all_enabled(
                &pool,
                &services::step_pool::http_client(sync_timeout_secs),
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
    if config.step_index.reviewed_manifest.trim().is_empty() {
        tracing::warn!(
            "step review gate is NOT enabled: [step_index] reviewed_manifest is empty — \
             every `uses:` template resolves without review checks; production deployments \
             should set reviewed_manifest to a reviews.json and require_reviewed = true"
        );
    } else if !config.step_index.require_reviewed {
        tracing::info!(
            manifest = %config.step_index.reviewed_manifest,
            "step review gate is advisory-only (require_reviewed = false): unreviewed \
             templates resolve with a log warning instead of being rejected"
        );
    } else {
        tracing::info!(
            manifest = %config.step_index.reviewed_manifest,
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
    let log_broadcast = crate::services::log_broadcast::LogBroadcaster::new();
    services::debug_log_cleanup::spawn_debug_log_cleanup(db_pool.clone(), log_broadcast.clone());

    services::pipeline_scanner::spawn_scanner(
        db_pool.clone(),
        config.pipeline.yaml_scan_dir.clone(),
        services::step_resolver::effective_local_dir(
            &config.storage.data_dir,
            &config.step_index.local_dir,
        )?,
        config.pipeline.yaml_scan_interval_seconds,
        sync_timeout_secs,
    );

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let app_state = crate::state::AppState {
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

/// `step-validate` — validate buz-plugin step template YAML files against
/// the marketplace ADR §5.1 spec. Requires no Manager config or database,
/// so buz-plugins repo CI can run it standalone.
fn step_validate(paths: &[String]) -> anyhow::Result<()> {
    if paths.is_empty() {
        anyhow::bail!("usage: buz-manager step-validate <spec.yaml | dir> [...]");
    }
    let current = env!("CARGO_PKG_VERSION");

    let mut files: Vec<std::path::PathBuf> = Vec::new();
    let mut roots = Vec::new();
    for path in paths {
        let meta = std::fs::symlink_metadata(path)
            .map_err(|e| anyhow::anyhow!("cannot access '{path}': {e}"))?;
        if meta.file_type().is_symlink() {
            anyhow::bail!("'{path}' is a symlink; step specs must be regular files");
        } else if meta.is_dir() {
            roots.push(std::path::PathBuf::from(path));
        } else {
            files.push(std::path::PathBuf::from(path));
        }
    }

    for root in roots {
        let entries = shared::yaml_files::discover_yaml_files(&root)?;
        if entries.is_empty() {
            anyhow::bail!(
                "directory '{}' contains no *.yaml/*.yml step specs",
                root.display()
            );
        }
        files.extend(entries);
    }
    files.sort();
    files.dedup();

    let mut seen_specs = std::collections::HashMap::<(String, String), String>::new();
    let mut failed = 0usize;
    for file in &files {
        let raw = std::fs::read_to_string(file)
            .map_err(|e| anyhow::anyhow!("cannot read '{}': {e}", file.display()))?;
        let spec = match serde_yaml::from_str::<shared::step_spec::StepSpec>(&raw) {
            Ok(spec) => spec,
            Err(error) => {
                failed += 1;
                eprintln!("INVALID  {}\n  - YAML: {error}", file.display());
                continue;
            }
        };

        let mut errors = shared::step_spec::validate_step_spec(&spec, current)
            .err()
            .unwrap_or_default();
        if let Some(previous) = seen_specs.insert(
            (spec.id.clone(), spec.version.clone()),
            file.display().to_string(),
        ) {
            errors.push(format!(
                "duplicate id/version {}@{} also declared in {}",
                spec.id, spec.version, previous
            ));
        }

        if errors.is_empty() {
            println!("OK       {} ({}@{})", file.display(), spec.id, spec.version);
        } else {
            failed += 1;
            eprintln!("INVALID  {}", file.display());
            for error in errors {
                eprintln!("  - {error}");
            }
        }
    }

    if failed > 0 {
        anyhow::bail!("{failed} of {} step spec(s) failed validation", files.len());
    }
    println!("{} step spec(s) valid", files.len());
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
        team_id: None,
        execution_mode: None,
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
