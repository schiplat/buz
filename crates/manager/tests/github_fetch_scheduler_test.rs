//! Unit tests for the GitHub auto-fetch scheduler due logic.

use manager::services::github_fetch_scheduler::connection_is_due;

#[test]
fn null_last_fetch_is_due() {
    assert!(connection_is_due(None, 60));
}

#[test]
fn unparseable_last_fetch_is_due() {
    assert!(connection_is_due(Some("not-a-date"), 60));
    assert!(connection_is_due(Some(""), 60));
}

#[test]
fn recent_fetch_is_not_due() {
    let now = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
    assert!(!connection_is_due(Some(&now), 60));
}

#[test]
fn old_fetch_is_due() {
    let old = chrono::Utc::now()
        .checked_sub_signed(chrono::Duration::hours(2))
        .expect("sub")
        .format("%Y-%m-%d %H:%M:%S")
        .to_string();
    assert!(connection_is_due(Some(&old), 60));
}

#[test]
fn boundary_exactly_at_interval_is_due() {
    let old = chrono::Utc::now()
        .checked_sub_signed(chrono::Duration::minutes(30))
        .expect("sub")
        .format("%Y-%m-%d %H:%M:%S")
        .to_string();
    // Truncated to seconds, so 30 min ago is ≥ the 30-min interval.
    assert!(connection_is_due(Some(&old), 30));
}

#[test]
fn short_interval_with_old_fetch_is_due() {
    let old = chrono::Utc::now()
        .checked_sub_signed(chrono::Duration::minutes(5))
        .expect("sub")
        .format("%Y-%m-%d %H:%M:%S")
        .to_string();
    assert!(connection_is_due(Some(&old), 2));
    assert!(!connection_is_due(Some(&old), 10));
}
