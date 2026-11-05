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
        let client = crate::http_util::client_builder()
            .user_agent(concat!("buz-manager/", env!("CARGO_PKG_VERSION")))
            .timeout(PROBE_TIMEOUT)
            // Redirects are refused: the probe target is derived from a
            // team-scoped secret, and a followed `Location` would let that
            // secret aim the Manager at any host (SSRF), including plain HTTP.
            .redirect(reqwest::redirect::Policy::none())
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
    ///
    /// The target is validated first: the source URL is derived from a
    /// team-scoped `config_source` secret, so an unvalidated host would turn
    /// the Manager into a request proxy for any secret owner (SSRF). Skipped
    /// targets are reported as a probe failure — the guard is optional, the
    /// request is not optional to skip.
    pub async fn sample(&self, source: &str) -> Result<SkewSample, String> {
        if let Err(reason) = self.check_probe_target(source) {
            tracing::debug!(
                source = %source,
                reason = %reason,
                "clock: probe skipped — target host not allowed"
            );
            return Err(reason);
        }

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

    /// Gate the probe target: only `https` URLs whose host matches
    /// `clock.allowed_hosts` (and whose port is the default or explicitly
    /// allowlisted) may be dialed.
    fn check_probe_target(&self, source: &str) -> Result<(), String> {
        let (host, port) = probe_target(source)?;
        if self.config.allowed_hosts.is_empty() {
            return Err("clock probe disabled: clock.allowed_hosts is empty".into());
        }
        if !host_allowed(&host, &self.config.allowed_hosts) {
            return Err(format!(
                "clock probe refused: host '{host}' is not in clock.allowed_hosts \
                 (the config_source endpoint is not a trusted time source)"
            ));
        }
        // An allowlisted host must not become an arbitrary-port scanner. The
        // guard's own tests probe a plain-HTTP loopback server on an ephemeral
        // port, which is the one target the port rule cannot cover.
        let test_loopback = loopback_http_tolerated(source);
        match port {
            Some(p) if !test_loopback && !port_allowed(&host, p, &self.config.allowed_hosts) => {
                Err(format!(
                    "clock probe refused: port {p} on '{host}' is not allowed — name it \
                     explicitly in clock.allowed_hosts (e.g. \"{host}:{p}\") if it is a \
                     trusted S3-compatible endpoint"
                ))
            }
            _ => Ok(()),
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
///
/// Formatting only — the result must pass [`probe_host`] + [`host_allowed`]
/// before it is dialed (see `ClockGuard::check_probe_target`).
pub fn oss_source_url(bucket: &str, endpoint: &str) -> String {
    let host = endpoint
        .trim()
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_end_matches('/');
    format!("https://{}.{}/", bucket.trim(), host)
}

/// Test-support switch for the loopback-HTTP probe tests. Production code never
/// calls this; even when enabled, only loopback literals are tolerated and
/// plain `http` to any other host is still refused.
#[doc(hidden)]
pub fn allow_loopback_http_for_tests() {
    ALLOW_LOOPBACK_HTTP.store(true, std::sync::atomic::Ordering::Relaxed);
}

static ALLOW_LOOPBACK_HTTP: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

fn loopback_http_tolerated(source: &str) -> bool {
    ALLOW_LOOPBACK_HTTP.load(std::sync::atomic::Ordering::Relaxed)
        && source
            .strip_prefix("http://")
            .map(|rest| {
                let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
                let host = authority.split(':').next().unwrap_or("");
                matches!(host, "127.0.0.1" | "localhost" | "[::1]" | "::1")
            })
            .unwrap_or(false)
}

/// Extract the host from a probe source URL, rejecting anything that could be
/// used to reach a different target than the one shown.
///
/// Rejects: non-`https` schemes, embedded credentials (`user@host`), paths,
/// queries, fragments and characters that are not valid in a DNS name — all of
/// which are ways to smuggle a different authority past a naive prefix check.
/// An explicit port is only accepted when the allowlist names it
/// (see [`port_allowed`]).
///
/// `oss_source_url` always builds `https://`, so plain `http` can only come
/// from a hand-written source. The probe tests exercise this guard against a
/// local plain-HTTP server, so [`allow_loopback_http_for_tests`] opens a
/// **loopback-only** exception; plain `http` to any other host stays refused.
pub fn probe_target(source: &str) -> Result<(String, Option<u16>), String> {
    let rest = match source.strip_prefix("https://") {
        Some(rest) => rest,
        None if loopback_http_tolerated(source) => source
            .strip_prefix("http://")
            .ok_or_else(|| format!("clock probe refused: '{source}' is not an https URL"))?,
        None => {
            return Err(format!(
                "clock probe refused: '{source}' is not an https URL"
            ))
        }
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.is_empty() {
        return Err(format!("clock probe refused: '{source}' has no host"));
    }
    if authority.contains('@') {
        return Err(
            "clock probe refused: URL carries embedded credentials (userinfo) — refusing to \
             resolve the host it would actually connect to"
                .into(),
        );
    }
    let (host_part, port_part) = match authority.split_once(':') {
        Some((host, port)) => (host, Some(port)),
        None => (authority, None),
    };
    let host = host_part.to_ascii_lowercase();
    if host.is_empty() {
        return Err(format!("clock probe refused: '{source}' has no host"));
    }
    if !host
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
    {
        return Err(format!(
            "clock probe refused: host '{host}' contains characters that are not valid in a DNS name"
        ));
    }
    let port =
        match port_part {
            None => None,
            Some("") => return Err(format!("clock probe refused: '{source}' has an empty port")),
            Some(p) => Some(p.parse::<u16>().map_err(|_| {
                format!("clock probe refused: '{source}' has an invalid port '{p}'")
            })?),
        };
    Ok((host, port))
}

/// True when `port` on an allowlisted `host` may be probed.
///
/// HTTPS defaults to 443; any other port has to be named explicitly by an
/// allowlist entry (`minio.internal:9000`), so an allowlisted host cannot be
/// turned into an arbitrary-port scanner.
pub fn port_allowed(host: &str, port: u16, patterns: &[String]) -> bool {
    if port == 443 {
        return true;
    }
    patterns.iter().any(|raw| {
        raw.trim()
            .rsplit_once(':')
            .is_some_and(|(pat_host, pat_port)| {
                pat_port.trim().parse::<u16>() == Ok(port)
                    && host_allowed(host, &[pat_host.to_string()])
            })
    })
}

/// True when `host` matches any allowlist pattern.
///
/// Patterns are exact (`oss.example.com`) or single-wildcard prefixes
/// (`*.aliyuncs.com`, which also matches the bare `aliyuncs.com`). An entry may
/// carry a port (`minio.internal:9000`) — only its host part is matched here,
/// the port is enforced by [`port_allowed`]. A pattern of `*` would allow
/// everything and is therefore ignored.
pub fn host_allowed(host: &str, patterns: &[String]) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    patterns.iter().any(|raw| {
        let raw = raw.trim();
        let pat = match raw.rsplit_once(':') {
            Some((h, p)) if p.trim().parse::<u16>().is_ok() => h.trim(),
            _ => raw,
        };
        let pat = pat.trim_end_matches('.').to_ascii_lowercase();
        if pat.is_empty() || pat == "*" {
            return false;
        }
        match pat.strip_prefix("*.") {
            Some(base) => host == base || host.ends_with(&format!(".{base}")),
            None => host == pat,
        }
    })
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
