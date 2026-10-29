//! Runner deployment identity: resolved once at startup and stored back into
//! `config.runner` so register and heartbeat payloads carry it.
//!
//! `runner_instance_id`: `BUZ_RUNNER_INSTANCE_ID` > persisted state file
//! (next to the token cache) > generated `rn-<uuid>`. Survives re-registration
//! so the UI and logs can correlate the same installation.
//!
//! `host_id`: `BUZ_HOST_ID` > machine-id > hostname. The hostname fallback is
//! warned about — cloned VMs share machine-ids and hostnames alike.

use std::path::Path;

use shared::instance::{self, IdSource};

/// Resolve both identities into `config.runner.instance_id` / `host_id`.
pub fn resolve_into(config: &mut crate::config::Config) -> anyhow::Result<()> {
    // Persist next to the Runner credential: the state directory is the one
    // location already known to be durable and writable.
    let state_dir = Path::new(&config.runner.token_cache_path)
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();
    let (runner_instance_id, instance_source) = instance::ensure_persistent(
        &state_dir,
        "runner-instance-id",
        "BUZ_RUNNER_INSTANCE_ID",
        "rn",
    )
    .map_err(|e| anyhow::anyhow!("resolve runner instance id: {e}"))?;
    let (host_id, host_source) = instance::resolve_host_id("BUZ_HOST_ID")
        .map_err(|e| anyhow::anyhow!("resolve host id: {e}"))?;
    if host_source == IdSource::Hostname {
        tracing::warn!(
            host_id = %host_id,
            "host id fell back to the hostname — cloned machines will share it; set BUZ_HOST_ID"
        );
    }
    tracing::info!(
        runner_instance_id = %runner_instance_id,
        instance_source = instance_source.as_str(),
        host_id = %host_id,
        host_source = host_source.as_str(),
        "runner identity resolved"
    );
    config.runner.instance_id = runner_instance_id;
    config.runner.host_id = host_id;
    Ok(())
}
