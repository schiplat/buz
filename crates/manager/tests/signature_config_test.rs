use manager::config::*;

/// The docs advertise these two keys under `[plugin_index]`; a silently
/// ignored key would leave a deployment that believes it is verifying
/// signatures without verifying anything.
#[test]
fn plugin_index_signature_keys_are_parsed() {
    let cfg: Config = config::Config::builder()
        .add_source(config::File::from_str(
            r#"
[server]
host = "127.0.0.1"

[plugin_index]
index_public_key = "QlO0bHFJmHzQ5r3AcDoy+pmIwb4boh2kA/drxMFgsjA="
require_signed_index = true
"#,
            config::FileFormat::Toml,
        ))
        .build()
        .expect("build config")
        .try_deserialize()
        .expect("keys accepted");

    assert_eq!(
        cfg.plugin_index.index_public_key,
        "QlO0bHFJmHzQ5r3AcDoy+pmIwb4boh2kA/drxMFgsjA="
    );
    assert!(cfg.plugin_index.require_signed_index);
}

#[test]
fn signature_keys_default_to_off() {
    let cfg = PluginIndexConfig::default();
    assert!(cfg.index_public_key.is_empty());
    assert!(!cfg.require_signed_index);
}
