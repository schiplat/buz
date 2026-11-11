//! Storage boundary: the only place Manager speaks SQL.
//!
//! Every statement lives here, behind a **named operation** that owns its
//! transaction (or documents that the caller owns one). Handlers in `api/`
//! and domain logic in `services/` call these functions; they never build
//! SQL themselves.
//!
//! Rules:
//!
//! - One function per business operation, named after the invariant it
//!   protects (`cancel_job_in_tx`, `reserve_job`, …). No per-table generic
//!   repositories: correctness here lives in constraints, guarded updates
//!   and transaction scope, and only named operations can carry those.
//! - A function that must be atomic either opens its own transaction or
//!   takes `&mut Transaction` and says so in its doc comment. Post-commit
//!   fan-out (notify, WebSocket, `wake_all`) never happens inside.
//! - The dialect stays SQLite for now; the Postgres port replaces bodies,
//!   not signatures (see
//!   `docs/content/docs/01-architecture/execution/transaction-boundaries.md`
//!   for the per-site lock translation).
//!
//! Enforcement: `scripts/check_store_boundary.sh` fails CI when `api/`
//! contains SQL again — the allowlist file lists files still to migrate,
//! and shrinks with every step until it is empty.

pub mod api_tokens;
pub mod audit_logs;
pub mod auth;
pub mod environments;
pub mod git_hooks;
pub mod github;
pub mod github_binds;
pub mod health;
pub mod hosts;
pub mod jobs;
pub mod logs;
pub mod metrics;
pub mod ops_sandbox;
pub mod pipelines;
pub mod plugin_sources;
pub mod runners;
pub mod sandboxd_config;
pub mod secrets;
pub mod teams;
pub mod trigger_events;
pub mod users;
pub mod variables;
pub mod webhook_pipeline;
