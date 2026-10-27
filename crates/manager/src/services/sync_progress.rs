//! Live progress for plugin-source syncs (the dashboard's progress bar).
//!
//! The tracker is process-global: syncs are spawned from API handlers and
//! from the startup hook, and no single AppState covers all of them. The
//! dashboard polls `GET /admin/plugin-sources/{id}/sync-progress` while a sync
//! is in flight.

use chrono::Utc;
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

/// One source's sync progress. `phase` walks `index` (fetching index.json /
/// reviews.json) → `processing` → `done` / `failed`. Entry totals are exact
/// once the index is parsed, so the dashboard renders a true fraction.
#[derive(Clone, Debug, serde::Serialize, PartialEq)]
pub struct SyncProgress {
    pub phase: &'static str,
    pub total_plugins: usize,
    pub done_plugins: usize,
    pub total_entries: usize,
    pub done_entries: usize,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub error: Option<String>,
}

fn now_stamp() -> String {
    Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

static TRACKER: LazyLock<Mutex<HashMap<String, SyncProgress>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Mark a sync as started (phase `index`, zero counts).
pub fn begin(source_id: &str) {
    let mut map = TRACKER.lock().expect("sync progress lock");
    map.insert(
        source_id.to_string(),
        SyncProgress {
            phase: "index",
            total_plugins: 0,
            done_plugins: 0,
            total_entries: 0,
            done_entries: 0,
            started_at: now_stamp(),
            finished_at: None,
            error: None,
        },
    );
}

/// The index is parsed: totals are known, processing starts.
pub fn set_totals(source_id: &str, total_plugins: usize, total_entries: usize) {
    let mut map = TRACKER.lock().expect("sync progress lock");
    if let Some(progress) = map.get_mut(source_id) {
        progress.phase = "processing";
        progress.total_plugins = total_plugins;
        progress.total_entries = total_entries;
    }
}

pub fn tick_entry(source_id: &str) {
    let mut map = TRACKER.lock().expect("sync progress lock");
    if let Some(progress) = map.get_mut(source_id) {
        progress.done_entries += 1;
    }
}

pub fn tick_plugin(source_id: &str) {
    let mut map = TRACKER.lock().expect("sync progress lock");
    if let Some(progress) = map.get_mut(source_id) {
        progress.done_plugins += 1;
    }
}

/// Terminal state: `done`, or `failed` with the message shown in the UI.
pub fn finish(source_id: &str, error: Option<String>) {
    let mut map = TRACKER.lock().expect("sync progress lock");
    if let Some(progress) = map.get_mut(source_id) {
        progress.phase = if error.is_none() { "done" } else { "failed" };
        progress.finished_at = Some(now_stamp());
        progress.error = error;
    }
}

pub fn snapshot(source_id: &str) -> Option<SyncProgress> {
    TRACKER
        .lock()
        .expect("sync progress lock")
        .get(source_id)
        .cloned()
}
