//! Package version — sourced from workspace `Cargo.toml` (`[workspace.package].version`).

/// Semantic version of the Buz workspace crates (manager / runner / shared).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Product name shown in CLI and API payloads.
pub const PRODUCT_NAME: &str = "Buz";

/// `--version` line for the manager binary: `Buz manager 1.7.4`.
pub const MANAGER_LONG_VERSION: &str = concat!("Buz manager ", env!("CARGO_PKG_VERSION"));

/// `--version` line for the runner binary: `Buz runner 1.7.4`.
pub const RUNNER_LONG_VERSION: &str = concat!("Buz runner ", env!("CARGO_PKG_VERSION"));

/// Build a clap-style version line: `1.7.4`.
pub fn version_string() -> &'static str {
    VERSION
}

/// Human-readable banner used in `--version` / startup logs.
pub fn long_version(component: &str) -> String {
    format!("{PRODUCT_NAME} {component} {VERSION}")
}
