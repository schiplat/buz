//! Manager clock-skew guard for signed config URLs.
//!
//! `config_source` delivery works by handing the Runner a pre-signed OSS URL
//! (`crates/shared/src/config_auth.rs`). The signature embeds an **absolute**
//! `Expires` computed from the Manager's own system clock, so a Manager host
//! whose clock has drifted behind real time signs URLs that are already expired
//! on arrival — the Runner then fails with a confusing
//! `HTTP 403 AccessDenied: Request has expired` that looks like an OSS or
//! permission problem.
//!
//! This module samples the **remote** time of the OSS endpoint (its `Date`
//! response header, which is present even on error responses) and, before
//! signing, refuses with an actionable message when the URL would be born
//! expired. Clock skew cannot be fixed from inside the process — the Manager
//! never touches the system clock — so the probe exists to turn a silent,
//! hard-to-diagnose failure into an explicit one that names the real cause.

use chrono::Utc;
use shared::clock::{parse_http_date, skew_from_http_date, SkewSample};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::config::ClockConfig;

/// Timeout for a single clock probe.
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// Upper bound for the safety margin kept between the signing moment and the
/// URL expiry (see [`expiry_margin_secs`]).
pub const MAX_EXPIRY_MARGIN_SECS: i64 = 300;

/// Result of one probe: either a measurement or the reason it failed.
#[derive(Debug, Clone)]
enum Probe {
    Sample(SkewSample),
    Failed(String),
}

#[derive(Debug, Clone)]
struct Entry {
    probe: Probe,
    checked: Instant,
}

/// Caches clock measurements per time source and exposes them to the dispatch
/// path (`guard_signing`) and to `/health`.
pub struct ClockGuard {
    client: reqwest::Client,
    config: ClockConfig,
    entries: Mutex<HashMap<String, Entry>>,
}

impl ClockGuard {
    pub fn new(config: ClockConfig) -> Self {
        let client = reqwest::Client::builder()
            .user_agent(concat!("buz-manager/", env!("CARGO_PKG_VERSION")))
            .timeout(PROBE_TIMEOUT)
            .build()
            .expect("reqwest client build cannot fail");
        Self {
            client,
            config,
            entries: Mutex::new(HashMap::new()),
        }
    }

    pub fn config(&self) -> &ClockConfig {
        &self.config
    }

    /// Most recent **successful** sample, if any (feeds `/health`).
    pub fn latest(&self) -> Option<SkewSample> {
        let entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        entries
            .values()
            .filter_map(|e| match &e.probe {
                Probe::Sample(s) => Some(s.clone()),
                Probe::Failed(_) => None,
            })
            .max_by_key(|s| s.checked_at)
    }

    /// Most recent probe failure across sources, if any (feeds `/health`).
    pub fn last_error(&self) -> Option<String> {
        let entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        entries
            .values()
            .filter_map(|e| match &e.probe {
                Probe::Failed(err) => Some((e.checked, err.clone())),
                Probe::Sample(_) => None,
            })
            .max_by_key(|(checked, _)| *checked)
            .map(|(_, err)| err)
    }

    /// Measure the skew against `source`, reusing a cached probe when it is
    /// still fresh (`clock.cache_ttl_seconds`).
    pub async fn sample(&self, source: &str) -> Result<SkewSample, String> {
        if let Some(entry) = self.fresh_entry(source) {
            return match entry.probe {
                Probe::Sample(s) => Ok(s),
                Probe::Failed(e) => Err(e),
            };
        }

        let probe = match self.probe(source).await {
            Ok(sample) => Probe::Sample(sample),
            Err(e) => Probe::Failed(e),
        };
        self.entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(
                source.to_string(),
                Entry {
                    probe: probe.clone(),
                    checked: Instant::now(),
                },
            );
        match probe {
            Probe::Sample(s) => Ok(s),
            Probe::Failed(e) => Err(e),
        }
    }

    fn fresh_entry(&self, source: &str) -> Option<Entry> {
        let entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let entry = entries.get(source)?;
        if entry.checked.elapsed() < Duration::from_secs(self.config.cache_ttl_seconds) {
            Some(entry.clone())
        } else {
            None
        }
    }

    /// One HTTP probe: read the remote `Date` header and compare it with the
    /// local clock, compensating for half the round-trip.
    async fn probe(&self, source: &str) -> Result<SkewSample, String> {
        let sent_at = Utc::now();
        // A HEAD to the bucket root may be rejected (403) — that is fine: the
        // `Date` header of the error response is just as authoritative, and the
        // status code is deliberately ignored.
        let resp = self
            .client
            .head(source)
            .send()
            .await
            .map_err(|e| format!("clock probe request to {source} failed: {e}"))?;
        let raw = resp
            .headers()
            .get(reqwest::header::DATE)
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| format!("clock probe: {source} returned no Date header"))?;
        let remote = parse_http_date(raw).map_err(|e| format!("clock probe: {e}"))?;
        let received_at = Utc::now();

        Ok(SkewSample {
            source: source.to_string(),
            skew_secs: skew_from_http_date(sent_at, received_at, remote),
            checked_at: received_at,
        })
    }
}

/// Safety margin subtracted from the signed-URL TTL: a URL whose remaining life
/// would be shorter than this is not worth handing to a Runner.
pub fn expiry_margin_secs(ttl_secs: i64) -> i64 {
    (ttl_secs / 12).clamp(5, MAX_EXPIRY_MARGIN_SECS)
}

/// True when a URL signed now with `ttl_secs` would already be expired (or
/// nearly so) by the time the Runner downloads it.
pub fn url_born_expired(behind_secs: i64, ttl_secs: i64) -> bool {
    behind_secs >= ttl_secs - expiry_margin_secs(ttl_secs)
}

/// Actionable message for a born-expired signed URL.
pub fn expiry_error_message(source: &str, behind_secs: i64, ttl_secs: i64) -> String {
    format!(
        "Manager clock is about {behind_secs}s behind {source}: a config URL signed with a \
         {ttl_secs}s TTL would already be expired when the Runner downloads it (OSS answers \
         `403 AccessDenied: Request has expired`). Sync the Manager host clock (NTP), e.g. \
         `sudo timedatectl set-ntp true`, then re-run — see Operations → Configuration → [clock]."
    )
}

/// Base URL used to sample the OSS server clock: `https://{bucket}.{endpoint}/`.
pub fn oss_source_url(bucket: &str, endpoint: &str) -> String {
    let host = endpoint
        .trim()
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_end_matches('/');
    format!("https://{}.{}/", bucket.trim(), host)
}

static GUARD: OnceLock<ClockGuard> = OnceLock::new();

/// Install the process-wide guard from configuration (called once from `main`).
pub fn init(config: &ClockConfig) {
    if GUARD.set(ClockGuard::new(config.clone())).is_err() {
        tracing::debug!("clock: guard already initialised, keeping the first configuration");
    }
}

/// Process-wide guard (defaults when `init` has not run, e.g. in tests).
pub fn global() -> &'static ClockGuard {
    GUARD.get_or_init(|| ClockGuard::new(ClockConfig::default()))
}

/// Pre-signing guard: returns an error message when the signed URL would be
/// born expired, `None` when signing may proceed.
///
/// Never blocks dispatch because the probe itself failed — a missing guard is
/// strictly better than blocking every `config_source` step on a probe.
pub async fn guard_signing(guard: &ClockGuard, source_url: &str, ttl_secs: i64) -> Option<String> {
    if !guard.config().enabled {
        return None;
    }

    match guard.sample(source_url).await {
        Ok(sample) => {
            let behind_secs = sample.local_behind_secs();
            if url_born_expired(behind_secs, ttl_secs) {
                tracing::error!(
                    source = %sample.source,
                    behind_secs,
                    ttl_secs,
                    "clock: Manager clock too slow — refusing to sign a config URL that would be expired on arrival"
                );
                Some(expiry_error_message(&sample.source, behind_secs, ttl_secs))
            } else if sample.is_skewed(guard.config().warn_threshold_seconds) {
                tracing::warn!(
                    source = %sample.source,
                    skew_secs = sample.skew_secs,
                    "clock: Manager clock skew above warn threshold — check NTP on the Manager host"
                );
                None
            } else {
                tracing::debug!(
                    source = %sample.source,
                    skew_secs = sample.skew_secs,
                    "clock: skew check passed"
                );
                None
            }
        }
        Err(e) => {
            tracing::warn!(
                source = %source_url,
                error = %e,
                "clock: skew check unavailable — signing config URL without clock guard"
            );
            None
        }
    }
}

#[cfg(test)]
#[path = "tests/clock_tests.rs"]
mod tests;
