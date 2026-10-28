use manager::services::yaml_cache::new_cache;
use manager::services::yaml_cache::parse_in_cache;
use manager::services::yaml_cache::CAP;
use sha2::Digest;

fn sample(name: &str) -> String {
    format!("name: {name}\nenvironments: [default]\nsteps:\n  - name: a\n    run: echo hi\n")
}

#[test]
fn identical_content_reuses_cached_parse_and_differs_on_new_content() {
    let mut cache = new_cache();

    let first = parse_in_cache(&mut cache, &sample("demo"));
    let second = parse_in_cache(&mut cache, &sample("demo"));
    assert_eq!(first.structural.name, second.structural.name);
    assert_eq!(
        second.parsed.as_ref().expect("parses").name,
        second.structural.name.clone().expect("name set")
    );

    let changed = parse_in_cache(&mut cache, &sample("other"));
    assert_eq!(changed.structural.name.as_deref(), Some("other"));
}

#[test]
fn fifo_eviction_bounds_the_cache() {
    let mut cache = new_cache();
    for i in 0..=CAP {
        parse_in_cache(&mut cache, &sample(&format!("pipeline-{i}")));
    }
    let cache = cache.expect("initialized");
    assert_eq!(cache.map.len(), CAP, "cache remains bounded");
    assert_eq!(cache.order.len(), CAP, "FIFO retains no stale order keys");
    assert!(!cache.map.contains_key(&format!(
        "{:x}",
        sha2::Sha256::digest(sample("pipeline-0").as_bytes())
    )));
}

#[test]
fn broken_yaml_caches_structural_error_and_absent_parse() {
    let mut cache = new_cache();
    let bad = "name: [unclosed";
    let first = parse_in_cache(&mut cache, bad);
    assert!(!first.structural.valid, "syntax error is structural");
    assert!(first.parsed.is_none(), "no typed doc for broken yaml");

    let second = parse_in_cache(&mut cache, bad);
    assert!(!second.structural.valid);
    assert!(second.parsed.is_none());
    assert_eq!(cache.expect("initialized").map.len(), 1);
}
