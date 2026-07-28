use shared::version::{long_version, version_string, PRODUCT_NAME, VERSION};

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
