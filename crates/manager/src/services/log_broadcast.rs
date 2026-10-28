use chrono::Utc;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use tokio::sync::broadcast;

const REPLAY_CAPACITY: usize = 256;

/// A single log entry with step metadata for per-step grouping on the frontend.
#[derive(Clone)]
pub struct LogEntry {
    pub step_order: i32,
    pub content: String,
    /// Chunk severity (lowercase, e.g. `"error"`); `None` = legacy chunk.
    pub level: Option<String>,
    /// RFC3339 UTC timestamp so Job Detail can render `[HH:MM:SS]`.
    pub created_at: String,
}

impl LogEntry {
    pub fn new(step_order: i32, level: Option<&str>, content: impl Into<String>) -> Self {
        Self {
            step_order,
            level: level.map(str::to_string),
            content: content.into(),
            created_at: Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        }
    }

    pub fn to_json_string(&self) -> String {
        serde_json::json!({
            "step_order": self.step_order,
            "content": self.content,
            "level": self.level,
            "created_at": self.created_at,
        })
        .to_string()
    }
}

/// One per-job sink that retains recent chunks so late WebSocket
/// subscribers see messages that arrived before they connected.
struct JobSink {
    tx: broadcast::Sender<String>,
    history: std::collections::VecDeque<String>,
}

/// Broadcasts log chunks and job-status updates to connected WebSocket clients.
#[derive(Clone)]
pub struct LogBroadcaster {
    sinks: Arc<RwLock<HashMap<String, JobSink>>>,
    pipeline_sinks: Arc<RwLock<HashMap<String, JobSink>>>,
}

impl Default for LogBroadcaster {
    fn default() -> Self {
        Self::new()
    }
}

impl LogBroadcaster {
    pub fn new() -> Self {
        Self {
            sinks: Arc::new(RwLock::new(HashMap::new())),
            pipeline_sinks: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Get or create a JobSink — ensures history is retained even before any WS client connects.
    fn get_or_create(map: &Arc<RwLock<HashMap<String, JobSink>>>, key: &str) {
        let mut m = map.write().unwrap();
        m.entry(key.to_string()).or_insert_with(|| {
            let (tx, _) = broadcast::channel(REPLAY_CAPACITY);
            JobSink {
                tx,
                history: std::collections::VecDeque::with_capacity(REPLAY_CAPACITY),
            }
        });
    }

    /// Subscribe to log chunks for a given job.
    /// Returns a receiver that first drains any buffered history,
    /// then continues receiving live chunks.
    pub fn subscribe(&self, job_id: &str) -> broadcast::Receiver<String> {
        Self::get_or_create(&self.sinks, job_id);
        let map = self.sinks.read().unwrap();
        map.get(job_id).unwrap().tx.subscribe()
    }

    /// Return a snapshot of recent history for replay (call once after subscribe).
    pub fn replay_history(&self, job_id: &str) -> Vec<String> {
        let map = self.sinks.read().unwrap();
        map.get(job_id)
            .map(|s| s.history.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Push a new log entry to all subscribers of the given job.
    /// Lazily creates the sink so history is retained even before any WS client connects.
    pub fn broadcast(&self, job_id: &str, step_order: i32, level: Option<&str>, content: &str) {
        Self::get_or_create(&self.sinks, job_id);
        let mut map = self.sinks.write().unwrap();
        if let Some(sink) = map.get_mut(job_id) {
            let entry = LogEntry::new(step_order, level, content);
            let json = entry.to_json_string();
            // Keep a bounded history ring for late joiners
            if sink.history.len() >= REPLAY_CAPACITY {
                sink.history.pop_front();
            }
            sink.history.push_back(json.clone());
            // Fan‑out to currently connected clients
            if sink.tx.receiver_count() > 0 {
                let _ = sink.tx.send(json);
            }
        }
    }

    /// Clean up the broadcast channel for a finished job.
    pub fn cleanup(&self, job_id: &str) {
        let mut map = self.sinks.write().unwrap();
        map.remove(job_id);
    }

    /// Subscribe to job-status updates for all jobs in a pipeline.
    pub fn pipeline_subscribe(&self, pipeline_id: &str) -> broadcast::Receiver<String> {
        Self::get_or_create(&self.pipeline_sinks, pipeline_id);
        let map = self.pipeline_sinks.read().unwrap();
        map.get(pipeline_id).unwrap().tx.subscribe()
    }

    /// Return recent status history for a pipeline (call once after subscribe).
    pub fn pipeline_replay(&self, pipeline_id: &str) -> Vec<String> {
        let map = self.pipeline_sinks.read().unwrap();
        map.get(pipeline_id)
            .map(|s| s.history.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Broadcast a job status update to both job-level and pipeline-level subscribers.
    /// Lazily creates sinks so history is retained even before any WS client connects.
    pub fn broadcast_status(&self, job_id: &str, pipeline_id: &str, status_json: &str) {
        // ── Job-level subscribers ──
        Self::get_or_create(&self.sinks, job_id);
        {
            let mut map = self.sinks.write().unwrap();
            if let Some(sink) = map.get_mut(job_id) {
                let msg = format!(r#"{{"type":"status","data":{}}}"#, status_json);
                if sink.history.len() >= REPLAY_CAPACITY {
                    sink.history.pop_front();
                }
                sink.history.push_back(msg.clone());
                if sink.tx.receiver_count() > 0 {
                    let _ = sink.tx.send(msg);
                }
            }
        }
        // ── Pipeline-level subscribers ──
        Self::get_or_create(&self.pipeline_sinks, pipeline_id);
        {
            let mut map = self.pipeline_sinks.write().unwrap();
            if let Some(sink) = map.get_mut(pipeline_id) {
                let msg = format!(
                    r#"{{"type":"status","job_id":"{}","data":{}}}"#,
                    job_id, status_json
                );
                if sink.history.len() >= REPLAY_CAPACITY {
                    sink.history.pop_front();
                }
                sink.history.push_back(msg.clone());
                if sink.tx.receiver_count() > 0 {
                    let _ = sink.tx.send(msg);
                }
            }
        }
    }

    /// Clean up the pipeline broadcast channel.
    pub fn pipeline_cleanup(&self, pipeline_id: &str) {
        let mut map = self.pipeline_sinks.write().unwrap();
        map.remove(pipeline_id);
    }

    /// Number of active job-level log sinks (health/observability signal).
    pub fn job_sink_count(&self) -> usize {
        self.sinks.read().unwrap().len()
    }

    /// Number of active pipeline-level status sinks (health/observability signal).
    pub fn pipeline_sink_count(&self) -> usize {
        self.pipeline_sinks.read().unwrap().len()
    }
}
