//! Runner clock validation against the Manager clock.
//!
//! The Runner must stay consistent with the **Manager** clock: it stamps local
//! config backups, and its output has to line up with Manager-side job logs and
//! step timings. Remotely, a skewed Runner is invisible — nothing in a job log
//! points at the clock — so the Runner measures the offset itself.
//!
//! Reference: the Manager's HTTP `Date` header (present even on error
//! responses). It is sampled on the unauthenticated `/health` endpoint at
//! startup, and again — for free, no extra request — from every heartbeat
//! response so mid-run drift is caught too.

use chrono::Utc;
use reqwest::header::DATE;
use reqwest::Client;
use shared::clock::{describe_skew, parse_http_date, skew_from_http_date, SkewSample};
use std::sync::Mutex;
use std::time::Duration;

use crate::config::{ClockSection, Config};

/// Timeout for the startup probe.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// Sample the Manager clock at startup and enforce the configured policy.
///
/// Returns the measurement when the check ran. Fails with an actionable error
/// when the skew exceeds `max_skew_seconds` and `fail_on_skew` is set — a Runner
/// with a grossly wrong clock is worse than a stopped Runner.
pub async fn verify_at_startup(
    client: &Client,
    config: &Config,
) -> anyhow::Result<Option<SkewSample>> {
    let clock = &config.clock;
    if !clock.enabled {
        tracing::info!("clock: startup check disabled ([clock] enabled = false)");
        return Ok(None);
    }

    let source = probe_url(&config.runner.manager_url);
    match probe(client, &config.runner.manager_url).await {
        Ok(sample) => {
            let skewed = sample.is_skewed(clock.warn_threshold_seconds);
            if sample.local_behind_secs() >= clock.max_skew_seconds {
                if clock.fail_on_skew {
                    tracing::error!(
                        runner = %config.runner.name,
                        source = %source,
                        skew_secs = sample.skew_secs,
                        max_skew_seconds = clock.max_skew_seconds,
                        "clock: Runner clock too far from Manager — refusing to start"
                    );
                    anyhow::bail!(startup_error_message(
                        &source,
                        sample.skew_secs,
                        clock.max_skew_seconds
                    ));
                }
                tracing::error!(
                    runner = %config.runner.name,
                    source = %source,
                    skew_secs = sample.skew_secs,
                    "clock: Runner clock far from Manager (fail_on_skew = false, continuing) — sync NTP"
                );
            } else if skewed {
                tracing::warn!(
                    runner = %config.runner.name,
                    source = %source,
                    skew_secs = sample.skew_secs,
                    "clock: Runner clock drifts from Manager — check NTP on this host"
                );
            } else {
                tracing::info!(
                    runner = %config.runner.name,
                    source = %source,
                    skew = %describe_skew(sample.skew_secs),
                    "clock: Runner clock consistent with Manager"
                );
            }
            Ok(Some(sample))
        }
        Err(e) => {
            // The clock check is a guard, not a dependency: an unreachable
            // `/health` must not stop a Runner that can still register.
            tracing::warn!(
                source = %source,
                error = %e,
                "clock: startup check unavailable — continuing without clock validation"
            );
            Ok(None)
        }
    }
}

/// Tracks the latest measurement and de-duplicates warnings across heartbeats.
pub struct ClockMonitor {
    clock: ClockSection,
    manager_url: String,
    latest: Mutex<Option<SkewSample>>,
    /// Skew (rounded to whole minutes) already warned about, so a persistent
    /// offset logs once instead of every 30s.
    warned_bucket: Mutex<Option<i64>>,
}

impl ClockMonitor {
    pub fn new(clock: ClockSection, manager_url: impl Into<String>) -> Self {
        Self {
            clock,
            manager_url: manager_url.into(),
            latest: Mutex::new(None),
            warned_bucket: Mutex::new(None),
        }
    }

    /// Last measured skew in seconds (`remote - local`), for the heartbeat
    /// payload — `None` until a measurement succeeded.
    pub fn skew_secs(&self) -> Option<i64> {
        self.latest
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|s| s.skew_secs)
    }

    /// Seed the monitor with the startup sample.
    pub fn seed(&self, sample: Option<SkewSample>) {
        let mut latest = self.latest.lock().unwrap_or_else(|e| e.into_inner());
        *latest = sample;
    }

    /// Measure from an already-sent request/response pair (no extra I/O).
    pub fn observe(&self, sent_at: chrono::DateTime<Utc>, headers: &reqwest::header::HeaderMap) {
        if !self.clock.enabled {
            return;
        }
        let Some(raw) = headers.get(DATE).and_then(|v| v.to_str().ok()) else {
            tracing::debug!("clock: heartbeat response had no Date header — cannot re-check skew");
            return;
        };
        let remote = match parse_http_date(raw) {
            Ok(remote) => remote,
            Err(e) => {
                tracing::debug!(error = %e, "clock: heartbeat Date header unparsable");
                return;
            }
        };

        let sample = SkewSample {
            source: probe_url(&self.manager_url),
            skew_secs: skew_from_http_date(sent_at, Utc::now(), remote),
            checked_at: Utc::now(),
        };
        self.track(&sample);
    }

    /// Store a measurement and log it when it crosses the warn threshold.
    pub fn track(&self, sample: &SkewSample) {
        if !self.clock.enabled {
            return;
        }

        {
            let mut latest = self.latest.lock().unwrap_or_else(|e| e.into_inner());
            *latest = Some(sample.clone());
        }

        let Some(bucket) = warn_bucket(sample.skew_secs, self.clock.warn_threshold_seconds) else {
            // Within tolerance (or recovered): let the next excursion warn again.
            *self.warned_bucket.lock().unwrap_or_else(|e| e.into_inner()) = None;
            tracing::debug!(skew_secs = sample.skew_secs, "clock: skew check passed");
            return;
        };

        {
            let mut warned = self.warned_bucket.lock().unwrap_or_else(|e| e.into_inner());
            if *warned == Some(bucket) {
                return;
            }
            *warned = Some(bucket);
        }

        if sample.local_behind_secs() >= self.clock.max_skew_seconds {
            tracing::error!(
                source = %sample.source,
                skew_secs = sample.skew_secs,
                "clock: Runner clock far from Manager — job timing may be off; sync NTP on this host"
            );
        } else {
            tracing::warn!(
                source = %sample.source,
                skew_secs = sample.skew_secs,
                "clock: Runner clock drifts from Manager — check NTP on this host"
            );
        }
    }
}

/// Whole-minute bucket used to de-duplicate drift warnings: a steady offset
/// logs once, while a worsening offset (new bucket) is reported again.
/// `None` while the Runner clock is within `warn_threshold_secs`.
pub fn warn_bucket(skew_secs: i64, warn_threshold_secs: i64) -> Option<i64> {
    let behind = skew_secs.max(0);
    if behind < warn_threshold_secs {
        None
    } else {
        Some(behind / 60)
    }
}

/// One HTTP probe against the Manager's unauthenticated `/health` endpoint.
pub async fn probe(client: &Client, manager_url: &str) -> Result<SkewSample, String> {
    let source = probe_url(manager_url);
    let sent_at = Utc::now();
    let resp = client
        .head(&source)
        .timeout(PROBE_TIMEOUT)
        .send()
        .await
        .map_err(|e| format!("request to {source} failed: {e}"))?;
    let raw = resp
        .headers()
        .get(DATE)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| format!("{source} returned no Date header"))?;
    let remote = parse_http_date(raw)?;
    let received_at = Utc::now();

    Ok(SkewSample {
        source,
        skew_secs: skew_from_http_date(sent_at, received_at, remote),
        checked_at: received_at,
    })
}

/// Manager `/health` URL (outside `/api/v1`, no auth) used as the time source.
pub fn probe_url(manager_url: &str) -> String {
    format!("{}/health", manager_url.trim().trim_end_matches('/'))
}

/// Actionable startup failure message.
pub fn startup_error_message(source: &str, skew_secs: i64, max_skew_seconds: i64) -> String {
    format!(
        "Runner clock is {} {source} (limit {max_skew_seconds}s): the Runner must stay \
         consistent with the Manager clock, otherwise its timestamps and job logs disagree.\n  \
         Fix the clock on this host, e.g. `sudo timedatectl set-ntp true` then \
         `sudo systemctl enable --now systemd-timesyncd`, and restart the Runner.\n  \
         Labs only: set `[clock] fail_on_skew = false` to start anyway \
         (see Operations → Configuration → [clock]).",
        describe_skew(skew_secs)
    )
}

#[cfg(test)]
#[path = "clock_tests.rs"]
mod tests;
