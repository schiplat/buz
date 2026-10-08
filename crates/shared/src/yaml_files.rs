//! Deterministic discovery of YAML files beneath a directory.
//!
//! Only regular `.yaml` / `.yml` files are returned. Hidden paths and
//! symlinks are not traversed or included.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Recursively discover YAML files under `root` in deterministic path order.
///
/// A symlink root is treated as an empty tree, and symlinks encountered below
/// the root are always skipped rather than followed.
pub fn discover_yaml_files(root: &Path) -> io::Result<Vec<PathBuf>> {
    let metadata = fs::symlink_metadata(root)?;
    if metadata.file_type().is_symlink() {
        return Ok(Vec::new());
    }
    if !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is not a directory", root.display()),
        ));
    }

    let mut files = Vec::new();
    visit_directory(root, &mut files)?;
    files.sort();
    Ok(files)
}

fn visit_directory(directory: &Path, files: &mut Vec<PathBuf>) -> io::Result<()> {
    let mut entries = fs::read_dir(directory)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.file_name());

    for entry in entries {
        let name = entry.file_name();
        if name.to_string_lossy().starts_with('.') {
            continue;
        }

        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            visit_directory(&path, files)?;
        } else if file_type.is_file()
            && matches!(
                path.extension().and_then(|ext| ext.to_str()),
                Some("yaml" | "yml")
            )
        {
            files.push(path);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::discover_yaml_files;
    use std::path::Path;

    fn test_dir() -> std::path::PathBuf {
        let path =
            std::env::temp_dir().join(format!("buz-yaml-discovery-{}", uuid::Uuid::new_v4()));
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
}
