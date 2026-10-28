use shared::log_masking::*;
use std::collections::HashMap;

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
fn forced_masks_short_values() {
    let text = "PASSWORD=abc SECRET=ab END";
    let secrets = vec!["abc".to_string(), "ab".to_string()];
    let masked = mask_secrets_forced(text, &secrets);
    assert_eq!(masked, "PASSWORD=*** SECRET=*** END");
}

#[test]
fn forced_skips_empty_values() {
    let text = "K=v END";
    let secrets = vec!["".to_string(), "v".to_string()];
    assert_eq!(mask_secrets_forced(text, &secrets), "K=*** END");
}

#[test]
fn forced_empty_secrets_noop() {
    assert_eq!(mask_secrets_forced("hello", &[]), "hello");
    assert_eq!(mask_secrets_forced("", &["x".to_string()]), "");
}

#[test]
fn forced_masks_values_shorter_than_normal_floor() {
    let text = "token: pi (2 chars)";
    let secrets = vec!["pi".to_string()];
    // Normal masking would skip `pi` (2 chars); forced masks it.
    assert_eq!(mask_secrets(text, &secrets), "token: pi (2 chars)");
    assert_eq!(mask_secrets_forced(text, &secrets), "token: *** (2 chars)");
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
fn catalog_collects_secret_key_values_and_direct_values() {
    let mut env = HashMap::new();
    env.insert("DOCKER_PASS".into(), "hunter2".into());
    env.insert("SMOKE_ENV".into(), "from-pipeline".into());

    let mut catalog = SecretCatalog::new();
    catalog.add_key("DOCKER_PASS");
    catalog.add_value("http://oss/signed?sig=abc");

    let values = catalog.secret_values(&env);
    assert!(
        values.contains(&"hunter2".to_string()),
        "key value collected"
    );
    assert!(
        values.contains(&"http://oss/signed?sig=abc".to_string()),
        "direct value collected"
    );
    assert!(
        !values.contains(&"from-pipeline".to_string()),
        "non-secret env key value not collected"
    );
}

#[test]
fn catalog_ignores_empty_values() {
    let mut env = HashMap::new();
    env.insert("EMPTY".into(), "".into());
    env.insert("KEEP".into(), "val".into());

    let mut catalog = SecretCatalog::new();
    catalog.add_key("EMPTY");
    catalog.add_key("KEEP");
    catalog.add_value("");

    let values = catalog.secret_values(&env);
    assert_eq!(values, vec!["val".to_string()]);
}

#[test]
fn catalog_add_keys_batch_and_is_secret_key() {
    let mut catalog = SecretCatalog::new();
    catalog.add_keys(vec!["A".to_string(), "B".to_string()]);
    assert!(catalog.is_secret_key("A"));
    assert!(catalog.is_secret_key("B"));
    assert!(!catalog.is_secret_key("C"));
}

#[test]
fn injected_key_is_secret_but_marked_for_hint() {
    let mut catalog = SecretCatalog::new();
    catalog.add_injected_key("BUZ_CONFIG_URL");
    assert!(
        catalog.is_secret_key("BUZ_CONFIG_URL"),
        "value still masked"
    );
    assert!(catalog.is_injected_key("BUZ_CONFIG_URL"));
    assert!(!catalog.is_injected_key("acr_personal"));

    // Value still participates in value-based masking.
    let mut env = HashMap::new();
    env.insert("BUZ_CONFIG_URL".into(), "http://oss/signed?sig=abc".into());
    assert!(catalog
        .secret_values(&env)
        .contains(&"http://oss/signed?sig=abc".to_string()));
}

#[test]
fn catalog_default_is_empty() {
    let catalog = SecretCatalog::new();
    assert!(!catalog.is_secret_key("anything"));
    assert!(!catalog.is_injected_key("anything"));
    assert!(catalog.secret_values(&HashMap::new()).is_empty());
}

#[test]
fn catalog_forced_values_bypass_length_floor() {
    let mut catalog = SecretCatalog::new();
    catalog.add_forced_value("pi");
    catalog.add_forced_value("ab");
    catalog.add_forced_value("");
    assert_eq!(
        catalog.forced_secret_values(),
        vec!["pi".to_string(), "ab".to_string()]
    );
}

#[test]
fn catalog_default_forced_empty() {
    let catalog = SecretCatalog::new();
    assert!(catalog.forced_secret_values().is_empty());
}
