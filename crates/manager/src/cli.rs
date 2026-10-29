//! Command-line surface for `buz-manager`.
//!
//! The types and subcommand implementations live in the library so they can be
//! exercised from `tests/`; `main` only wires them up.

use clap::{Parser, Subcommand};

use crate::api;
use crate::config;
use crate::db;
use crate::rbac;

/// Buz Manager — lightweight cross‑network orchestration control plane.
#[derive(Parser)]
#[command(
    name = "buz-manager",
    version = shared::version::VERSION,
    about = "Buz Manager — orchestration control plane"
)]
pub struct Cli {
    /// Path to TOML configuration file
    #[arg(short, long, global = true, default_value = "conf/manager.toml")]
    pub config: String,

    /// Optional disk UI override (default: assets embedded in this binary).
    #[arg(short = 'f', long, env = "BUZ_FRONTEND_DIR")]
    pub frontend: Option<String>,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand)]
pub enum Command {
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

    /// Validate plugin spec YAML files (marketplace ADR §5.1).
    ///
    /// Intended for buz-plugins repo CI: exits non-zero when any file fails
    /// so golden tests can gate merges. Works without a Manager config or
    /// database — pass spec files or directories containing *.yaml/*.yml.
    PluginValidate {
        /// Plugin spec files or directories (directories are scanned
        /// recursively for *.yaml / *.yml; hidden paths and symlinks are
        /// skipped)
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
pub enum PipelineCommand {
    /// Resolve marketplace steps and print a non-executing plan.
    Plan {
        /// Pipeline YAML file to inspect
        #[arg(value_name = "PIPELINE")]
        pipeline: std::path::PathBuf,
        /// Directory containing local step-template YAML files
        #[arg(long, value_name = "DIR")]
        plugin_index: std::path::PathBuf,
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

/// `plugin-validate` — validate plugin spec YAML files against the
/// marketplace ADR §5.1 spec. Requires no Manager config or database, so
/// buz-plugins repo CI can run it standalone.
pub fn plugin_validate(paths: &[String]) -> anyhow::Result<()> {
    if paths.is_empty() {
        anyhow::bail!("usage: buz-manager plugin-validate <spec.yaml | dir> [...]");
    }
    let current = env!("CARGO_PKG_VERSION");

    let mut files: Vec<std::path::PathBuf> = Vec::new();
    let mut roots = Vec::new();
    for path in paths {
        let meta = std::fs::symlink_metadata(path)
            .map_err(|e| anyhow::anyhow!("cannot access '{path}': {e}"))?;
        if meta.file_type().is_symlink() {
            anyhow::bail!("'{path}' is a symlink; plugin specs must be regular files");
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
                "directory '{}' contains no *.yaml/*.yml plugin specs",
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
        let spec = match serde_yaml::from_str::<shared::plugin_spec::PluginSpec>(&raw) {
            Ok(spec) => spec,
            Err(error) => {
                failed += 1;
                eprintln!("INVALID  {}\n  - YAML: {error}", file.display());
                continue;
            }
        };

        let mut errors = shared::plugin_spec::validate_plugin_spec(&spec, current)
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
        anyhow::bail!(
            "{failed} of {} plugin spec(s) failed validation",
            files.len()
        );
    }
    println!("{} plugin spec(s) valid", files.len());
    Ok(())
}

/// `gen-runner-token` — create a runner in the local database and print its
/// one-shot register token for automation (e.g. provisioning runner.toml).
///
/// Requires a platform-admin JWT (via `--token` or `BUZ_ADMIN_TOKEN`): only
/// someone who can log into the Manager web UI as a platform admin may
/// provision runners.
pub async fn gen_runner_token(
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
        claim_strategy: None,
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
