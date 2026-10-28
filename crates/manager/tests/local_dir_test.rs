use manager::services::plugin_resolver::*;
use std::path::{Path, PathBuf};

#[test]
fn empty_anchors_to_data_root() {
    let dir = local_plugin_dir("/srv/buz/data", "").unwrap();
    assert_eq!(dir, Path::new("/srv/buz/data").join("plugins"));
}

#[test]
fn relative_anchors_to_data_root() {
    let dir = local_plugin_dir("/srv/buz/data", "my-plugins").unwrap();
    assert_eq!(dir, Path::new("/srv/buz/data").join("my-plugins"));
}

#[test]
fn parent_components_are_rejected() {
    let err = local_plugin_dir("/srv/buz/data", "../escape").unwrap_err();
    assert!(err.to_string().contains("data directory"), "{err}");
}

#[test]
fn absolute_paths_are_used_as_is() {
    let dir = local_plugin_dir("/srv/buz/data", "/opt/data/plugins").unwrap();
    assert_eq!(
        dir,
        PathBuf::from("/opt/data/plugins"),
        "absolute dirs are an explicit admin choice"
    );
}

#[test]
fn reviewed_manifest_defaults_beside_plugins() {
    assert_eq!(
        reviewed_manifest_default(Path::new("/srv/buz/data")),
        Path::new("/srv/buz/data")
            .join("plugins")
            .join("reviews.json")
    );
}
