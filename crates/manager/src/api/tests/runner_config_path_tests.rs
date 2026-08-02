use super::expand_var_refs;
use serde_json::json;

#[test]
fn expands_bare_and_braced_refs() {
    let vars = json!({
        "tag": "1.4.0",
        "project": "super-manager",
    });
    assert_eq!(
        expand_var_refs("config_tpl/super-manager/$tag", &vars),
        "config_tpl/super-manager/1.4.0"
    );
    assert_eq!(
        expand_var_refs("config_tpl/$project/$tag", &vars),
        "config_tpl/super-manager/1.4.0"
    );
    assert_eq!(
        expand_var_refs("config_tpl/${project}/v${tag}", &vars),
        "config_tpl/super-manager/v1.4.0"
    );
}

#[test]
fn missing_refs_expand_to_empty() {
    let vars = json!({ "tag": "1.4.0" });
    assert_eq!(expand_var_refs("cfg/$unknown/$tag", &vars), "cfg//1.4.0");
    assert_eq!(expand_var_refs("cfg/${tag}/v${other}", &vars), "cfg/1.4.0/v");
}

#[test]
fn keeps_literal_plain_strings() {
    let vars = json!({});
    assert_eq!(
        expand_var_refs("config_tpl/super-manager/v1.4.0", &vars),
        "config_tpl/super-manager/v1.4.0"
    );
    // Trailing/embedded `$` that is not a valid ref is kept as-is.
    assert_eq!(expand_var_refs("a$", &vars), "a$");
    assert_eq!(expand_var_refs("$", &vars), "$");
    // `$5` looks like a valid ref name but is absent from the map → "".
    assert_eq!(expand_var_refs("cost$5", &vars), "cost");
}

#[test]
fn uses_string_values_only() {
    let vars = json!({ "n": 42, "flag": true, "tag": "1.4.0" });
    // Non-string values fall back to empty string.
    assert_eq!(expand_var_refs("x/$n/$flag/$tag", &vars), "x///1.4.0");
}

#[test]
fn expansion_is_not_recursive() {
    let vars = json!({ "a": "$b", "b": "real" });
    // Single pass: $a → "$b", then $b is NOT re-expanded because only the
    // lookup map is consulted, not the expanded output.
    assert_eq!(expand_var_refs("$a", &vars), "$b");
}
