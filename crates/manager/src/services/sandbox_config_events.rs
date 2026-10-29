//! Wake-up bus for sandboxd configuration long-polls.
//!
//! Deliberately separate from `poll_events` (the Runner job-poll bus): a
//! configuration change must not wake every Runner waiting for work, and a
//! new job must not wake every sandboxd waiting for configuration. Waiters
//! subscribe before reading the database, so a change committed between the
//! read and the await is buffered and cannot be missed.

use std::sync::OnceLock;
use tokio::sync::broadcast;

static CONFIG_EVENTS: OnceLock<broadcast::Sender<()>> = OnceLock::new();

fn sender() -> &'static broadcast::Sender<()> {
    CONFIG_EVENTS.get_or_init(|| broadcast::channel(64).0)
}

pub fn subscribe() -> broadcast::Receiver<()> {
    sender().subscribe()
}

/// Wake every sandboxd long-poll; each re-checks its own desired version.
/// Config changes are rare, so the extra reads are negligible.
pub fn wake_all() {
    // No receivers is normal when no sandboxd is currently waiting.
    let _ = sender().send(());
}
