use super::normalize_cron;
use std::str::FromStr;

#[test]
fn normalize_5_to_7_field() {
    assert_eq!(normalize_cron("0 2 * * *"), "0 0 2 * * * *");
    assert_eq!(normalize_cron("*/5 * * * *"), "0 */5 * * * * *");
    assert_eq!(normalize_cron("30 4 1,15 * *"), "0 30 4 1,15 * * *");
}

#[test]
fn normalize_6_to_7_field() {
    // sec + 5 fields → append year wildcard
    assert_eq!(normalize_cron("0 0 2 * * *"), "0 0 2 * * * *");
    assert_eq!(normalize_cron("15 */10 * * * *"), "15 */10 * * * * *");
}

#[test]
fn normalize_7_field_passthrough() {
    assert_eq!(normalize_cron("30 0 2 * * * *"), "30 0 2 * * * *");
}

#[test]
fn parse_5_field_cron() {
    let expr = "0 2 * * *";
    let normalized = normalize_cron(expr);
    let schedule = cron::Schedule::from_str(&normalized);
    assert!(
        schedule.is_ok(),
        "normalized 5-field cron should parse: {normalized}"
    );
}

#[test]
fn parse_6_field_cron() {
    let expr = "0 0 2 * * *";
    let normalized = normalize_cron(expr);
    let schedule = cron::Schedule::from_str(&normalized);
    assert!(
        schedule.is_ok(),
        "normalized 6-field cron should parse: {normalized}"
    );
}

#[test]
fn parse_7_field_cron() {
    let expr = "30 0 2 * * * *";
    let schedule = cron::Schedule::from_str(expr);
    assert!(schedule.is_ok(), "valid 7-field cron should parse");
}

#[test]
fn reject_invalid_cron() {
    let expr = "invalid string";
    let normalized = normalize_cron(expr);
    // best-effort passthrough — Schedule::from_str must still fail
    assert!(cron::Schedule::from_str(&normalized).is_err());
}

#[test]
fn daily_cron_upcoming() {
    let expr = "0 2 * * *";
    let normalized = normalize_cron(expr);
    let schedule = cron::Schedule::from_str(&normalized).unwrap();
    let mut upcoming = schedule.upcoming(chrono::Utc);
    assert!(upcoming.next().is_some(), "should have an upcoming fire time");
}

#[test]
fn every_minute_cron_has_upcoming() {
    let normalized = normalize_cron("* * * * *");
    let schedule = cron::Schedule::from_str(&normalized).unwrap();
    let next = schedule.upcoming(chrono::Utc).next();
    assert!(next.is_some());
}
