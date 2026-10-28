use manager::config::*;

/// A manager.toml whose [plugin_index] section exists but omits the field
/// must still get the semantic default (60), not the numeric zero from a
/// derived Default.
#[test]
fn missing_sync_timeout_in_toml_gets_semantic_default() {
    let cfg: Config = config::Config::builder()
        .add_source(config::File::from_str(
            "[plugin_index]\n",
            config::FileFormat::Toml,
        ))
        .build()
        .expect("build config")
        .try_deserialize()
        .expect("deserialize config");
    assert_eq!(cfg.plugin_index.sync_timeout_seconds, 60);
}

/// Empirical: the actual demo manager.toml (no [storage] section, no
/// local_dir) must produce the data-root plugins path — the exact
/// regression the /private/tmp live check caught.
#[test]
fn demo_manager_toml_yields_data_root_plugins() {
    let demo = std::path::Path::new("/tmp/buz-demo/example/conf/manager.toml");
    if !demo.exists() {
        return; // demo not present on this machine
    }
    let cfg: Config = config::Config::builder()
        .add_source(config::File::with_name(demo.to_str().unwrap()))
        .build()
        .expect("build demo config")
        .try_deserialize()
        .expect("deserialize demo config");
    assert_eq!(
        cfg.storage.data_dir, "./data",
        "derived Default must carry the semantic value, not empty"
    );
    let dir = manager::services::plugin_resolver::local_plugin_dir(
        &cfg.storage.data_dir,
        &cfg.plugin_index.local_dir,
    )
    .unwrap();
    assert!(
        dir.to_string_lossy().ends_with("data/plugins"),
        "got {dir:?}"
    );
}

/// No alias: a config still using `[step_index]` parses (unknown sections
/// are ignored) but its settings are NOT applied — the section must be
/// renamed to `[plugin_index]`. Locked here so the omission is a decision,
/// not an accident someone "fixes" by re-adding a silent fallback.
#[test]
fn legacy_step_index_section_is_ignored_not_merged() {
    let cfg: Config = config::Config::builder()
        .add_source(config::File::from_str(
            r#"
[server]
host = "127.0.0.1"

[step_index]
local_dir = "/srv/legacy-plugins"
reviewed_manifest = "/srv/legacy/reviews.json"
require_reviewed = true
"#,
            config::FileFormat::Toml,
        ))
        .build()
        .expect("build config")
        .try_deserialize()
        .expect("unknown sections are ignored");

    assert!(
        cfg.plugin_index.local_dir.is_empty()
            && cfg.plugin_index.reviewed_manifest.is_empty()
            && !cfg.plugin_index.require_reviewed,
        "legacy section must not be merged: {:?}",
        cfg.plugin_index
    );
}

/// The warning above is only useful if the detection works: a legacy
/// section must be observable on the merged config.
#[test]
fn legacy_section_is_detectable_on_the_merged_config() {
    let merged = config::Config::builder()
        .add_source(config::File::from_str(
            "[step_index]\nlocal_dir = \"/srv/legacy\"\n",
            config::FileFormat::Toml,
        ))
        .build()
        .expect("build config");
    assert!(merged.get_table("step_index").is_ok());
    assert!(merged.get_table("plugin_index").is_err());
}

#[test]
fn no_step_index_section_at_all_gets_semantic_default() {
    // Same code path as config::load (config-rs, not toml::from_str).
    let cfg: Config = config::Config::builder()
        .add_source(config::File::from_str(
            r#"
[server]
host = "127.0.0.1"
"#,
            config::FileFormat::Toml,
        ))
        .build()
        .expect("build config")
        .try_deserialize()
        .expect("deserialize config");
    assert_eq!(cfg.plugin_index.sync_timeout_seconds, 60);
}
