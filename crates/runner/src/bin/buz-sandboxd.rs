use clap::Parser;
use serde::Deserialize;

use runner::sandbox;

#[derive(Parser)]
#[command(name = "buz-sandboxd", about = "Host-side job container/VM supervisor")]
struct Args {
    #[arg(short, long, default_value = "conf/sandboxd.toml")]
    config: String,
    #[arg(long)]
    healthcheck: bool,
}

#[derive(Deserialize)]
struct FileConfig {
    sandboxd: sandbox::SandboxdConfig,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    if args.healthcheck {
        return sandbox::healthcheck("/run/buz-sandboxd/sandboxd.sock").await;
    }
    let raw = config::Config::builder()
        .add_source(config::File::with_name(&args.config))
        .build()?;
    let mut parsed: FileConfig = raw.try_deserialize()?;
    if let Ok(volume) = std::env::var("BUZ_SANDBOXD_WORKSPACE_VOLUME") {
        parsed.sandboxd.workspace_volume = Some(volume);
    }
    if let Ok(instance) = std::env::var("BUZ_SANDBOXD_INSTANCE_ID") {
        parsed.sandboxd.instance_id = instance;
    }
    let _guard = shared::logging::init("buz_sandboxd", &shared::logging::LoggingConfig::default());

    // Host identity for log/audit correlation (env > machine-id > hostname).
    // Never fatal: an unresolvable host id must not stop the supervisor.
    let (host_id, host_source) = match shared::instance::resolve_host_id("BUZ_SANDBOXD_HOST_ID") {
        Ok(resolved) => resolved,
        Err(error) => {
            tracing::warn!(error = %error, "host id unresolved; reporting 'unknown'");
            ("unknown".to_string(), shared::instance::IdSource::Hostname)
        }
    };
    let instance_id = parsed.sandboxd.instance_id.clone();
    let _ = shared::instance::validate(&instance_id)
        .map_err(|error| tracing::warn!(%error, %instance_id, "sandboxd instance id is not a valid identity"));
    tracing::info!(
        instance = %instance_id,
        host_id = %host_id,
        host_source = host_source.as_str(),
        "sandbox supervisor starting"
    );

    sandbox::serve(parsed.sandboxd).await
}
