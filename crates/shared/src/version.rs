//! Package version — sourced from workspace `Cargo.toml` (`[workspace.package].version`).

/// Semantic version of the Buz workspace crates (manager / runner / shared).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Product name shown in CLI and API payloads.
pub const PRODUCT_NAME: &str = "Buz";

/// Build a clap-style version line: `0.3.0`.
pub fn version_string() -> &'static str {
    VERSION
}

/// Human-readable banner used in `--version` / startup logs.
pub fn long_version(component: &str) -> String {
    format!("{PRODUCT_NAME} {component} {VERSION}")
}
