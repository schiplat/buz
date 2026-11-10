//! Repo rule: every plugin directory must ship a non-empty README.md.
//!
//! A plugin directory is any directory under the plugins root that contains a
//! step-template YAML file (`.yaml`/`.yml`). Hidden directories and symlinks
//! are ignored, matching how the index and validator scan specs.

use anyhow::{bail, Result};
use std::collections::BTreeSet;
use std::path::PathBuf;

use crate::layout;

pub struct Options {
    pub plugins_dir: PathBuf,
}

pub fn run(options: &Options) -> Result<()> {
    if !options.plugins_dir.is_dir() {
        bail!(
            "{}: plugins directory not found",
            options.plugins_dir.display()
        );
    }
    let mut dirs: BTreeSet<PathBuf> = BTreeSet::new();
    for path in layout::discover_spec_files(&options.plugins_dir)? {
        if let Some(parent) = path.parent() {
            if parent != options.plugins_dir {
                dirs.insert(parent.to_path_buf());
            }
        }
    }

    let mut missing = Vec::new();
    for directory in &dirs {
        let readme = directory.join("README.md");
        let empty = match std::fs::read_to_string(&readme) {
            Ok(content) => content.trim().is_empty(),
            Err(_) => true,
        };
        if empty {
            missing.push(readme);
        }
    }
    if !missing.is_empty() {
        for path in &missing {
            eprintln!("missing or empty README: {}", path.display());
        }
        bail!(
            "{} plugin README(s) missing — every plugin needs one",
            missing.len()
        );
    }
    println!("plugin readmes: {} plugin dir(s) ok", dirs.len());
    Ok(())
}
