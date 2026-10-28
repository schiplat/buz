use shared::yaml_files::discover_yaml_files;
use std::path::Path;

fn test_dir() -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("buz-yaml-discovery-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&path).unwrap();
    path
}

#[test]
fn recursively_finds_yaml_in_deterministic_order_and_skips_hidden_paths() {
    let root = test_dir();
    std::fs::create_dir_all(root.join("nested")).unwrap();
    std::fs::create_dir_all(root.join(".hidden-dir")).unwrap();
    for relative in [
        "z.yaml",
        "nested/b.yml",
        "nested/a.yaml",
        ".hidden.yaml",
        ".hidden-dir/secret.yaml",
        "notes.txt",
    ] {
        std::fs::write(root.join(relative), "test").unwrap();
    }

    let files = discover_yaml_files(&root).unwrap();
    let relative: Vec<_> = files
        .iter()
        .map(|path| {
            path.strip_prefix(&root)
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    assert_eq!(relative, ["nested/a.yaml", "nested/b.yml", "z.yaml"]);

    std::fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn skips_symlink_files_and_directories() {
    use std::os::unix::fs::symlink;

    let root = test_dir();
    let outside = test_dir();
    std::fs::write(outside.join("external.yaml"), "test").unwrap();
    std::fs::write(root.join("real.yaml"), "test").unwrap();
    symlink(outside.join("external.yaml"), root.join("linked.yaml")).unwrap();
    symlink(&outside, root.join("linked-dir")).unwrap();

    let files = discover_yaml_files(&root).unwrap();
    assert_eq!(files, [root.join("real.yaml")]);

    std::fs::remove_dir_all(root).unwrap();
    std::fs::remove_dir_all(outside).unwrap();
}

#[test]
fn a_symlink_root_is_not_followed() {
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;

        let root = test_dir();
        let link = root.with_extension("link");
        symlink(&root, &link).unwrap();
        assert!(discover_yaml_files(Path::new(&link)).unwrap().is_empty());
        std::fs::remove_file(link).unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }
}
