use super::{probe, probe_url, startup_error_message, warn_bucket, ClockMonitor};
use crate::config::{ClockSection, Config, RunnerSection};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::io::AsyncWriteExt;

fn clock_config() -> ClockSection {
    ClockSection {
        enabled: true,
        warn_threshold_seconds: 60,
        max_skew_seconds: 300,
        fail_on_skew: true,
    }
}

fn test_config(clock: ClockSection, manager_url: &str) -> Config {
    Config {
        runner: RunnerSection {
            manager_url: manager_url.to_string(),
            ..Default::default()
        },
        clock,
        ..Default::default()
    }
}

fn http_date(offset_secs: i64) -> String {
    (Utc::now() + ChronoDuration::seconds(offset_secs))
        .format("%a, %d %b %Y %H:%M:%S GMT")
        .to_string()
}

/// Minimal HTTP server answering `status` with the given `Date` header.
/// Returns the base URL and a request counter.
async fn spawn_server(date: Option<String>, status: &'static str) -> (String, Arc<AtomicUsize>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind server");
    let addr = listener.local_addr().expect("server addr");
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

    (format!("http://{addr}"), hits)
}

fn headers_with_date(date: &str) -> reqwest::header::HeaderMap {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::DATE,
        reqwest::header::HeaderValue::from_str(date).expect("valid header value"),
    );
    headers
}

// -- URL + message helpers --

#[test]
fn probe_url_targets_unauthenticated_health() {
    assert_eq!(
        probe_url("http://127.0.0.1:8080"),
        "http://127.0.0.1:8080/health"
    );
    assert_eq!(
        probe_url("https://manager.example.com/"),
        "https://manager.example.com/health"
    );
    assert_eq!(
        probe_url("  https://manager.example.com//  "),
        "https://manager.example.com/health"
    );
}

#[test]
fn startup_error_message_names_skew_limit_and_fix() {
    let msg = startup_error_message("https://manager.example.com/health", 9600, 300);
    assert!(msg.contains("9600s behind"), "{msg}");
    assert!(msg.contains("limit 300s"), "{msg}");
    assert!(msg.contains("timedatectl set-ntp true"), "{msg}");
    assert!(msg.contains("fail_on_skew = false"), "{msg}");
}

#[test]
fn startup_error_message_handles_runner_clock_ahead() {
    let msg = startup_error_message("https://manager.example.com/health", -400, 300);
    assert!(msg.contains("400s ahead"), "{msg}");
}

// -- probing --

#[tokio::test]
async fn probe_reads_manager_date_header() {
    let (url, _) = spawn_server(Some(http_date(45)), "200 OK").await;
    let client = reqwest::Client::new();

    let sample = probe(&client, &url).await.expect("probe succeeds");
    assert_eq!(sample.source, format!("{url}/health"));
    // HTTP dates are second-resolution, so allow a small tolerance around 45s.
    assert!(
        (40..=47).contains(&sample.skew_secs),
        "expected ~45s skew, got {}",
        sample.skew_secs
    );
    assert!(!sample.is_skewed(60));
}

#[tokio::test]
async fn probe_survives_non_success_status() {
    let (url, _) = spawn_server(Some(http_date(10)), "403 Forbidden").await;
    let client = reqwest::Client::new();

    let sample = probe(&client, &url).await.expect("403 still carries Date");
    assert!(
        (5..=11).contains(&sample.skew_secs),
        "expected ~10s skew, got {}",
        sample.skew_secs
    );
}

#[tokio::test]
async fn probe_errors_without_date_header() {
    let (url, _) = spawn_server(None, "200 OK").await;
    let client = reqwest::Client::new();

    let err = probe(&client, &url).await.expect_err("no Date header");
    assert!(err.contains("no Date header"), "{err}");
}

#[tokio::test]
async fn probe_errors_when_manager_unreachable() {
    let client = reqwest::Client::new();
    let err = probe(&client, "http://127.0.0.1:1")
        .await
        .expect_err("nothing listens on port 1");
    assert!(err.contains("127.0.0.1:1/health"), "{err}");
}

// -- startup policy --

#[tokio::test]
async fn startup_fails_when_skew_exceeds_limit() {
    // Manager ~66 min ahead of the Runner → grossly skewed host.
    let (url, _) = spawn_server(Some(http_date(4000)), "200 OK").await;
    let client = reqwest::Client::new();
    let config = test_config(clock_config(), &url);

    let err = super::verify_at_startup(&client, &config)
        .await
        .expect_err("skew beyond max_skew_seconds must abort startup");
    assert!(err.to_string().contains("Runner clock is"), "{err}");
}

#[tokio::test]
async fn startup_continues_when_fail_on_skew_is_disabled() {
    let (url, _) = spawn_server(Some(http_date(4000)), "200 OK").await;
    let client = reqwest::Client::new();
    let clock = ClockSection {
        fail_on_skew: false,
        ..clock_config()
    };
    let config = test_config(clock, &url);

    let sample = super::verify_at_startup(&client, &config)
        .await
        .expect("fail_on_skew = false → start anyway")
        .expect("sample recorded");
    assert!(sample.skew_secs >= 3990, "{}", sample.skew_secs);
}

#[tokio::test]
async fn startup_passes_for_small_skew() {
    let (url, _) = spawn_server(Some(http_date(30)), "200 OK").await;
    let client = reqwest::Client::new();
    let config = test_config(clock_config(), &url);

    let sample = super::verify_at_startup(&client, &config)
        .await
        .expect("small skew is fine")
        .expect("sample recorded");
    assert!(
        (25..=32).contains(&sample.skew_secs),
        "expected ~30s skew, got {}",
        sample.skew_secs
    );
}

#[tokio::test]
async fn startup_skips_check_when_disabled() {
    let (url, hits) = spawn_server(Some(http_date(4000)), "200 OK").await;
    let client = reqwest::Client::new();
    let clock = ClockSection {
        enabled: false,
        ..clock_config()
    };
    let config = test_config(clock, &url);

    assert!(super::verify_at_startup(&client, &config)
        .await
        .expect("disabled check never fails")
        .is_none());
    assert_eq!(hits.load(Ordering::SeqCst), 0, "disabled → no probe");
}

#[tokio::test]
async fn startup_continues_when_probe_unavailable() {
    let client = reqwest::Client::new();
    let config = test_config(clock_config(), "http://127.0.0.1:1");

    // A broken probe must not stop a Runner that can still register.
    assert!(super::verify_at_startup(&client, &config)
        .await
        .expect("probe failure is not fatal")
        .is_none());
}

// -- heartbeat monitor --

#[test]
fn monitor_measures_skew_from_heartbeat_headers() {
    let monitor = ClockMonitor::new(clock_config(), "https://manager.example.com");
    assert_eq!(monitor.skew_secs(), None);

    let sent_at = Utc::now() - ChronoDuration::milliseconds(20);
    let remote = (Utc::now() + ChronoDuration::seconds(90))
        .format("%a, %d %b %Y %H:%M:%S GMT")
        .to_string();
    monitor.observe(sent_at, &headers_with_date(&remote));

    let skew = monitor.skew_secs().expect("skew recorded");
    assert!((85..=91).contains(&skew), "expected ~90s, got {skew}");
}

#[test]
fn monitor_ignores_missing_or_invalid_date_headers() {
    let monitor = ClockMonitor::new(clock_config(), "https://manager.example.com");

    monitor.observe(Utc::now(), &reqwest::header::HeaderMap::new());
    monitor.observe(Utc::now(), &headers_with_date("not a date"));

    assert_eq!(monitor.skew_secs(), None, "no sample from bad headers");
}

#[test]
fn monitor_ignores_observations_when_disabled() {
    let monitor = ClockMonitor::new(
        ClockSection {
            enabled: false,
            ..clock_config()
        },
        "https://manager.example.com",
    );

    let remote = (Utc::now() + ChronoDuration::seconds(9000))
        .format("%a, %d %b %Y %H:%M:%S GMT")
        .to_string();
    monitor.observe(Utc::now(), &headers_with_date(&remote));

    assert_eq!(monitor.skew_secs(), None);
}

#[test]
fn monitor_seed_supplies_startup_sample() {
    let monitor = ClockMonitor::new(clock_config(), "https://manager.example.com");
    monitor.seed(Some(shared::clock::SkewSample {
        source: "https://manager.example.com/health".into(),
        skew_secs: 12,
        checked_at: Utc::now(),
    }));
    assert_eq!(monitor.skew_secs(), Some(12));

    monitor.seed(None);
    assert_eq!(monitor.skew_secs(), None);
}

#[test]
fn warn_bucket_is_none_within_tolerance() {
    assert_eq!(warn_bucket(0, 60), None);
    assert_eq!(warn_bucket(59, 60), None);
    // Clock ahead of the Manager is never a "behind" warning.
    assert_eq!(warn_bucket(-600, 60), None);
}

#[test]
fn warn_bucket_groups_drift_by_minute() {
    assert_eq!(warn_bucket(60, 60), Some(1));
    assert_eq!(warn_bucket(119, 60), Some(1), "same minute → same bucket");
    assert_eq!(
        warn_bucket(120, 60),
        Some(2),
        "worsening drift → new bucket"
    );
    assert_eq!(warn_bucket(9600, 60), Some(160));
}

#[test]
fn monitor_track_records_latest_sample() {
    let monitor = ClockMonitor::new(clock_config(), "https://manager.example.com");
    let mut checked_at: DateTime<Utc> = Utc::now();

    for skew in [10, 90, 240] {
        checked_at += ChronoDuration::seconds(30);
        monitor.track(&shared::clock::SkewSample {
            source: "https://manager.example.com/health".into(),
            skew_secs: skew,
            checked_at,
        });
        assert_eq!(monitor.skew_secs(), Some(skew));
    }
}

#[test]
fn monitor_track_ignores_samples_when_disabled() {
    let monitor = ClockMonitor::new(
        ClockSection {
            enabled: false,
            ..clock_config()
        },
        "https://manager.example.com",
    );
    monitor.track(&shared::clock::SkewSample {
        source: "https://manager.example.com/health".into(),
        skew_secs: 9600,
        checked_at: Utc::now(),
    });
    assert_eq!(monitor.skew_secs(), None);
}
