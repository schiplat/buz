use std::sync::atomic::Ordering;
use std::sync::Mutex;

use super::{parse_cached, reset_for_test, CAP, HITS, MISSES};

/// The cache is process-global and other test modules exercise it through
/// `validate_pipeline_yaml`; stat assertions need exclusive access.
static SERIAL: Mutex<()> = Mutex::new(());

fn sample(name: &str) -> String {
    format!("name: {name}\nenvironments: [default]\nsteps:\n  - name: a\n    run: echo hi\n")
}

fn sample_n(i: usize) -> String {
    sample(&format!("pipeline-{i}"))
}

#[test]
fn identical_content_hits_and_differs_misses() {
    let _guard = SERIAL.lock().unwrap();
    reset_for_test();

    let first = parse_cached(&sample("demo"));
    assert_eq!(MISSES.load(Ordering::Relaxed), 1, "first call misses");
    assert_eq!(HITS.load(Ordering::Relaxed), 0);

    let second = parse_cached(&sample("demo"));
    assert_eq!(MISSES.load(Ordering::Relaxed), 1, "unchanged content hits");
    assert_eq!(HITS.load(Ordering::Relaxed), 1);
    assert_eq!(first.structural.name, second.structural.name);
    assert_eq!(
        second.parsed.as_ref().expect("parses").name,
        second.structural.name.clone().expect("name set")
    );

    parse_cached(&sample("other"));
    assert_eq!(MISSES.load(Ordering::Relaxed), 2, "new content misses");
}

#[test]
fn fifo_eviction_bounds_the_cache() {
    let _guard = SERIAL.lock().unwrap();
    reset_for_test();

    for i in 0..=CAP {
        parse_cached(&sample_n(i));
    }
    assert_eq!(MISSES.load(Ordering::Relaxed), CAP + 1);

    // The oldest entry (0) was evicted — it misses again.
    parse_cached(&sample_n(0));
    assert_eq!(MISSES.load(Ordering::Relaxed), CAP + 2);

    // The newest entry (CAP) is still resident.
    parse_cached(&sample_n(CAP));
    assert_eq!(HITS.load(Ordering::Relaxed), 1);
}

#[test]
fn broken_yaml_caches_structural_error_and_absent_parse() {
    let _guard = SERIAL.lock().unwrap();
    reset_for_test();

    let bad = "name: [unclosed";
    let first = parse_cached(bad);
    assert!(!first.structural.valid, "syntax error is structural");
    assert!(first.parsed.is_none(), "no typed doc for broken yaml");

    let second = parse_cached(bad);
    assert!(!second.structural.valid);
    assert!(second.parsed.is_none());
    assert_eq!(HITS.load(Ordering::Relaxed), 1, "broken yaml is cached too");
}
