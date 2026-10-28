use plugin_cli::scaffold::*;
use plugin_cli::validate;
use std::fs;

#[test]
fn scaffolds_spec_and_readme_that_pass_the_gate() {
    let dir = tempfile::tempdir().unwrap();
    run(
        "alice/hello",
        dir.path(),
        "community",
        Some("alice"),
        Some("https://github.com/alice/buz-plugins"),
        "shell",
        "0.1.0",
        Some("Say hello."),
    )
    .unwrap();
    let spec_path = dir.path().join("plugins/alice/hello/0.1.0.yaml");
    assert!(spec_path.exists());
    assert!(dir.path().join("plugins/alice/hello/README.md").exists());
    let spec = fs::read_to_string(&spec_path).unwrap();
    assert!(spec.contains("id: alice/hello"));
    // The scaffold pins the contract version it was generated against; derive it
    // from the crate so a release bump does not break the test.
    let expected = format!("requires_buz: \">={}\"", env!("CARGO_PKG_VERSION"));
    assert!(
        spec.contains(&expected),
        "scaffolded spec must gate on the current contract: {spec}"
    );
    assert!(spec.contains("$INPUT_EXAMPLE"));
    // The scaffold must clear the same gate the Manager applies.
    assert_eq!(
        validate::check(&[dir.path().join("plugins/alice/hello")]).unwrap(),
        0
    );
}

#[test]
fn never_overwrites_an_existing_spec() {
    let dir = tempfile::tempdir().unwrap();
    run(
        "alice/hello",
        dir.path(),
        "community",
        Some("alice"),
        Some("https://github.com/alice/buz-plugins"),
        "shell",
        "0.1.0",
        None,
    )
    .unwrap();
    let error = run(
        "alice/hello",
        dir.path(),
        "community",
        Some("alice"),
        Some("https://github.com/alice/buz-plugins"),
        "shell",
        "0.1.0",
        None,
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("init never overwrites"), "{error}");
}

#[test]
fn rejects_invalid_ids() {
    let dir = tempfile::tempdir().unwrap();
    let error = run(
        "hello",
        dir.path(),
        "community",
        None,
        None,
        "shell",
        "0.1.0",
        None,
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("'<namespace>/<name>'"), "{error}");
}
