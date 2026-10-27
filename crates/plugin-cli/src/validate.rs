//! `buz-plugin validate` — the authoring moment of the Manager's save-time
//! gate. Same function (`shared::plugin_spec::validate_plugin_spec`), advisory
//! here, enforced there.

use crate::layout;
use anyhow::Context;
use shared::plugin_spec::{validate_plugin_spec, PluginSpec};
use std::collections::BTreeSet;
use std::path::PathBuf;

pub fn run(paths: &[PathBuf]) -> anyhow::Result<()> {
    let failures = check(paths)?;
    if failures > 0 {
        std::process::exit(1);
    }
    Ok(())
}

/// Validate every spec under `paths`; returns the failure count.
pub fn check(paths: &[PathBuf]) -> anyhow::Result<usize> {
    if paths.is_empty() {
        anyhow::bail!("usage: buz-plugin validate <spec.yaml | dir> [...]");
    }
    let mut files: Vec<PathBuf> = Vec::new();
    for path in paths {
        if path.is_dir() {
            files.extend(layout::discover_spec_files(path)?);
        } else {
            files.push(path.clone());
        }
    }
    if files.is_empty() {
        anyhow::bail!("no *.yaml/*.yml plugin specs found");
    }

    let mut seen: BTreeSet<(String, String)> = BTreeSet::new();
    let mut failures = 0usize;
    for path in &files {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("cannot read {}", path.display()))?;
        let outcome = validate_one(&raw);
        match outcome {
            Ok((id, version)) => {
                if !seen.insert((id.clone(), version.clone())) {
                    failures += 1;
                    eprintln!(
                        "FAIL {}: duplicate id+version {}/{}",
                        path.display(),
                        id,
                        version
                    );
                } else {
                    println!("OK   {} ({} {})", path.display(), id, version);
                }
            }
            Err(errors) => {
                failures += 1;
                eprintln!("FAIL {}:", path.display());
                for error in errors {
                    eprintln!("  - {error}");
                }
            }
        }
    }
    println!("{}/{} spec(s) passed", files.len() - failures, files.len());
    Ok(failures)
}

fn validate_one(raw: &str) -> Result<(String, String), Vec<String>> {
    let spec: PluginSpec =
        serde_yaml::from_str(raw).map_err(|error| vec![format!("invalid YAML: {error}")])?;
    validate_plugin_spec(&spec, env!("CARGO_PKG_VERSION"))?;
    Ok((spec.id.clone(), spec.version.clone()))
}
