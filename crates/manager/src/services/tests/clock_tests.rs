use super::{
    expiry_error_message, expiry_margin_secs, guard_signing, oss_source_url, url_born_expired,
    ClockGuard,
};
use crate::config::ClockConfig;
use chrono::{Duration as ChronoDuration, Utc};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::io::AsyncWriteExt;

const TTL: i64 = 3600;

fn cfg(enabled: bool, cache_ttl_seconds: u64) -> ClockConfig {
    ClockConfig {
        enabled,
        warn_threshold_seconds: 60,
        cache_ttl_seconds,
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
