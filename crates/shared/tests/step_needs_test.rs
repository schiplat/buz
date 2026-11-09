//! `needs:` DAG declaration validation (save-time layer).
//!
//! Targets must exist, step names must be unambiguous when `needs` is used,
//! and the declared graph must be acyclic. Scheduling itself is Runner-side.

use shared::pipeline_yaml::parse_and_validate;

fn validate(steps_yaml: &str) -> shared::pipeline_yaml::YamlValidationResult {
    let yaml = format!("name: needs-test\nenvironments: [default]\nsteps:\n{steps_yaml}");
    parse_and_validate(&yaml)
}

const THREE: &str = "  - name: build\n    run: cargo build\n  - name: test\n    needs: [build]\n    run: cargo test\n  - name: deploy\n    needs: [test]\n    run: ./deploy\n";

#[test]
fn valid_chain_and_diamond_validate() {
    let result = validate(THREE);
    assert!(result.valid, "{:?}", result.errors);

    // Diamond: deploy waits for both; each test branch waits for build.
    let diamond = "  - name: build\n    run: cargo build\n  - name: lint\n    needs: [build]\n    run: cargo lint\n  - name: test\n    needs: [build]\n    run: cargo test\n  - name: deploy\n    needs: [lint, test]\n    run: ./deploy\n";
    let result = validate(diamond);
    assert!(result.valid, "{:?}", result.errors);
}

#[test]
fn unknown_target_is_rejected() {
    let yaml = "  - name: build\n    run: cargo build\n  - name: test\n    needs: [bulid]\n    run: cargo test\n";
    let result = validate(yaml);
    assert!(!result.valid);
    assert!(
        result
            .errors
            .iter()
            .any(|e| e.field == "steps[1].needs" && e.message.contains("bulid")),
        "{:?}",
        result.errors
    );
}

#[test]
fn self_reference_is_rejected() {
    let yaml = "  - name: build\n    needs: [build]\n    run: cargo build\n";
    let result = validate(yaml);
    assert!(!result.valid);
    assert!(
        result
            .errors
            .iter()
            .any(|e| e.message.contains("cannot need itself")),
        "{:?}",
        result.errors
    );
}

#[test]
fn cycle_is_reported_with_the_loop() {
    let yaml = "  - name: a\n    needs: [c]\n    run: echo a\n  - name: b\n    needs: [a]\n    run: echo b\n  - name: c\n    needs: [b]\n    run: echo c\n";
    let result = validate(yaml);
    assert!(!result.valid);
    assert!(
        result.errors.iter().any(|e| e.message.contains("cycle")),
        "{:?}",
        result.errors
    );
}

#[test]
fn duplicate_names_are_rejected_only_when_needs_is_used() {
    let duplicated = "  - name: same\n    run: echo one\n  - name: same\n    run: echo two\n";
    assert!(
        validate(duplicated).valid,
        "no needs → duplicates tolerated"
    );

    let referenced = "  - name: same\n    run: echo one\n  - name: same\n    run: echo two\n  - name: third\n    needs: [same]\n    run: echo three\n";
    let result = validate(referenced);
    assert!(!result.valid);
    assert!(
        result.errors.iter().any(|e| e.message.contains("unique")),
        "{:?}",
        result.errors
    );
}

#[test]
fn empty_needs_entry_is_rejected() {
    let yaml = "  - name: build\n    run: cargo build\n  - name: test\n    needs: [\" \"]\n    run: cargo test\n";
    let result = validate(yaml);
    assert!(!result.valid);
    assert!(
        result
            .errors
            .iter()
            .any(|e| e.message.contains("must not be empty")),
        "{:?}",
        result.errors
    );
}

#[test]
fn step_data_carries_needs_for_the_frontend_import() {
    let result = validate(THREE);
    let parsed = result.parsed.expect("valid result carries parsed data");
    let test_step = parsed
        .steps
        .iter()
        .find(|step| step.name.as_deref() == Some("test"))
        .expect("test step");
    assert_eq!(test_step.needs.as_deref(), Some(&["build".to_string()][..]));
}
