use runner::client::env_render::*;

#[test]
fn parse_basic_key_value() {
    let entries = parse_env_file("A=1\nB=hello world\nC=\n");
    assert_eq!(
        entries,
        vec![
            ("A".to_string(), "1".to_string()),
            ("B".to_string(), "hello world".to_string()),
            ("C".to_string(), String::new()),
        ]
    );
}

#[test]
fn parse_skips_comments_and_blanks() {
    let entries = parse_env_file("# comment\n\n  # indented comment\nA=1\n   \nB=\"quoted\"\n");
    assert_eq!(
        entries,
        vec![
            ("A".to_string(), "1".to_string()),
            ("B".to_string(), "quoted".to_string()),
        ]
    );
}

#[test]
fn parse_quoted_values() {
    let entries = parse_env_file("A=\"has # hash\"\nB='single'\nC=\"escaped \\\"quote\\\"\"\n");
    assert_eq!(
        entries,
        vec![
            ("A".to_string(), "has # hash".to_string()),
            ("B".to_string(), "single".to_string()),
            ("C".to_string(), "escaped \"quote\"".to_string()),
        ]
    );
}

#[test]
fn parse_ignores_invalid_lines() {
    let entries = parse_env_file("1BAD=value\nA=1\n=novalue\nB=2\n");
    assert_eq!(
        entries,
        vec![
            ("A".to_string(), "1".to_string()),
            ("B".to_string(), "2".to_string()),
        ]
    );
}

#[test]
fn quote_plain_value_unquoted() {
    assert_eq!(quote_env_value("hello"), "hello");
    assert_eq!(quote_env_value("a_b-1.2"), "a_b-1.2");
}

#[test]
fn quote_special_values() {
    assert_eq!(quote_env_value("has # hash"), "\"has # hash\"");
    assert_eq!(quote_env_value("a=b"), "\"a=b\"");
    assert_eq!(quote_env_value(" leading"), "\" leading\"");
    assert_eq!(quote_env_value("trailing "), "\"trailing \"");
    assert_eq!(quote_env_value("say \"hi\""), "\"say \\\"hi\\\"\"");
    assert_eq!(quote_env_value(""), "\"\"");
    assert_eq!(quote_env_value("back\\slash"), "\"back\\\\slash\"");
}

#[test]
fn quote_round_trips() {
    for value in [
        "plain",
        "",
        "has # hash",
        "a=b",
        " leading",
        "say \"hi\"",
        "back\\slash",
        "mixed \" # = x",
    ] {
        let quoted = quote_env_value(value);
        let parsed = parse_env_file(&format!("K={quoted}\n"));
        assert_eq!(
            parsed,
            vec![("K".to_string(), value.to_string())],
            "round-trip failed for {value:?} (quoted: {quoted:?})"
        );
    }
}

#[test]
fn diff_detects_changes() {
    let changed = diff_env("A=1\nB=2\n", "A=1\nB=3\n");
    assert_eq!(changed, vec!["B".to_string()]);
}

#[test]
fn diff_detects_added_and_removed() {
    // Only keys in `new` are reported.
    let changed = diff_env("A=1\n", "A=1\nC=3\n");
    assert_eq!(changed, vec!["C".to_string()]);
    let removed = diff_env("A=1\nB=2\n", "A=1\n");
    assert!(removed.is_empty());
}

#[test]
fn merge_updates_template_keys() {
    let (merged, changed) = merge_env("A=old\nB=2\n", "A=new\nC=3\n");
    let entries = parse_env_file(&merged);
    assert_eq!(
        entries,
        vec![
            ("A".to_string(), "new".to_string()),
            ("C".to_string(), "3".to_string()),
            ("B".to_string(), "2".to_string()),
        ]
    );
    assert_eq!(changed, vec!["A".to_string(), "C".to_string()]);
}

#[test]
fn merge_quotes_special_values_from_template_and_legacy() {
    let (merged, _) = merge_env("OLD=has # hash\n", "A=1\n");
    let entries = parse_env_file(&merged);
    assert_eq!(
        entries,
        vec![
            ("A".to_string(), "1".to_string()),
            ("OLD".to_string(), "has # hash".to_string()),
        ]
    );
    assert!(merged.contains("OLD=\"has # hash\""));
}

#[test]
fn merge_preserves_comments_and_warning_lines() {
    let rendered = "# WARNING: ${secrets.TURNSTILE_SECRET} missing at 2026-01-01\n\
                    A=\n\
                    # a normal template comment\n\
                    B=2\n";
    let (merged, _) = merge_env("", rendered);
    assert!(
        merged.contains("# WARNING: ${secrets.TURNSTILE_SECRET} missing at 2026-01-01\n"),
        "warning comment preserved: {merged:?}"
    );
    assert!(merged.contains("# a normal template comment\n"));
    let entries = parse_env_file(&merged);
    assert_eq!(
        entries,
        vec![
            ("A".to_string(), String::new()),
            ("B".to_string(), "2".to_string()),
        ]
    );
}

#[test]
fn round_trip_ok_for_clean_content() {
    assert!(round_trip_check("A=1\nB=\"quoted value\"\nC=\n").is_ok());
}

#[test]
fn round_trip_rejects_duplicate_keys() {
    let err = round_trip_check("A=1\nA=2\n").unwrap_err();
    assert!(err.contains("duplicate key 'A'"), "got: {err}");
}

#[test]
fn round_trip_accepts_quoted_hash_values() {
    // `#` mid-value is preserved by our parser and re-serialized quoted, so a
    // fully merged content (which always quotes such values) passes the guard.
    assert!(round_trip_check("A=\"has # hash\"\n").is_ok());
}

#[test]
fn merge_preserves_legacy_unchanged_keys() {
    let (merged, changed) = merge_env("KEEP=yes\nTOUCHED=old\n", "TOUCHED=new\n");
    let entries = parse_env_file(&merged);
    assert_eq!(
        entries,
        vec![
            ("TOUCHED".to_string(), "new".to_string()),
            ("KEEP".to_string(), "yes".to_string()),
        ]
    );
    assert_eq!(changed, vec!["TOUCHED".to_string()]);
}

#[test]
fn parse_env_lines_preserves_structure() {
    let lines = parse_env_lines("# hi\n\nA=1\n");
    assert_eq!(
        lines,
        vec![
            EnvLine::Comment("# hi".to_string()),
            EnvLine::Blank,
            EnvLine::KeyValue {
                key: "A".to_string(),
                value: "1".to_string(),
            },
        ]
    );
}
