//! Cross-layer input contract: the schema layer (save/scan) and the trigger
//! layer must agree on what is a declaration error versus a value error.
//!
//! A declaration typo (`type: interger`, `choice` without options, an invalid
//! `pattern` regex) used to pass `parse_and_validate` and then fail every
//! trigger with "unknown input type". These cases pin the split:
//!
//! - declaration errors → `parse_and_validate` invalid (save time);
//! - value errors → `validate_input_values` rejects (trigger/claim time),
//!   with the declaration itself valid.

use std::collections::HashMap;

use shared::pipeline_yaml::parse_and_validate;
use shared::protocol::{validate_input_values, PipelineInputDef};

struct Case {
    label: &'static str,
    /// YAML lines for the single `target` input (already indented).
    def_yaml: &'static str,
    def: PipelineInputDef,
    /// Value passed at trigger time; `None` = omitted.
    value: Option<&'static str>,
    /// Whether the declaration itself must validate.
    declaration_ok: bool,
    /// Whether the supplied value must be accepted (only checked when the
    /// declaration is valid).
    value_ok: bool,
    /// Host registry answer for `type: host`.
    host_registered: bool,
}

fn input(
    input_type: Option<&str>,
    required: Option<bool>,
    pattern: Option<&str>,
    options: Option<Vec<&str>>,
) -> PipelineInputDef {
    PipelineInputDef {
        name: "target".into(),
        description: None,
        required,
        default: None,
        masking: None,
        input_type: input_type.map(str::to_string),
        pattern: pattern.map(str::to_string),
        options: options.map(|list| list.into_iter().map(str::to_string).collect()),
    }
}

fn pipeline_yaml(def_yaml: &str) -> String {
    format!(
        "name: layer-test\nenvironments: [default]\non:\n  manual:\n    inputs:\n      target:\n{def_yaml}\nsteps:\n  - name: run\n    run: echo hi\n"
    )
}

fn cases() -> Vec<Case> {
    vec![
        Case {
            label: "string accepts any value",
            def_yaml: "        type: string",
            def: input(Some("string"), None, None, None),
            value: Some("anything"),
            declaration_ok: true,
            value_ok: true,
            host_registered: false,
        },
        Case {
            label: "number accepts an integer",
            def_yaml: "        type: number",
            def: input(Some("number"), None, None, None),
            value: Some("42"),
            declaration_ok: true,
            value_ok: true,
            host_registered: false,
        },
        Case {
            label: "number rejects non-numeric",
            def_yaml: "        type: number",
            def: input(Some("number"), None, None, None),
            value: Some("4x"),
            declaration_ok: true,
            value_ok: false,
            host_registered: false,
        },
        Case {
            label: "boolean accepts true",
            def_yaml: "        type: boolean",
            def: input(Some("boolean"), None, None, None),
            value: Some("TRUE"),
            declaration_ok: true,
            value_ok: true,
            host_registered: false,
        },
        Case {
            label: "boolean rejects yes",
            def_yaml: "        type: boolean",
            def: input(Some("boolean"), None, None, None),
            value: Some("yes"),
            declaration_ok: true,
            value_ok: false,
            host_registered: false,
        },
        Case {
            label: "host accepts a registered host",
            def_yaml: "        type: host",
            def: input(Some("host"), None, None, None),
            value: Some("web-1"),
            declaration_ok: true,
            value_ok: true,
            host_registered: true,
        },
        Case {
            label: "host rejects an unregistered host",
            def_yaml: "        type: host",
            def: input(Some("host"), None, None, None),
            value: Some("ghost"),
            declaration_ok: true,
            value_ok: false,
            host_registered: false,
        },
        Case {
            label: "choice accepts a declared option",
            def_yaml: "        type: choice\n        options: [staging, prod]",
            def: input(Some("choice"), None, None, Some(vec!["staging", "prod"])),
            value: Some("prod"),
            declaration_ok: true,
            value_ok: true,
            host_registered: false,
        },
        Case {
            label: "choice rejects an undeclared option",
            def_yaml: "        type: choice\n        options: [staging, prod]",
            def: input(Some("choice"), None, None, Some(vec!["staging", "prod"])),
            value: Some("dev"),
            declaration_ok: true,
            value_ok: false,
            host_registered: false,
        },
        Case {
            label: "choice without options is a declaration error",
            def_yaml: "        type: choice",
            def: input(Some("choice"), None, None, None),
            value: Some("anything"),
            declaration_ok: false,
            value_ok: false,
            host_registered: false,
        },
        Case {
            label: "unknown type is a declaration error, not a value error",
            def_yaml: "        type: interger",
            def: input(Some("interger"), None, None, None),
            value: Some("1"),
            declaration_ok: false,
            value_ok: false,
            host_registered: false,
        },
        Case {
            label: "pattern accepts a matching value",
            def_yaml: "        type: string\n        pattern: '^[a-z]+$'",
            def: input(Some("string"), None, Some("^[a-z]+$"), None),
            value: Some("abc"),
            declaration_ok: true,
            value_ok: true,
            host_registered: false,
        },
        Case {
            label: "pattern rejects a non-matching value",
            def_yaml: "        type: string\n        pattern: '^[a-z]+$'",
            def: input(Some("string"), None, Some("^[a-z]+$"), None),
            value: Some("ABC"),
            declaration_ok: true,
            value_ok: false,
            host_registered: false,
        },
        Case {
            label: "invalid pattern regex is a declaration error",
            def_yaml: "        type: string\n        pattern: '([a-z'",
            def: input(Some("string"), None, Some("([a-z"), None),
            value: Some("abc"),
            declaration_ok: false,
            value_ok: false,
            host_registered: false,
        },
        Case {
            label: "required omitted is a value error",
            def_yaml: "        type: string\n        required: true",
            def: input(Some("string"), Some(true), None, None),
            value: None,
            declaration_ok: true,
            value_ok: false,
            host_registered: false,
        },
        Case {
            label: "list accepts one item per line",
            def_yaml: "        type: list",
            def: input(Some("list"), None, None, None),
            value: Some("a\nb"),
            declaration_ok: true,
            value_ok: true,
            host_registered: false,
        },
        Case {
            label: "list rejects an empty middle item",
            def_yaml: "        type: list",
            def: input(Some("list"), None, None, None),
            value: Some("a\n\nb"),
            declaration_ok: true,
            value_ok: false,
            host_registered: false,
        },
        Case {
            label: "options without choice is a warning, not an error",
            def_yaml: "        type: string\n        options: [a, b]",
            def: input(Some("string"), None, None, Some(vec!["a", "b"])),
            value: Some("anything"),
            declaration_ok: true,
            value_ok: true,
            host_registered: false,
        },
    ]
}

#[test]
fn schema_and_trigger_layers_agree_on_declaration_vs_value_errors() {
    for case in cases() {
        let result = parse_and_validate(&pipeline_yaml(case.def_yaml));
        assert_eq!(
            result.valid, case.declaration_ok,
            "{}: schema validity (errors: {:?})",
            case.label, result.errors
        );

        if !case.declaration_ok {
            // A rejected declaration never reaches the value check.
            continue;
        }

        let mut inputs = HashMap::new();
        if let Some(value) = case.value {
            inputs.insert("target".to_string(), value.to_string());
        }
        let outcome = validate_input_values(std::slice::from_ref(&case.def), &inputs, |host| {
            case.host_registered && host == "web-1"
        });
        assert_eq!(
            outcome.is_ok(),
            case.value_ok,
            "{}: trigger-layer verdict ({outcome:?})",
            case.label
        );
    }
}
