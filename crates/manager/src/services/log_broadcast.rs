use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, RwLock};
use tokio::sync::broadcast;

const REPLAY_CAPACITY: usize = 256;

/// A single log entry with step metadata for per-step grouping on the frontend.
#[derive(Clone)]
pub struct LogEntry {
    pub step_order: i32,
    pub content: String,
}

impl LogEntry {
    pub fn to_json_string(&self) -> String {
        // Simple json string without serde overhead at broadcast time
        format!("{{\"step_order\":{},\"content\":{}}}",
            self.step_order,
            serde_json::to_string(&self.content).unwrap_or_else(|_| "\"\"".into()))
    }
}

/// One per-job sink that retains recent chunks so late WebSocket
/// subscribers see messages that arrived before they connected.
struct JobSink {
    tx: broadcast::Sender<String>,
    history: VecDeque<String>,
}

/// Broadcasts log chunks to connected WebSocket clients per job.
#[derive(Clone)]
pub struct LogBroadcaster {
    sinks: Arc<RwLock<HashMap<String, JobSink>>>,
}

impl LogBroadcaster {
    pub fn new() -> Self {
        Self {
            sinks: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Subscribe to log chunks for a given job.
    /// Returns a receiver that first drains any buffered history,
    /// then continues receiving live chunks.
    pub fn subscribe(&self, job_id: &str) -> broadcast::Receiver<String> {
        let mut map = self.sinks.write().unwrap();
        let sink = map
            .entry(job_id.to_string())
            .or_insert_with(|| {
                let (tx, _) = broadcast::channel(REPLAY_CAPACITY);
                JobSink {
                    tx,
                    history: VecDeque::with_capacity(REPLAY_CAPACITY),
                }
            });
        sink.tx.subscribe()
    }

    /// Return a snapshot of recent history for replay (call once after subscribe).
    pub fn replay_history(&self, job_id: &str) -> Vec<String> {
        let map = self.sinks.read().unwrap();
        map.get(job_id)
            .map(|s| s.history.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Push a new log entry to all subscribers of the given job.
    pub fn broadcast(&self, job_id: &str, step_order: i32, content: &str) {
        let mut map = self.sinks.write().unwrap();
        if let Some(sink) = map.get_mut(job_id) {
            let entry = LogEntry { step_order, content: content.to_string() };
            let json = entry.to_json_string();
            // Keep a bounded history ring for late joiners
            if sink.history.len() >= REPLAY_CAPACITY {
                sink.history.pop_front();
            }
            sink.history.push_back(json.clone());
            // Fan‑out to currently connected clients
            if sink.tx.receiver_count() > 0 {
                let _ = sink.tx.send(json.clone());
            }
        }
    }

    /// Clean up the broadcast channel for a finished job.
    pub fn cleanup(&self, job_id: &str) {
        let mut map = self.sinks.write().unwrap();
        map.remove(job_id);
    }
}
