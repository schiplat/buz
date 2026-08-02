//! config_path templating tests — now covered by the unified
//! `shared::value_source` expander (Lenient mode keeps the legacy
//! `expand_var_refs` semantics: unresolved → empty string).

use serde_json::json;
use shared::value_source::{expand, ExpandMode, MissingPolicy, ValueMap};

fn value_map(vars: &serde_json::Value) -> ValueMap {
    ValueMap {
        vars: vars
            .as_object()
            .map(|o| {
                o.iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                    .collect()
            })
            .unwrap_or_default(),
        secrets: Default::default(),
    }
}

#[test]
fn expands_bare_and_braced_refs() {
    let vars = json!({
        "tag": "1.4.0",
        "project": "super-manager",
    });
    let m = value_map(&vars);
    assert_eq!(
        expand(
            "config_tpl/super-manager/$tag",
            &m,
            ExpandMode::Lenient,
            MissingPolicy::Blank
        )
        .unwrap()
        .content,
        "config_tpl/super-manager/1.4.0"
    );
    assert_eq!(
        expand(
            "config_tpl/$project/$tag",
            &m,
            ExpandMode::Lenient,
            MissingPolicy::Blank
        )
        .unwrap()
        .content,
        "config_tpl/super-manager/1.4.0"
    );
    assert_eq!(
        expand(
            "config_tpl/${project}/v${tag}",
            &m,
            ExpandMode::Lenient,
            MissingPolicy::Blank
        )
        .unwrap()
        .content,
        "config_tpl/super-manager/v1.4.0"
    );
}

#[test]
fn missing_refs_expand_to_empty() {
    let vars = json!({ "tag": "1.4.0" });
    let m = value_map(&vars);
    assert_eq!(
        expand(
            "cfg/$unknown/$tag",
            &m,
            ExpandMode::Lenient,
            MissingPolicy::Blank
        )
        .unwrap()
        .content,
        "cfg//1.4.0"
    );
    assert_eq!(
        expand(
            "cfg/${tag}/v${other}",
            &m,
            ExpandMode::Lenient,
            MissingPolicy::Blank
        )
        .unwrap()
        .content,
        "cfg/1.4.0/v"
    );
}

#[test]
fn keeps_literal_plain_strings() {
    let vars = json!({});
    let m = value_map(&vars);
    assert_eq!(
        expand(
            "config_tpl/super-manager/v1.4.0",
            &m,
            ExpandMode::Lenient,
            MissingPolicy::Blank
        )
        .unwrap()
        .content,
        "config_tpl/super-manager/v1.4.0"
    );
    // Trailing/embedded `$` that is not a valid ref is kept as-is.
    assert_eq!(
        expand("a$", &m, ExpandMode::Lenient, MissingPolicy::Blank)
            .unwrap()
            .content,
        "a$"
    );
    assert_eq!(
        expand("$", &m, ExpandMode::Lenient, MissingPolicy::Blank)
            .unwrap()
            .content,
        "$"
    );
    // `$5` looks like a valid ref name but is absent from the map → "".
    assert_eq!(
        expand("cost$5", &m, ExpandMode::Lenient, MissingPolicy::Blank)
            .unwrap()
            .content,
        "cost"
    );
}

#[test]
fn uses_string_values_only() {
    let vars = json!({ "n": 42, "flag": true, "tag": "1.4.0" });
    let m = value_map(&vars);
    // Non-string values are dropped from the map → empty string.
    assert_eq!(
        expand(
            "x/$n/$flag/$tag",
            &m,
            ExpandMode::Lenient,
            MissingPolicy::Blank
        )
        .unwrap()
        .content,
        "x///1.4.0"
    );
}

#[test]
fn expansion_is_not_recursive() {
    let vars = json!({ "a": "$b", "b": "real" });
    let m = value_map(&vars);
    // Single pass: $a → "$b", then $b is NOT re-expanded.
    assert_eq!(
        expand("$a", &m, ExpandMode::Lenient, MissingPolicy::Blank)
            .unwrap()
            .content,
        "$b"
    );
}
