use super::*;

#[test]
fn masks_known_secret() {
    let text = "Using API key: sk-abc123xyz for authentication";
    let secrets = vec!["sk-abc123xyz".to_string()];
    let masked = mask_secrets(text, &secrets);
    assert_eq!(masked, "Using API key: *** for authentication");
}

#[test]
fn skips_short_values() {
    let text = "a b c d";
    let secrets = vec!["a".to_string(), "b".to_string()];
    let masked = mask_secrets(text, &secrets);
    assert_eq!(masked, "a b c d");
}

#[test]
fn empty_secrets_noop() {
    let text = "hello world";
    assert_eq!(mask_secrets(text, &[]), "hello world");
}

#[test]
fn empty_text_returns_empty() {
    assert_eq!(mask_secrets("", &["secret".to_string()]), "");
}

#[test]
fn handles_multiple_occurrences() {
    let text = "token=secret123 token=secret123 done";
    let secrets = vec!["secret123".to_string()];
    let masked = mask_secrets(text, &secrets);
    assert_eq!(masked, "token=*** token=*** done");
}

#[test]
fn collect_secret_values_skips_empty() {
    let mut env = std::collections::HashMap::new();
    env.insert("A".into(), "value".into());
    env.insert("B".into(), "".into());
    let values = collect_secret_values(&env);
    assert_eq!(values, vec!["value".to_string()]);
}

#[test]
fn collect_secret_values_for_keys_ignores_non_secret_env() {
    let mut env = std::collections::HashMap::new();
    env.insert("SMOKE_ENV".into(), "from-pipeline".into());
    env.insert("DOCKER_PASS".into(), "hunter2".into());
    let mut keys = std::collections::HashSet::new();
    keys.insert("DOCKER_PASS".into());
    let values = collect_secret_values_for_keys(&env, &keys);
    assert_eq!(values, vec!["hunter2".to_string()]);
}
