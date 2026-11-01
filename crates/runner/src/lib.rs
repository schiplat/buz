//! Shared library for the Buz Runner binaries.
//!
//! `buz-runner` (`src/main.rs`) and `buz-sandboxd` (`src/bin/buz-sandboxd.rs`)
//! are thin shells over these modules, which keeps every module reachable from
//! the crate's integration tests under `tests/`.

pub mod cli;
pub mod client;
pub mod config;
pub mod engine;
pub mod identity;
pub mod masker;
pub mod policy;
pub mod sandbox;
pub mod secret;
pub mod sysinfo;
