use plugin_cli::readme_check::{run, Options};
use std::path::Path;

fn spec(root: &Path, relative: &str) {
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, "id: buz/a\nversion: 1.0.0\n").unwrap();
}

#[test]
fn every_plugin_directory_with_a_readme_passes() {
    let dir = tempfile::tempdir().unwrap();
    let plugins = dir.path().join("plugins");
    spec(&plugins, "buz/a/1.0.0.yaml");
    std::fs::write(plugins.join("buz/a/README.md"), "# A\n").unwrap();
    run(&Options {
        plugins_dir: plugins,
    })
    .unwrap();
}

#[test]
fn a_missing_or_empty_readme_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let plugins = dir.path().join("plugins");
    spec(&plugins, "buz/a/1.0.0.yaml");
    let error = run(&Options {
        plugins_dir: plugins.clone(),
    })
    .unwrap_err();
    assert!(format!("{error:#}").contains("README"));

    std::fs::write(plugins.join("buz/a/README.md"), "   \n").unwrap();
    let error = run(&Options {
        plugins_dir: plugins,
    })
    .unwrap_err();
    assert!(format!("{error:#}").contains("README"));
}

#[test]
fn hidden_directories_are_ignored() {
    let dir = tempfile::tempdir().unwrap();
    let plugins = dir.path().join("plugins");
    spec(&plugins, "buz/a/1.0.0.yaml");
    std::fs::write(plugins.join("buz/a/README.md"), "# A\n").unwrap();
    spec(&plugins, ".hidden/1.0.0.yaml");
    run(&Options {
        plugins_dir: plugins,
    })
    .unwrap();
}
