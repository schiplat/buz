use super::step_validate;

fn write_spec(dir: &std::path::Path, file: &str, id: &str, version: &str) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(
        dir.join(file),
        format!("id: {id}\npublisher: official\nversion: {version}\nrun: echo ok\n"),
    )
    .unwrap();
}

#[test]
fn recursively_validates_specs_and_rejects_duplicate_id_version() {
    let dir = tempfile::tempdir().unwrap();
    write_spec(dir.path(), "one.yaml", "buz/notify", "1.0.0");
    write_spec(&dir.path().join("nested"), "two.yml", "buz/notify", "1.0.0");
    write_spec(
        &dir.path().join("nested"),
        "different-version.yml",
        "buz/notify",
        "1.1.0",
    );
    write_spec(
        &dir.path().join(".hidden"),
        "hidden.yaml",
        "buz/hidden",
        "1.0.0",
    );

    let error = step_validate(&[dir.path().to_string_lossy().into_owned()]).unwrap_err();
    assert_eq!(error.to_string(), "1 of 3 step spec(s) failed validation");
}

#[test]
fn step_validate_accepts_distinct_versions_and_skips_symlinked_files() {
    let dir = tempfile::tempdir().unwrap();
    write_spec(dir.path(), "one.yaml", "buz/notify", "1.0.0");
    write_spec(dir.path(), "two.yaml", "buz/notify", "1.1.0");
    std::fs::write(dir.path().join(".hidden.yaml"), "not: a step").unwrap();

    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        let external = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            external.path(),
            "id: buz/linked\npublisher: official\nversion: 1.0.0\nrun: echo linked\n",
        )
        .unwrap();
        symlink(external.path(), dir.path().join("linked.yaml")).unwrap();
    }

    step_validate(&[dir.path().to_string_lossy().into_owned()]).unwrap();
}

#[test]
fn step_validate_rejects_direct_symlink_arguments() {
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target.yaml");
        write_spec(dir.path(), "target.yaml", "buz/notify", "1.0.0");
        let link = dir.path().join("link.yaml");
        symlink(target, &link).unwrap();

        let error = step_validate(&[link.to_string_lossy().into_owned()]).unwrap_err();
        assert!(error.to_string().contains("symlink"), "{error}");
    }
}
