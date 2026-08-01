use shared::version::{
    long_version, version_string, MANAGER_LONG_VERSION, PRODUCT_NAME, RUNNER_LONG_VERSION, VERSION,
};

#[test]
fn version_matches_cargo_pkg() {
    assert_eq!(VERSION, env!("CARGO_PKG_VERSION"));
    assert!(!VERSION.is_empty());
    assert_eq!(version_string(), VERSION);
}

#[test]
fn long_version_includes_component() {
    let s = long_version("manager");
    assert!(s.contains(PRODUCT_NAME));
    assert!(s.contains("manager"));
    assert!(s.contains(VERSION));
}

#[test]
fn binary_long_versions_match_format() {
    assert!(MANAGER_LONG_VERSION.starts_with("Buz manager "));
    assert!(MANAGER_LONG_VERSION.ends_with(VERSION));
    assert!(RUNNER_LONG_VERSION.starts_with("Buz runner "));
    assert!(RUNNER_LONG_VERSION.ends_with(VERSION));
}
