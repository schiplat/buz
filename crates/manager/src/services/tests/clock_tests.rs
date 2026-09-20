use super::{
    expiry_error_message, expiry_margin_secs, guard_signing, host_allowed, oss_source_url,
    port_allowed, probe_target, url_born_expired, ClockGuard,
};
use crate::config::ClockConfig;
use chrono::{Duration as ChronoDuration, Utc};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::io::AsyncWriteExt;

const TTL: i64 = 3600;

/// Probe tests target a loopback HTTP server, so the allowlist mirrors that.
fn cfg(enabled: bool, cache_ttl_seconds: u64) -> ClockConfig {
    ClockConfig {
        enabled,
        warn_threshold_seconds: 60,
        cache_ttl_seconds,
        allowed_hosts: vec!["127.0.0.1".to_string(), "*.aliyuncs.com".to_string()],
    }
}

fn guard(enabled: bool, cache_ttl_seconds: u64) -> ClockGuard {
    ClockGuard::new(cfg(enabled, cache_ttl_seconds))
}

fn http_date(offset_secs: i64) -> String {
    (Utc::now() + ChronoDuration::seconds(offset_secs))
        .format("%a, %d %b %Y %H:%M:%S GMT")
        .to_string()
}

/// Minimal HTTP/1.1 server that always answers `status`, optionally with a
/// `Date` header — mirrors OSS replying 403 while still sending its clock.
/// Returns the base URL and a request counter.
async fn spawn_probe_server(
    date: Option<String>,
    status: &'static str,
) -> (String, Arc<AtomicUsize>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind probe server");
    let addr = listener.local_addr().expect("probe server addr");
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = hits.clone();

    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                break;
            };
            counter.fetch_add(1, Ordering::SeqCst);
            let mut resp =
                format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n");
            if let Some(d) = &date {
                resp.push_str(&format!("Date: {d}\r\n"));
            }
            resp.push_str("\r\n");
            let _ = sock.write_all(resp.as_bytes()).await;
            let _ = sock.flush().await;
            let _ = sock.shutdown().await;
        }
    });

    (format!("http://{addr}/"), hits)
}

/// Minimal server answering `302` with `Location: {target}` (no `Date` header),
/// used to prove the guard does not chase redirects.
async fn spawn_redirector(target: String) -> (String, Arc<AtomicUsize>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind redirector");
    let addr = listener.local_addr().expect("redirector addr");
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = hits.clone();

    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                break;
            };
            counter.fetch_add(1, Ordering::SeqCst);
            let resp = format!(
                "HTTP/1.1 302 Found\r\nLocation: {target}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
            let _ = sock.write_all(resp.as_bytes()).await;
            let _ = sock.flush().await;
            let _ = sock.shutdown().await;
        }
    });

    (format!("http://{addr}/"), hits)
}

// -- pure helpers --

// `Date` parsing / half-RTT skew math is shared with the Runner and covered by
// `shared::clock` tests — only the Manager guard policy is tested here.

#[test]
fn oss_source_url_builds_https_base_and_tolerates_scheme() {
    assert_eq!(
        oss_source_url("example-bucket", "oss-cn-hongkong.aliyuncs.com"),
        "https://example-bucket.oss-cn-hongkong.aliyuncs.com/"
    );
    assert_eq!(
        oss_source_url("bucket ", " https://oss.example.com/ "),
        "https://bucket.oss.example.com/"
    );
}

#[test]
fn expiry_margin_scales_with_ttl_and_is_capped() {
    assert_eq!(expiry_margin_secs(TTL), 300);
    assert_eq!(expiry_margin_secs(60), 5);
    assert_eq!(expiry_margin_secs(0), 5);
}

#[test]
fn url_born_expired_only_for_long_skew() {
    // A little skew is fine: the URL still has almost its full TTL.
    assert!(!url_born_expired(0, TTL));
    assert!(!url_born_expired(60, TTL));
    // Beyond the safety margin the URL arrives expired / not worth using.
    assert!(url_born_expired(TTL - 300, TTL));
    assert!(url_born_expired(4000, TTL));
    // Short TTLs use a proportionally smaller margin.
    assert!(!url_born_expired(30, 60));
    assert!(url_born_expired(55, 60));
}

#[test]
fn expiry_message_names_source_skew_and_fix() {
    let msg = expiry_error_message("https://bucket.oss.example.com/", 9600, TTL);
    assert!(
        msg.contains("9600s behind https://bucket.oss.example.com/"),
        "{msg}"
    );
    assert!(msg.contains("Request has expired"), "{msg}");
    assert!(msg.contains("timedatectl set-ntp true"), "{msg}");
}

// -- probe target validation (SSRF gate) --

#[test]
fn probe_target_extracts_and_normalises_the_host() {
    assert_eq!(
        probe_target("https://example-bucket.oss-cn-hongkong.aliyuncs.com/").unwrap(),
        (
            "example-bucket.oss-cn-hongkong.aliyuncs.com".to_string(),
            None
        )
    );
    // Case and a path/query tail do not change the host.
    assert_eq!(
        probe_target("https://OSS.Example.COM/health?x=1#f").unwrap(),
        ("oss.example.com".to_string(), None)
    );
}

#[test]
fn probe_target_rejects_hosts_that_hide_a_different_target() {
    // `user@169.254.169.254` would connect to the IP, not to `a.x`.
    let err = probe_target("https://a.x@169.254.169.254:80/").expect_err("userinfo rejected");
    assert!(err.contains("embedded credentials"), "{err}");

    for bad in [
        "http://oss.example.com/",          // plain http (non-loopback)
        "ftp://oss.example.com/",           // unsupported scheme
        "https:///",                        // no host
        "oss.example.com/",                 // missing scheme
        "https://oss example.com/",         // whitespace in host
        "https://oss%2eexample.com/",       // percent-encoded host
        "http://metadata.google.internal/", // http to a non-loopback host
        "http://169.254.169.254/latest/",   // http metadata endpoint
    ] {
        assert!(probe_target(bad).is_err(), "{bad} must be refused");
    }
}

#[test]
fn host_allowed_supports_wildcards_exact_and_rejects_catch_all() {
    let patterns = vec![
        "*.aliyuncs.com".to_string(),
        "oss.internal.example".to_string(),
    ];

    assert!(host_allowed(
        "example-bucket.oss-cn-hongkong.aliyuncs.com",
        &patterns
    ));
    assert!(host_allowed("aliyuncs.com", &patterns));
    assert!(host_allowed("oss.internal.example", &patterns));
    // Case / trailing dot normalisation.
    assert!(host_allowed("OSS-CN-Hongkong.AliYunCs.CoM.", &patterns));

    assert!(!host_allowed("aliyuncs.com.attacker.example", &patterns));
    assert!(!host_allowed("evil-aliyuncs.com", &patterns));
    assert!(!host_allowed("169.254.169.254", &patterns));
    assert!(!host_allowed(
        "oss.internal.example.attacker.example",
        &patterns
    ));

    // A `*` pattern would allow anything → ignored, and an empty list denies all.
    assert!(!host_allowed("anything.example", &["*".to_string()]));
    assert!(!host_allowed("oss-cn-hongkong.aliyuncs.com", &[]));
}

#[tokio::test]
async fn sample_refuses_hosts_outside_the_allowlist() {
    let guard = guard(true, 300);

    for blocked in [
        "https://attacker.example/",
        "https://a.x@169.254.169.254:80/",
        "http://169.254.169.254/latest/",
    ] {
        let err = guard
            .sample(blocked)
            .await
            .expect_err("non-allowlisted target must not be dialed");
        assert!(
            err.contains("clock probe refused") || err.contains("not in clock.allowed_hosts"),
            "{blocked}: {err}"
        );
    }
    assert_eq!(guard.latest(), None);
}

#[tokio::test]
async fn sample_refuses_everything_when_the_allowlist_is_empty() {
    let guard = ClockGuard::new(ClockConfig {
        allowed_hosts: vec![],
        ..cfg(true, 300)
    });
    let err = guard
        .sample("https://example-bucket.oss-cn-hongkong.aliyuncs.com/")
        .await
        .expect_err("empty allowlist disables probing");
    assert!(err.contains("clock.allowed_hosts is empty"), "{err}");
}

#[tokio::test]
async fn guard_signing_fails_open_for_non_allowlisted_sources() {
    // The guard is a diagnostic, not a gate: an endpoint outside the allowlist
    // must not block dispatch, and must not be contacted either.
    let guard = guard(true, 300);
    assert_eq!(
        guard_signing(&guard, "https://attacker.example/", TTL).await,
        None
    );
}

#[test]
fn probe_target_reports_explicit_ports_only_when_valid() {
    assert_eq!(
        probe_target("https://oss.example.com/").unwrap(),
        ("oss.example.com".to_string(), None)
    );
    assert_eq!(
        probe_target("https://oss.example.com:443/").unwrap(),
        ("oss.example.com".to_string(), Some(443))
    );
    assert_eq!(
        probe_target("https://OSS.Example.com:9000/path").unwrap(),
        ("oss.example.com".to_string(), Some(9000))
    );

    for bad in [
        "https://oss.example.com:abc/",
        "https://oss.example.com:/",
        "https://oss.example.com:9000:80/",
        "https://[::1]:443/",
    ] {
        assert!(probe_target(bad).is_err(), "{bad} must be refused");
    }
}

#[test]
fn port_allowed_requires_an_explicit_allowlist_entry() {
    let patterns = vec![
        "*.aliyuncs.com".to_string(),
        "minio.internal:9000".to_string(),
    ];

    // The scheme default is always fine.
    assert!(port_allowed("oss-cn-hongkong.aliyuncs.com", 443, &patterns));
    // Any other port needs to be named by a matching allowlist entry.
    assert!(!port_allowed(
        "oss-cn-hongkong.aliyuncs.com",
        8443,
        &patterns
    ));
    assert!(!port_allowed("minio.internal", 9001, &patterns));
    assert!(port_allowed("minio.internal", 9000, &patterns));
    // Named port on the wrong host stays refused, and wildcards carry over.
    assert!(!port_allowed("evil.internal", 9000, &patterns));
    assert!(port_allowed(
        "minio.internal",
        9000,
        &["*.internal:9000".to_string()]
    ));
    // A port-named entry still matches its host for the allowlist itself.
    assert!(host_allowed(
        "minio.internal",
        &["minio.internal:9000".to_string()]
    ));
}

#[tokio::test]
async fn sample_refuses_non_default_ports_on_allowlisted_hosts() {
    let guard = guard(true, 300);

    // Allowlisted host, but the endpoint points at another port — the guard
    // must not become a port scanner.
    let err = guard
        .sample("https://127.0.0.1:8443/")
        .await
        .expect_err("non-default port must not be probed");
    assert!(err.contains("port 8443"), "{err}");
    assert!(err.contains("clock.allowed_hosts"), "{err}");

    // Naming the port in the allowlist restores the guard (probe then runs).
    let guard = ClockGuard::new(ClockConfig {
        allowed_hosts: vec!["buz.oss.internal:9000".to_string()],
        ..cfg(true, 300)
    });
    let err = guard
        .sample("https://buz.oss.internal:9000/")
        .await
        .expect_err("allowlisted target is dialed (and unreachable here)");
    assert!(
        !err.contains("clock probe refused"),
        "the port gate must not fire: {err}"
    );
}

#[tokio::test]
async fn probe_does_not_follow_redirects() {
    // A redirect is the other way an allowlisted host could steer the Manager
    // at an arbitrary target (and downgrade to http) — it must be returned,
    // not chased.
    let (victim_url, victim_hits) = spawn_probe_server(Some(http_date(0)), "200 OK").await;
    let (redirect_url, _) = spawn_redirector(victim_url).await;
    let guard = guard(true, 300);

    let err = guard
        .sample(&redirect_url)
        .await
        .expect_err("302 response carries no Date header");
    assert!(err.contains("no Date header"), "{err}");
    assert_eq!(
        victim_hits.load(Ordering::SeqCst),
        0,
        "the redirect target must never be contacted"
    );
}

// -- probing --

#[tokio::test]
async fn sample_uses_remote_date_header() {
    let (url, hits) = spawn_probe_server(Some(http_date(120)), "403 Forbidden").await;
    let guard = guard(true, 300);

    let sample = guard.sample(&url).await.expect("probe succeeds on 403");

    // HTTP dates have 1s resolution → allow ±2s around the injected offset.
    assert!(
        (115..=123).contains(&sample.skew_secs),
        "expected ~120s skew, got {}",
        sample.skew_secs
    );
    assert_eq!(sample.local_behind_secs(), sample.skew_secs);
    assert_eq!(hits.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn sample_reports_clock_ahead_as_zero_behind() {
    let (url, _) = spawn_probe_server(Some(http_date(-90)), "200 OK").await;
    let guard = guard(true, 300);

    let sample = guard.sample(&url).await.expect("probe succeeds");
    assert!(sample.skew_secs < 0, "clock ahead: {}", sample.skew_secs);
    assert_eq!(sample.local_behind_secs(), 0);
    assert!(!sample.is_skewed(60));
}

#[tokio::test]
async fn sample_reuses_cache_within_ttl() {
    let (url, hits) = spawn_probe_server(Some(http_date(0)), "200 OK").await;
    let guard = guard(true, 300);

    guard.sample(&url).await.expect("first probe");
    guard.sample(&url).await.expect("cached probe");

    assert_eq!(
        hits.load(Ordering::SeqCst),
        1,
        "second call must hit the cache"
    );
}

#[tokio::test]
async fn sample_reprobes_when_cache_ttl_is_zero() {
    let (url, hits) = spawn_probe_server(Some(http_date(0)), "200 OK").await;
    let guard = guard(true, 0);

    guard.sample(&url).await.expect("first probe");
    guard.sample(&url).await.expect("second probe");

    assert_eq!(hits.load(Ordering::SeqCst), 2, "ttl 0 disables caching");
}

#[tokio::test]
async fn sample_fails_without_date_header_and_caches_the_failure() {
    let (url, hits) = spawn_probe_server(None, "403 Forbidden").await;
    let guard = guard(true, 300);

    let err = guard.sample(&url).await.expect_err("no Date header");
    assert!(err.contains("no Date header"), "{err}");
    let again = guard.sample(&url).await.expect_err("cached failure");
    assert_eq!(err, again);
    assert_eq!(hits.load(Ordering::SeqCst), 1, "failure is cached too");
    assert_eq!(guard.latest(), None, "failed probe is not a sample");
    assert!(guard.last_error().is_some());
}

#[tokio::test]
async fn latest_tracks_successful_samples() {
    let (url, _) = spawn_probe_server(Some(http_date(30)), "200 OK").await;
    let guard = guard(true, 300);

    assert_eq!(guard.latest(), None);
    guard.sample(&url).await.expect("probe");
    let latest = guard.latest().expect("sample recorded");
    assert_eq!(latest.source, url);
    assert!(
        (25..=32).contains(&latest.local_behind_secs()),
        "expected ~30s behind, got {}",
        latest.local_behind_secs()
    );
}

// -- signing guard --

#[tokio::test]
async fn guard_signing_blocks_url_that_would_be_born_expired() {
    // Manager clock ~66 min behind the remote: the 1h URL is already expired.
    let (url, _) = spawn_probe_server(Some(http_date(4000)), "403 Forbidden").await;
    let guard = guard(true, 300);

    let msg = guard_signing(&guard, &url, TTL)
        .await
        .expect("slow clock must block signing");
    assert!(msg.contains("behind"), "{msg}");
    assert!(msg.contains("Request has expired"), "{msg}");
}

#[tokio::test]
async fn guard_signing_allows_small_skew() {
    let (url, _) = spawn_probe_server(Some(http_date(120)), "200 OK").await;
    let guard = guard(true, 300);

    assert_eq!(guard_signing(&guard, &url, TTL).await, None);
}

#[tokio::test]
async fn guard_signing_is_skipped_when_disabled() {
    let (url, hits) = spawn_probe_server(Some(http_date(4000)), "200 OK").await;
    let guard = guard(false, 300);

    assert_eq!(guard_signing(&guard, &url, TTL).await, None);
    assert_eq!(
        hits.load(Ordering::SeqCst),
        0,
        "disabled guard must not probe"
    );
}

#[tokio::test]
async fn guard_signing_continues_when_probe_is_unreachable() {
    let guard = guard(true, 300);
    // Nothing listens on port 1 → probe fails; dispatch must not be blocked.
    assert_eq!(
        guard_signing(&guard, "http://127.0.0.1:1/", TTL).await,
        None
    );
}
