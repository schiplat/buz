//! Runner-side observation of the sandbox supervisor's applied configuration.
//!
//! Every heartbeat (30 s) the Runner asks sandboxd for a cheap `status`. When
//! the applied hash changes it fetches the full document (`get_config`) and
//! atomically rewrites `sandboxd.snapshot.json` (owner-only). The snapshot is
//! **observational**: operators and support read it; sandboxd never trusts it
//! and keeps its own root-owned cache for recovery.
//!
//! The same status feeds the heartbeat's `sandboxd` block and tells the
//! heartbeat loop to re-probe capabilities immediately after a config change
//! instead of waiting for the 5-minute cadence.

use std::path::Path;

use serde_json::{json, Value};

use crate::config::Config;

/// What one observation round learned, for the heartbeat loop.
#[derive(Debug, Default, Clone)]
pub struct Observation {
    /// Heartbeat `sandboxd` block (always present once a sandboxed tier is
    /// selected, even when the supervisor is unreachable).
    pub block: Value,
    /// The applied config version changed since the last observation — the
    /// caller should re-probe capabilities now.
    pub config_changed: bool,
}

/// Rolling observer state across heartbeats.
#[derive(Debug, Default)]
pub struct Observer {
    last_sha256: Option<String>,
    last_version: Option<i64>,
    snapshot_written_sha256: Option<String>,
    snapshot_error: Option<String>,
    last_error: Option<String>,
    consecutive_failures: u32,
}

impl Observer {
    /// One observation round. Never fails: an unreachable supervisor becomes
    /// an error field in the block.
    pub async fn observe(&mut self, config: &Config) -> Observation {
        let status = match crate::client::sandbox_client::status(&config.sandbox).await {
            Ok(status) => status,
            Err(error) => {
                self.consecutive_failures += 1;
                self.last_error = Some(format!("{error:#}"));
                return Observation {
                    block: self.block(None, config),
                    config_changed: false,
                };
            }
        };
        self.consecutive_failures = 0;
        self.last_error = None;

        let version = status["config_version"].as_i64().unwrap_or(0);
        let sha = status["config_sha256"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let config_changed = self
            .last_version
            .is_some_and(|previous| previous != version);
        self.last_version = Some(version);

        if !sha.is_empty() && self.snapshot_written_sha256.as_deref() != Some(&sha) {
            match crate::client::sandbox_client::get_config(&config.sandbox).await {
                Ok(document) => match write_snapshot(
                    Path::new(&config.sandbox.snapshot_path),
                    &status,
                    &document,
                ) {
                    Ok(()) => {
                        self.snapshot_written_sha256 = Some(sha.clone());
                        self.snapshot_error = None;
                        tracing::info!(
                            version,
                            path = %config.sandbox.snapshot_path,
                            "sandboxd configuration snapshot written"
                        );
                    }
                    Err(error) => {
                        self.snapshot_error = Some(format!("{error:#}"));
                        tracing::warn!(error = %error, "could not write sandboxd snapshot");
                    }
                },
                Err(error) => {
                    self.snapshot_error = Some(format!("get_config: {error:#}"));
                }
            }
        }
        self.last_sha256 = Some(sha);

        Observation {
            block: self.block(Some(&status), config),
            config_changed,
        }
    }

    fn block(&self, status: Option<&Value>, config: &Config) -> Value {
        let mut block = status.cloned().unwrap_or_else(|| json!({}));
        if let Some(object) = block.as_object_mut() {
            object.remove("ok");
            object.insert("reachable".into(), json!(status.is_some()));
            object.insert("socket".into(), json!(config.sandbox.supervisor_socket));
            object.insert("last_error".into(), json!(self.last_error));
            object.insert(
                "consecutive_failures".into(),
                json!(self.consecutive_failures),
            );
            object.insert(
                "snapshot".into(),
                json!({
                    "path": config.sandbox.snapshot_path,
                    "sha256": self.snapshot_written_sha256,
                    "error": self.snapshot_error,
                }),
            );
            object.insert(
                "reported_at".into(),
                json!(chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
            );
        }
        block
    }
}

/// Atomic, owner-only write of the observational snapshot.
fn write_snapshot(path: &Path, status: &Value, document: &Value) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    let record = json!({
        "host_id": status["host_id"],
        "sandboxd_instance_id": status["instance_id"],
        "manager_instance_id": status["manager_instance_id"],
        "version": status["config_version"],
        "sha256": status["config_sha256"],
        "applied_at": status["applied_at"],
        "fetched_at": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        "config": document["config"],
    });
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("sandboxd.snapshot.json");
    let temp = parent.join(format!(".{file_name}.{}", uuid::Uuid::new_v4().simple()));
    let write = || -> anyhow::Result<()> {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temp)?;
        use std::io::Write;
        file.write_all(&serde_json::to_vec_pretty(&record)?)?;
        file.sync_all()?;
        std::fs::rename(&temp, path)?;
        Ok(())
    };
    match write() {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = std::fs::remove_file(&temp);
            Err(error)
        }
    }
}
