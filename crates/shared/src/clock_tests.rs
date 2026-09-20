use super::{describe_skew, format_utc, parse_http_date, skew_from_http_date, SkewSample};
use chrono::{TimeZone, Utc};

fn at(secs: i64) -> chrono::DateTime<Utc> {
    Utc.timestamp_opt(secs, 0)
        .single()
        .expect("valid timestamp")
}

#[test]
fn parse_http_date_reads_oss_style_header() {
    let parsed = parse_http_date("Sun, 20 Sep 2026 12:54:56 GMT").expect("parse Date header");
    assert_eq!(parsed.timestamp(), 1789908896);
    assert_eq!(parsed.naive_utc().to_string(), "2026-09-20 12:54:56");
}

#[test]
fn parse_http_date_tolerates_surrounding_whitespace() {
    let parsed = parse_http_date("  Sun, 20 Sep 2026 12:54:56 GMT \r\n").expect("trim + parse");
    assert_eq!(parsed.timestamp(), 1789908896);
}

#[test]
fn parse_http_date_rejects_garbage() {
    let err = parse_http_date("not a date").expect_err("garbage should be rejected");
    assert!(err.contains("Date header"), "{err}");
}

#[test]
fn skew_is_remote_minus_local_after_discounting_half_rtt() {
    let sent = at(1_000_000);
    let received = at(1_000_002); // 2s round-trip
    let remote = at(1_000_101); // remote = sent + 100s

    // Midpoint is sent + 1s → skew 100s, not 101s / 99s.
    assert_eq!(skew_from_http_date(sent, received, remote), 100);
}

#[test]
fn skew_is_negative_when_local_clock_is_ahead() {
    let sent = at(2_000_000);
    let received = at(2_000_000);
    assert_eq!(skew_from_http_date(sent, received, at(1_999_960)), -40);
}

#[test]
fn zero_rtt_is_exact() {
    let t = at(1_700_000_000);
    assert_eq!(skew_from_http_date(t, t, t), 0);
}

#[test]
fn sample_reports_behind_only_for_slow_local_clock() {
    let behind = SkewSample {
        source: "https://bucket.oss.example.com/".into(),
        skew_secs: 9600,
        checked_at: at(1_700_000_000),
    };
    assert_eq!(behind.local_behind_secs(), 9600);
    assert!(behind.is_skewed(60));
    assert!(behind.is_skewed(9600));

    let ahead = SkewSample {
        source: "https://bucket.oss.example.com/".into(),
        skew_secs: -120,
        checked_at: at(1_700_000_000),
    };
    assert_eq!(ahead.local_behind_secs(), 0);
    assert!(!ahead.is_skewed(60));
}

#[test]
fn skew_below_threshold_is_not_flagged() {
    let sample = SkewSample {
        source: "https://manager.example.com/".into(),
        skew_secs: 59,
        checked_at: at(1_700_000_000),
    };
    assert!(!sample.is_skewed(60));
}

#[test]
fn describe_skew_uses_signed_wording() {
    assert_eq!(describe_skew(9600), "9600s behind");
    assert_eq!(describe_skew(0), "0s behind");
    assert_eq!(describe_skew(-45), "45s ahead");
}

#[test]
fn format_utc_renders_absolute_stamp() {
    // 1789902875 = 2026-09-20 11:14:35 UTC (the Expires of the incident URL).
    assert_eq!(format_utc(1_789_902_875), "2026-09-20 11:14:35 UTC");
    assert_eq!(format_utc(0), "1970-01-01 00:00:00 UTC");
    assert_eq!(format_utc(-1), "1969-12-31 23:59:59 UTC");
}
