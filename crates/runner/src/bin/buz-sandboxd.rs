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
    sandbox::serve(parsed.sandboxd).await
}
