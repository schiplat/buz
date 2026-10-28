//! Clock-skew helpers shared by the Manager and the Runner.
//!
//! Both components exchange absolute timestamps with remote peers whose clock
//! they must trust:
//!
//! - **Manager** signs OSS URLs with an absolute `Expires`
//!   (`crate::config_auth`), so a slow Manager clock mints URLs that are already
//!   expired when the Runner downloads them.
//! - **Runner** stamps config backups and correlates with Manager-side job logs,
//!   so it must not diverge from the Manager clock either.
//!
//! The cheapest reliable reference available to both is the `Date` response
//! header of an HTTP peer (present even on `403`), compared against the local
//! clock with half the round-trip discounted. The helpers here keep both sides
//! measuring skew the same way; each side applies its own policy on top.

use chrono::{DateTime, Duration as ChronoDuration, Utc};

/// One measured clock skew against a remote time source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkewSample {
    /// Where the sample came from (peer base URL).
    pub source: String,
    /// `remote - local`. **Positive means the local clock is behind**, which is
    /// the direction that breaks absolute-expiry signatures.
    pub skew_secs: i64,
    /// When the measurement completed (local clock).
    pub checked_at: DateTime<Utc>,
}

impl SkewSample {
    /// How far the local clock is behind the remote (0 when ahead).
    pub fn local_behind_secs(&self) -> i64 {
        self.skew_secs.max(0)
    }

    /// True when the local clock is at or beyond `warn_threshold_secs` behind.
    pub fn is_skewed(&self, warn_threshold_secs: i64) -> bool {
        self.local_behind_secs() >= warn_threshold_secs
    }
}

/// Parse an HTTP `Date` header (RFC 1123 / RFC 2822, always GMT).
pub fn parse_http_date(raw: &str) -> Result<DateTime<Utc>, String> {
    DateTime::parse_from_rfc2822(raw.trim())
        .map(|d| d.with_timezone(&Utc))
        .map_err(|e| format!("invalid Date header '{raw}': {e}"))
}

/// Skew of the local clock (`remote - local`) for a request bracketed by
/// `sent_at` / `received_at`.
///
/// HTTP dates carry 1-second resolution, so the midpoint of the round-trip is
/// used instead of the send time — that keeps a slow link from looking like a
/// clock offset.
pub fn skew_from_http_date(
    sent_at: DateTime<Utc>,
    received_at: DateTime<Utc>,
    remote: DateTime<Utc>,
) -> i64 {
    let rtt = received_at.signed_duration_since(sent_at);
    let local_mid = sent_at + ChronoDuration::milliseconds(rtt.num_milliseconds() / 2);
    remote.signed_duration_since(local_mid).num_seconds()
}

/// Human-readable skew, e.g. `+12s behind` / `-3s ahead` — for log messages.
pub fn describe_skew(skew_secs: i64) -> String {
    if skew_secs >= 0 {
        format!("{skew_secs}s behind")
    } else {
        format!("{}s ahead", -skew_secs)
    }
}

/// Absolute UTC stamp for a Unix epoch, e.g. `2026-09-20 10:14:35 UTC`.
///
/// Used in job logs next to OSS `Date` headers so operators can compare
/// timestamps without converting time zones by hand.
pub fn format_utc(epoch_secs: i64) -> String {
    match DateTime::from_timestamp(epoch_secs, 0) {
        Some(d) => d.format("%Y-%m-%d %H:%M:%S UTC").to_string(),
        None => format!("epoch {epoch_secs}"),
    }
}
