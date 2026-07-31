//! Process logging: level + optional file (stdout always).

use std::fs::OpenOptions;
use std::path::Path;

use serde::Deserialize;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

#[derive(Debug, Clone, Deserialize)]
pub struct LoggingConfig {
    /// `error` | `warn` | `info` | `debug` | `trace`. Default: `info`.
    #[serde(default = "default_level")]
    pub level: String,
    /// Optional log file path (append). Empty / omitted → stdout only.
    /// When set, logs go to **both** stdout and the file.
    #[serde(default)]
    pub file: Option<String>,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: default_level(),
            file: None,
        }
    }
}

fn default_level() -> String {
    "info".into()
}

fn normalize_level(raw: &str) -> &'static str {
    match raw.trim().to_ascii_lowercase().as_str() {
        "error" => "error",
        "warn" | "warning" => "warn",
        "debug" => "debug",
        "trace" => "trace",
        "info" | "" => "info",
        other => {
            eprintln!("unknown logging.level '{other}', using info");
            "info"
        }
    }
}

/// Initialize tracing. Keep the returned guard alive for the process lifetime
/// when file logging is enabled (dropping it can lose buffered lines).
///
/// `RUST_LOG` still overrides the filter when set (standard tracing behavior).
pub fn init(crate_target: &str, cfg: &LoggingConfig) -> Option<WorkerGuard> {
    let level = normalize_level(&cfg.level);
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        // Quiet noisy dependencies; raise our crates to configured level.
        EnvFilter::new(format!(
            "{crate_target}={level},tower_http=warn,sqlx=warn,russh=warn,hyper=warn,{level}"
        ))
    });

    let file_path = cfg
        .file
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());

    if let Some(path) = file_path {
        if let Some(parent) = Path::new(&path).parent() {
            if !parent.as_os_str().is_empty() {
                let _ = std::fs::create_dir_all(parent);
            }
        }
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .unwrap_or_else(|e| panic!("failed to open log file '{path}': {e}"));
        let (nb, guard) = tracing_appender::non_blocking(file);
        tracing_subscriber::registry()
            .with(filter)
            .with(fmt::layer().with_target(true).with_writer(std::io::stdout))
            .with(
                fmt::layer()
                    .with_ansi(false)
                    .with_target(true)
                    .with_writer(nb),
            )
            .init();
        eprintln!("logging to stdout and file: {path} (level={level})");
        Some(guard)
    } else {
        tracing_subscriber::registry()
            .with(filter)
            .with(fmt::layer().with_target(true))
            .init();
        None
    }
}
