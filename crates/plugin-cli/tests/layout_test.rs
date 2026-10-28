use plugin_cli::layout::*;

#[test]
fn base_url_strips_index_tail() {
    assert_eq!(
        base_url("https://x.example/main/plugins/buz/index.json"),
        "https://x.example/main/plugins/buz"
    );
    assert_eq!(
        base_url("https://x.example/main/plugins/buz"),
        "https://x.example/main/plugins/buz"
    );
    assert_eq!(
        base_url("https://x.example/main/plugins/buz/"),
        "https://x.example/main/plugins/buz"
    );
}

#[test]
fn join_url_trims_slashes() {
    assert_eq!(
        join_url("https://x/base/", "/a.json"),
        "https://x/base/a.json"
    );
}

#[test]
fn id_and_semver_gates() {
    assert!(is_valid_id("buz/hello"));
    assert!(is_valid_id("alice/oss-upload-2"));
    assert!(!is_valid_id("archive"));
    assert!(!is_valid_id("Buz/hello"));
    assert!(!is_valid_id("buz/Hello"));
    assert!(!is_valid_id("a/b/c"));
    assert!(is_semver("1.10.6"));
    assert!(!is_semver("1.10"));
    assert!(!is_semver("1.x.0"));
}
