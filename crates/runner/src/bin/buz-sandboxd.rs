use clap::Parser;
use serde::Deserialize;

#[path = "../sandboxd.rs"]
mod sandboxd;

#[derive(Parser)]
#[command(name = "buz-sandboxd", about = "Host-side job container/VM supervisor")]
struct Args {
    #[arg(short, long, default_value = "conf/sandboxd.toml")]
    config: String,
}

#[derive(Deserialize)]
struct FileConfig {
    sandboxd: sandboxd::SandboxdConfig,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let raw = config::Config::builder()
        .add_source(config::File::with_name(&args.config))
        .build()?;
    let mut parsed: FileConfig = raw.try_deserialize()?;
    if let Ok(volume) = std::env::var("BUZ_SANDBOXD_WORKSPACE_VOLUME") {
        parsed.sandboxd.workspace_volume = Some(volume);
    }
    let _guard = shared::logging::init("buz_sandboxd", &shared::logging::LoggingConfig::default());
    sandboxd::serve(parsed.sandboxd).await
}
