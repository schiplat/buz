//! Shared library for the Buz Runner binaries.
//!
//! `buz-runner` keeps its `main` in `src/main.rs`; `buz-sandboxd` reuses the
//! sandbox supervisor from `crate::sandbox` instead of `#[path]`-including it.

pub mod sandbox;
