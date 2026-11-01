//! Manager fetch loop for sandboxd: long-poll with fallback polling, atomic
//! apply, and a root-owned last-known-good cache.
//!
//! Cadence (design record: sandbox-host-configuration): a long-poll wait is
//! the steady state — a published change is applied within ~1 s; failures
//! back off exponentially and degrade to plain polling; recovery returns to
//! long-poll. The cache is written only after a successful apply and has no
//! TTL: it is last-known-good, and the Manager (not a cache expiry) is what
//! prevents stale policy from running new jobs.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::config::{AppliedConfig, Bootstrap, ConfigState};

const LONG_POLL_WAIT: &str = "wait=30";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(50);
/// Backoff steps for failed fetches (seconds), capped at the last entry.
const BACKOFF_SECS: [u64; 6] = [1, 2, 4, 8, 16, 30];
/// Polling interval once long-polls repeatedly fail.
const FALLBACK_POLL: Duration = Duration::from_secs(60);
/// Consecutive failures before degrading to plain polling.
const FALLBACK_THRESHOLD: u32 = 3;

/// On-disk cache record: the last configuration confirmed applied, with the
/// identity bindings it was served under. Loading refuses a cache that
/// belongs to another Manager/host/instance.
#[derive(Debug, Serialize, Deserialize)]
struct CacheFile {
    manager_instance_id: String,
    host_id: String,
    instance_id: String,
    version: i64,
    sha256: String,
    applied_at: String,
    document: shared::sandbox_config::SandboxHostDocument,
}

/// Load the last-known-good cache, if it validates against this bootstrap.
pub fn load_cache(bootstrap: &Bootstrap) -> anyhow::Result<Option<AppliedConfig>> {
    let path = bootstrap.cache_path();
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let parsed: CacheFile = serde_json::from_str(&raw)?;
    let expected_manager = bootstrap.manager_instance_id.trim();
    if !expected_manager.is_empty() && parsed.manager_instance_id != expected_manager {
        anyhow::bail!(
            "cached configuration belongs to manager '{}' but this host is pinned to '{}'",
            parsed.manager_instance_id,
            expected_manager
        );
    }
    anyhow::ensure!(
        parsed.host_id == bootstrap.host_id && parsed.instance_id == bootstrap.instance_id,
        "cached configuration belongs to {}/{} but this supervisor is {}/{}",
        parsed.host_id,
        parsed.instance_id,
        bootstrap.host_id,
        bootstrap.instance_id
    );
    // A cache is only trusted when it still validates under the current rules.
    let (document, _) = shared::sandbox_config::validate(&parsed.document).map_err(|problems| {
        anyhow::anyhow!(
            "cached configuration no longer valid: {}",
            problems.join("; ")
        )
    })?;
    Ok(Some(AppliedConfig {
        version: parsed.version,
        sha256: parsed.sha256,
        manager_instance_id: parsed.manager_instance_id,
        applied_at: parsed.applied_at,
        document,
    }))
}

/// Atomically persist the last-known-good configuration (owner-only).
fn write_cache(bootstrap: &Bootstrap, applied: &AppliedConfig) -> anyhow::Result<()> {
    let path = bootstrap.cache_path();
    std::fs::create_dir_all(&bootstrap.state_dir)?;
    let record = CacheFile {
        manager_instance_id: applied.manager_instance_id.clone(),
        host_id: bootstrap.host_id.clone(),
        instance_id: bootstrap.instance_id.clone(),
        version: applied.version,
        sha256: applied.sha256.clone(),
        applied_at: applied.applied_at.clone(),
        document: applied.document.clone(),
    };
    let temp = bootstrap.state_dir.join(format!(
        ".sandboxd-config.{}.tmp",
        uuid::Uuid::new_v4().simple()
    ));
    let write = || -> anyhow::Result<()> {
        {
            use std::io::Write;
            let mut file = std::fs::File::create(&temp)?;
            file.write_all(&serde_json::to_vec(&record)?)?;
            file.sync_all()?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o600))?;
        }
        std::fs::rename(&temp, &path)?;
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

/// One fetch round against the Manager. `Ok(None)` = unchanged.
async fn fetch_once(
    client: &reqwest::Client,
    bootstrap: &Bootstrap,
    applied_version: i64,
) -> anyhow::Result<Option<AppliedConfig>> {
    let url = format!(
        "{}/api/v1/sandboxd/config?applied_version={applied_version}",
        bootstrap.manager_url.trim_end_matches('/')
    );
    let response = client
        .get(&url)
        .bearer_auth(&bootstrap.token)
        .header("Prefer", LONG_POLL_WAIT)
        .timeout(REQUEST_TIMEOUT)
        .send()
        .await?;
    let status = response.status();
    anyhow::ensure!(status.is_success(), "config fetch returned {status}");
    let body: serde_json::Value = response.json().await?;
    let data = &body["data"];
    anyhow::ensure!(data.is_object(), "malformed config response");
    let manager_instance_id = data["manager_instance_id"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let expected = bootstrap.manager_instance_id.trim();
    // A different control plane must never be applied, even if its token or
    // URL were (mis)configured onto this host.
    anyhow::ensure!(
        expected.is_empty() || manager_instance_id == expected,
        "manager identity mismatch: response from '{manager_instance_id}', pinned to '{expected}'"
    );
    if data["changed"].as_bool() != Some(true) {
        return Ok(None);
    }
    let version = data["version"].as_i64().unwrap_or(0);
    let document: shared::sandbox_config::SandboxHostDocument =
        serde_json::from_value(data["config"].clone())?;
    let (document, _) = shared::sandbox_config::validate(&document)
        .map_err(|problems| anyhow::anyhow!("manager document invalid: {}", problems.join("; ")))?;
    let sha256 = data["sha256"].as_str().unwrap_or_default().to_string();
    // Trust but verify: the hash must match what we were given.
    let computed = shared::sandbox_config::sha256_hex(
        serde_json::to_string(&shared::sandbox_config::canonical_json(&document))?.as_bytes(),
    );
    if !sha256.is_empty() && computed != sha256 {
        anyhow::bail!(
            "config sha256 mismatch (declared {sha256}, computed {computed}) — refusing to apply"
        );
    }
    Ok(Some(AppliedConfig {
        version,
        sha256: if sha256.is_empty() { computed } else { sha256 },
        manager_instance_id,
        applied_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        document,
    }))
}

/// Report the applied state back to the Manager (best-effort; failures only
/// log — the next round carries fresher data anyway).
async fn report_status(
    client: &reqwest::Client,
    bootstrap: &Bootstrap,
    applied_version: i64,
    last_error: &str,
    consecutive_failures: i64,
) {
    let url = format!(
        "{}/api/v1/sandboxd/status",
        bootstrap.manager_url.trim_end_matches('/')
    );
    let request = client
        .post(&url)
        .bearer_auth(&bootstrap.token)
        .json(&serde_json::json!({
            "applied_version": applied_version,
            "applied_sha256": "",
            "last_error": last_error,
            "consecutive_failures": consecutive_failures,
        }));
    if let Err(error) = request.timeout(REQUEST_TIMEOUT).send().await {
        tracing::debug!(error = %error, "sandboxd status report failed");
    }
}

/// Apply a fetched configuration: swap it in, then persist the cache.
///
/// The fetch already validated the document, so the swap cannot fail. A cache
/// write failure downgrades durability, not correctness: the in-memory config
/// is live and the next successful apply overwrites the stale file.
fn apply(bootstrap: &Bootstrap, state: &ConfigState, fetched: AppliedConfig) {
    state.replace(fetched.clone());
    if let Err(error) = write_cache(bootstrap, &fetched) {
        tracing::warn!(error = %error, "could not persist the sandboxd config cache");
    }
    tracing::info!(
        version = fetched.version,
        profiles = fetched.document.profiles.len(),
        tiers = fetched.document.tiers.len(),
        "sandboxd configuration applied"
    );
}

/// The fetch loop. Runs until the process exits.
pub async fn run_fetch_loop(bootstrap: Bootstrap, state: ConfigState) {
    let mut client_builder = reqwest::Client::builder();
    if bootstrap.tls_skip_verify {
        client_builder = client_builder.danger_accept_invalid_certs(true);
    }
    let client = match client_builder.build() {
        Ok(client) => client,
        Err(error) => {
            tracing::error!(error = %error, "sandboxd http client construction failed; staying on cache");
            return;
        }
    };
    tracing::info!(
        manager = %bootstrap.manager_url,
        instance = %bootstrap.instance_id,
        host = %bootstrap.host_id,
        "sandboxd config loop starting"
    );
    let mut backoff_index = 0usize;
    let mut consecutive_failures: u32 = 0;
    let mut degraded = false;
    loop {
        let applied_version = state.applied_version();
        let result = fetch_once(&client, &bootstrap, applied_version).await;
        match result {
            Ok(Some(fetched)) => {
                apply(&bootstrap, &state, fetched);
                report_status(&client, &bootstrap, state.applied_version(), "", 0).await;
                backoff_index = 0;
                consecutive_failures = 0;
                if degraded {
                    degraded = false;
                    tracing::info!("sandboxd config delivery recovered to long-poll");
                }
            }
            Ok(None) => {
                backoff_index = 0;
                consecutive_failures = 0;
                if degraded {
                    degraded = false;
                    tracing::info!("sandboxd config delivery recovered to long-poll");
                }
            }
            Err(error) => {
                consecutive_failures += 1;
                if !degraded && consecutive_failures >= FALLBACK_THRESHOLD {
                    degraded = true;
                    tracing::warn!(
                        error = %error,
                        "sandboxd long-poll failing repeatedly; degrading to {}s polling",
                        FALLBACK_POLL.as_secs()
                    );
                }
                report_status(
                    &client,
                    &bootstrap,
                    applied_version,
                    &error.to_string(),
                    consecutive_failures as i64,
                )
                .await;
                let delay = if degraded {
                    FALLBACK_POLL
                } else {
                    Duration::from_secs(BACKOFF_SECS[backoff_index.min(BACKOFF_SECS.len() - 1)])
                };
                backoff_index = (backoff_index + 1).min(BACKOFF_SECS.len() - 1);
                tokio::time::sleep(delay).await;
                continue;
            }
        }
        // Steady state: reconnect immediately; the server holds the wait.
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
