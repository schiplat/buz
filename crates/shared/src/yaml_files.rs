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
