use shared::secret_ref::{
    collect_secrets_names, parse_secrets_expr, parse_vars_expr, resolve_secret_slot,
    run_contains_secrets_expr,
};

#[test]
fn parse_secrets_expr_ok() {
    assert_eq!(parse_secrets_expr("${secrets.GIT_SSH_KEY}"), Some("GIT_SSH_KEY"));
    assert_eq!(
        parse_secrets_expr("  ${secrets.ACR_PERSONAL}  "),
        Some("ACR_PERSONAL")
    );
}

#[test]
fn parse_secrets_expr_rejects_bad() {
    assert_eq!(parse_secrets_expr("GIT_SSH_KEY"), None);
    assert_eq!(parse_secrets_expr("${secrets.}"), None);
    assert_eq!(parse_secrets_expr("${vars.X}"), None);
    assert_eq!(parse_secrets_expr("${secrets.FOO.BAR}"), None);
    assert_eq!(parse_secrets_expr("${secrets.FOO"), None);
}

#[test]
fn parse_vars_expr_ok() {
    assert_eq!(parse_vars_expr("${vars.TAG}"), Some("TAG"));
}

#[test]
fn resolve_secret_slot_expr_or_bare() {
    assert_eq!(
        resolve_secret_slot("${secrets.OSS_CONFIG}"),
        Some("OSS_CONFIG".into())
    );
    assert_eq!(
        resolve_secret_slot("OSS_CONFIG"),
        Some("OSS_CONFIG".into())
    );
    assert_eq!(resolve_secret_slot("${secrets.X}extra"), None);
    assert_eq!(resolve_secret_slot(""), None);
}

#[test]
fn run_detects_secrets_expr() {
    assert!(run_contains_secrets_expr("echo ${secrets.FOO}"));
    assert!(!run_contains_secrets_expr("echo $FOO"));
}

#[test]
fn collect_secrets_names_dedupes() {
    assert_eq!(
        collect_secrets_names("a=${secrets.FOO} b=${secrets.BAR} c=${secrets.FOO}"),
        vec!["FOO".to_string(), "BAR".to_string()]
    );
    assert!(collect_secrets_names("no refs").is_empty());
}
