//! Buz Manager — orchestration control plane.
//!
//! The `buz-manager` binary (`src/main.rs`) is a thin shell over these modules,
//! which keeps the HTTP API, services, and database layer reachable from the
//! integration tests under `tests/`.

pub mod api;
pub mod audit;
pub mod cli;
pub mod config;
pub mod crypto;
pub mod db;
pub mod frontend;
pub mod http_util;
pub mod image_registry;
pub mod middleware;
pub mod password;
pub mod pipeline_plan;
pub mod rbac;
pub mod services;
pub mod ssh_probe;
pub mod state;
pub mod store;
