//! Integration tests for declarative manual-input validation
//! (`on.manual.inputs.type/pattern/options` + trigger-time `validate_input_values`).

use std::collections::HashMap;

use shared::protocol::{validate_input_values, PipelineInputDef};

fn def(name: &str, required: bool) -> PipelineInputDef {
    PipelineInputDef {
        name: name.to_string(),
        description: None,
        required: Some(required),
        default: None,
        masking: None,
        input_type: None,
        pattern: None,
        options: None,
    }
}

#[test]
fn required_missing_is_rejected() {
    let defs = vec![def("version", true), def("note", false)];
    let inputs = HashMap::from([("note".to_string(), "x".to_string())]);
    let err = validate_input_values(&defs, &inputs, |_| false).unwrap_err();
    assert_eq!(err.len(), 1);
    assert_eq!(err[0].name, "version");
    assert!(err[0].message.contains("required"));
}

#[test]
fn optional_missing_is_allowed() {
    let defs = vec![def("note", false)];
    let inputs = HashMap::new();
    assert!(validate_input_values(&defs, &inputs, |_| false).is_ok());
}

#[test]
fn number_type_accepts_and_rejects() {
    let mut d = def("replicas", true);
    d.input_type = Some("number".into());
    let defs = vec![d];

    let ok = HashMap::from([("replicas".to_string(), "3".to_string())]);
    assert!(validate_input_values(&defs, &ok, |_| false).is_ok());

    let bad = HashMap::from([("replicas".to_string(), "three".to_string())]);
    let err = validate_input_values(&defs, &bad, |_| false).unwrap_err();
    assert_eq!(err[0].message, "must be an integer");
}

#[test]
fn boolean_type_accepts_and_rejects() {
    let mut d = def("dry_run", false);
    d.input_type = Some("boolean".into());
    let defs = vec![d];

    for v in ["true", "TRUE", "false", "False"] {
        let inputs = HashMap::from([("dry_run".to_string(), v.to_string())]);
        assert!(
            validate_input_values(&defs, &inputs, |_| false).is_ok(),
            "should accept {v}"
        );
    }

    let bad = HashMap::from([("dry_run".to_string(), "yes".to_string())]);
    let err = validate_input_values(&defs, &bad, |_| false).unwrap_err();
    assert_eq!(err[0].message, "must be true or false");
}

#[test]
fn host_type_checks_registry() {
    let mut d = def("deploy_host", true);
    d.input_type = Some("host".into());
    let defs = vec![d];

    let known = HashMap::from([("deploy_host".to_string(), "prod-01".to_string())]);
    assert!(validate_input_values(&defs, &known, |h| h == "prod-01").is_ok());

    let unknown = HashMap::from([("deploy_host".to_string(), "ghost".to_string())]);
    let err = validate_input_values(&defs, &unknown, |h| h == "prod-01").unwrap_err();
    assert!(err[0].message.contains("not registered"));
}

#[test]
fn choice_requires_declared_options() {
    let mut d = def("env", true);
    d.input_type = Some("choice".into());
    d.options = Some(vec!["staging".into(), "production".into()]);
    let defs = vec![d];

    let ok = HashMap::from([("env".to_string(), "staging".to_string())]);
    assert!(validate_input_values(&defs, &ok, |_| false).is_ok());

    let bad = HashMap::from([("env".to_string(), "sandbox".to_string())]);
    let err = validate_input_values(&defs, &bad, |_| false).unwrap_err();
    assert!(err[0].message.contains("must be one of"));
}

#[test]
fn choice_without_options_is_a_definition_error() {
    let mut d = def("env", true);
    d.input_type = Some("choice".into());
    let defs = vec![d];

    let inputs = HashMap::from([("env".to_string(), "staging".to_string())]);
    let err = validate_input_values(&defs, &inputs, |_| false).unwrap_err();
    assert!(err[0].message.contains("options"));
}

#[test]
fn pattern_is_enforced_and_bad_pattern_is_definition_error() {
    let mut d = def("tag", true);
    d.pattern = Some("^[A-Za-z0-9._-]+$".into());
    let defs = vec![d];

    let ok = HashMap::from([("tag".to_string(), "0.1.0-rc.2".to_string())]);
    assert!(validate_input_values(&defs, &ok, |_| false).is_ok());

    let bad = HashMap::from([("tag".to_string(), "v1..$".to_string())]);
    let err = validate_input_values(&defs, &bad, |_| false).unwrap_err();
    assert!(err[0].message.contains("does not match"));

    let mut d2 = def("tag2", true);
    d2.pattern = Some("(".into()); // unbalanced → regex compile failure
    let err2 = validate_input_values(
        &[d2],
        &HashMap::from([("tag2".to_string(), "x".to_string())]),
        |_| false,
    )
    .unwrap_err();
    assert!(err2[0].message.contains("invalid pattern"));
}

#[test]
fn unknown_type_is_reported() {
    let mut d = def("weird", false);
    d.input_type = Some("date".into());
    let defs = vec![d];

    let inputs = HashMap::from([("weird".to_string(), "2026-08-24".to_string())]);
    let err = validate_input_values(&defs, &inputs, |_| false).unwrap_err();
    assert!(err[0].message.contains("unknown input type"));
}

#[test]
fn multiple_fields_aggregate_errors() {
    let mut tag = def("tag", true);
    tag.pattern = Some("^[0-9]+$".into());
    let mut env = def("env", true);
    env.input_type = Some("choice".into());
    env.options = Some(vec!["staging".into()]);
    let defs = vec![tag, env];

    let inputs = HashMap::from([
        ("tag".to_string(), "abc".to_string()),
        ("env".to_string(), "production".to_string()),
    ]);
    let err = validate_input_values(&defs, &inputs, |_| false).unwrap_err();
    assert_eq!(err.len(), 2);
    assert!(err.iter().any(|e| e.name == "tag"));
    assert!(err.iter().any(|e| e.name == "env"));
}

#[test]
fn number_with_pattern_applies_both() {
    let mut d = def("port", true);
    d.input_type = Some("number".into());
    d.pattern = Some("^[0-9]{1,5}$".into());
    let defs = vec![d];

    let ok = HashMap::from([("port".to_string(), "8080".to_string())]);
    assert!(validate_input_values(&defs, &ok, |_| false).is_ok());

    let too_long = HashMap::from([("port".to_string(), "123456".to_string())]);
    let err = validate_input_values(&defs, &too_long, |_| false).unwrap_err();
    assert!(err[0].message.contains("does not match"));
}
