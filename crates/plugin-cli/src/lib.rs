//! `buz-plugin` — plugin authoring & publishing toolchain.
//!
//! Responsibility boundary (docs/architecture → Plugin Tooling): this CLI is
//! the authoring moment of the contract (`shared::plugin_spec` is the single
//! authority); the Manager re-runs the same validation when it consumes a
//! template. Feedback here, enforcement there — never two implementations.
//!
//! The `buz-plugin` binary (`src/main.rs`) is a thin shell over these modules,
//! which keeps them reachable from the integration tests under `tests/`.

pub mod cli;
pub mod indexgen;
pub mod layout;
pub mod mirror;
pub mod release_check;
pub mod reviews;
pub mod revision;
pub mod scaffold;
pub mod validate;
