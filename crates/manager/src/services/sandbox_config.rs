//! Manager-side alias for the shared sandbox host document.
//!
//! The typed document and its validation live in `shared::sandbox_config` so
//! the ops API (on write) and `buz-sandboxd` (on apply) run the *same* code.
//! Manager code keeps importing `crate::services::sandbox_config::*`.

pub use shared::sandbox_config::*;
