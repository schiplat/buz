//! Unit tests for `shared::value_source`.
//! Included via `value_source.rs` under `#[cfg(test)]`.

use super::{expand, ExpandMode, MissingPolicy, RenderError, ValueMap};

fn map() -> ValueMap {
    let mut m = ValueMap::new();
    m.secrets
        .insert("AFDIAN_API_TOKEN".into(), "tok_123".into());
    m.secrets.insert("TURNSTILE_SECRET".into(), "ts_abc".into());
    m.secrets.insert("EMPTY_SECRET".into(), "".into());
    m.vars
        .insert("REGISTRY".into(), "registry.cn-hk.example.com".into());
    m.vars.insert("NAMESPACE".into(), "dadou".into());
    m.vars.insert("IMAGE_NAME".into(), "super-manager".into());
    m.vars.insert("INPUT_TAG".into(), "1.4.0".into());
    m
}

fn strict(t: &str, m: &ValueMap) -> Result<String, RenderError> {
    expand(t, m, ExpandMode::Strict, MissingPolicy::Fail).map(|e| e.content)
}

// ── Strict, all resolvable ────────────────────────────────────────────────

#[test]
fn strict_renders_secrets_and_vars() {
    let m = map();
    let out = strict(
        "MANAGER_AFDIAN_API_TOKEN=${secrets.AFDIAN_API_TOKEN}\n\
         MANAGER_IMAGE=${vars.REGISTRY}/${vars.NAMESPACE}/${vars.IMAGE_NAME}:${vars.INPUT_TAG}\n\
         MANAGER_PATH_PREFIX=pi\n",
        &m,
    )
    .unwrap();
    assert_eq!(
        out,
        "MANAGER_AFDIAN_API_TOKEN=tok_123\n\
         MANAGER_IMAGE=registry.cn-hk.example.com/dadou/super-manager:1.4.0\n\
         MANAGER_PATH_PREFIX=pi\n"
    );
}

#[test]
fn strict_keeps_comments_and_blanks() {
    let m = map();
    let out = strict("# header\n\nKEY=${vars.INPUT_TAG} # trailing\n", &m).unwrap();
    assert_eq!(out, "# header\n\nKEY=1.4.0 # trailing\n");
}

#[test]
fn strict_default_fallback() {
    let m = map();
    let out = strict(
        "A=${vars.MISSING:-fallback}\nB=${secrets.TURNSTILE_SECRET:-nope}\n",
        &m,
    )
    .unwrap();
    assert_eq!(out, "A=fallback\nB=ts_abc\n");
}

#[test]
fn strict_empty_secret_is_missing() {
    let m = map();
    let out = strict("E=${secrets.EMPTY_SECRET:-df}\n", &m).unwrap();
    assert_eq!(out, "E=df\n");
}

// ── Strict, fail-fast ─────────────────────────────────────────────────────

#[test]
fn strict_missing_secret_fails() {
    let m = map();
    let err = strict("A=${secrets.UNKNOWN_SECRET}\n", &m).unwrap_err();
    assert!(matches!(err, RenderError::Missing { .. }));
}

#[test]
fn strict_missing_var_fails() {
    let m = map();
    let err = strict("A=${vars.NOPE}\n", &m).unwrap_err();
    assert!(matches!(err, RenderError::Missing { .. }));
}

#[test]
fn strict_hard_required_overrides_default() {
    let m = map();
    // `:?` wins even with a default present in the reference.
    let err = strict("A=${secrets.UNKNOWN:?}\n", &m).unwrap_err();
    assert!(matches!(err, RenderError::Missing { .. }));
}

#[test]
fn strict_rejects_bare_reference() {
    let m = map();
    let err = strict("A=$INPUT_TAG\n", &m).unwrap_err();
    assert!(matches!(err, RenderError::InvalidReference { .. }));
    let err = strict("A=${INPUT_TAG}\n", &m).unwrap_err();
    assert!(matches!(err, RenderError::Missing { .. }));
}

#[test]
fn strict_rejects_invalid_key() {
    let m = map();
    let err = strict("1BAD_KEY=x\n", &m).unwrap_err();
    assert!(matches!(err, RenderError::InvalidKey { .. }));
}

#[test]
fn strict_rejects_duplicate_key() {
    let m = map();
    let err = strict("A=1\nA=2\n", &m).unwrap_err();
    assert!(matches!(err, RenderError::DuplicateKey { .. }));
}

#[test]
fn strict_detects_residual_after_value() {
    let mut m = map();
    m.secrets.insert("EVIL".into(), "left${HOOK}".into());
    let err = strict("A=${secrets.EVIL}\n", &m).unwrap_err();
    assert!(matches!(err, RenderError::ComposeInterpolate { .. }));
}

#[test]
fn strict_rejects_multiline_secret_value() {
    let mut m = map();
    m.secrets.insert("KEY".into(), "line1\nline2".into());
    let err = strict("A=${secrets.KEY}\n", &m).unwrap_err();
    assert!(matches!(err, RenderError::MultilineValue { .. }));
    assert!(err.to_string().contains("multiline"));
}

#[test]
fn strict_rejects_multiline_vars_value() {
    let mut m = map();
    m.vars.insert("v".into(), "line1\r\nline2".into());
    let err = strict("A=${vars.v}\n", &m).unwrap_err();
    assert!(matches!(err, RenderError::MultilineValue { .. }));
}

#[test]
fn lenient_allows_multiline_value() {
    // Legacy config_path behavior is preserved: no newline rejection.
    let mut m = map();
    m.vars.insert("v".into(), "line1\nline2".into());
    let out = expand("A=${vars.v}", &m, ExpandMode::Lenient, MissingPolicy::Blank).unwrap();
    assert_eq!(out.content, "A=line1\nline2");
}

#[test]
fn strict_keeps_bare_dollar() {
    let mut m = map();
    m.secrets.insert("PW".into(), "pa$$word".into());
    let out = strict("A=${secrets.PW}\n", &m).unwrap();
    assert_eq!(out, "A=pa$$word\n");
}

// ── Warn policy ───────────────────────────────────────────────────────────

#[test]
fn warn_writes_key_empty_with_comment() {
    let m = map();
    let e = expand(
        "A=${secrets.TURNSTILE_SECRET}\nB=${secrets.GONE}\n",
        &m,
        ExpandMode::Strict,
        MissingPolicy::Warn,
    )
    .unwrap();
    assert!(e.content.contains("A=ts_abc"));
    assert!(e.content.contains("# WARNING: ${secrets.GONE} missing"));
    assert!(e.content.contains("B=\n"));
    assert_eq!(e.missing, vec!["${secrets.GONE}".to_string()]);
}

#[test]
fn warn_does_not_fail() {
    let m = map();
    let e = expand(
        "A=${vars.NOPE}\n",
        &m,
        ExpandMode::Strict,
        MissingPolicy::Warn,
    )
    .unwrap();
    assert_eq!(e.missing, vec!["${vars.NOPE}".to_string()]);
}

// ── Lenient (legacy config_path) ──────────────────────────────────────────

#[test]
fn lenient_expands_bare_refs_and_blank_on_missing() {
    let mut m = map();
    let out = expand(
        "config_tpl/super-manager/$tag",
        &m,
        ExpandMode::Lenient,
        MissingPolicy::Blank,
    )
    .unwrap();
    assert_eq!(out.content, "config_tpl/super-manager/");
    m.vars.insert("tag".into(), "1.4.0".into());
    let out = expand(
        "config_tpl/super-manager/$tag",
        &m,
        ExpandMode::Lenient,
        MissingPolicy::Blank,
    )
    .unwrap();
    assert_eq!(out.content, "config_tpl/super-manager/1.4.0");
}

#[test]
fn lenient_keeps_unknown_dollar() {
    let m = map();
    // `$4` / `$b` look like references (matching the legacy expand_var_refs
    // behavior) and expand to empty when absent.
    let out = expand(
        "keep $4 and a$b",
        &m,
        ExpandMode::Lenient,
        MissingPolicy::Blank,
    )
    .unwrap();
    assert_eq!(out.content, "keep  and a");
}

#[test]
fn lenient_allows_bare_key_lines() {
    let m = map();
    // Not a KEY=VALUE line → passes through.
    let out = expand(
        "some free text",
        &m,
        ExpandMode::Lenient,
        MissingPolicy::Blank,
    )
    .unwrap();
    assert_eq!(out.content, "some free text");
}
