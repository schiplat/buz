//! Coarse wake-up bus for Runner long polls. A broadcast signal wakes every
//! waiting Runner poll after a new job is committed or a cancellation is
//! recorded. Pollers subscribe before checking the database, so a signal
//! between the check and await is buffered and cannot be missed.

use std::sync::OnceLock;
use tokio::sync::broadcast;

static POLL_EVENTS: OnceLock<broadcast::Sender<()>> = OnceLock::new();

fn sender() -> &'static broadcast::Sender<()> {
    POLL_EVENTS.get_or_init(|| broadcast::channel(64).0)
}

pub fn subscribe() -> broadcast::Receiver<()> {
    sender().subscribe()
}

pub fn wake_all() {
    // No receivers is normal when no runner is currently waiting.
    let _ = sender().send(());
}
