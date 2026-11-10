//! Derive `revision.json` from the catalog published in the index.
//!
//! `revision.json` is the mirror drift sentinel: the manager fetches it from
//! the canonical source and from each mirror and compares the two values for
//! **equality** (`check_mirror` in `crates/manager/src/services/plugin_pool.rs`).
//! It therefore needs a stable identity for the catalog, not a build marker.
//!
//! The value is catalog-derived: the first 16 hex characters of sha256 over
//! the sorted `(id, version, sha256)` triples in `index.json`. Two trees
//! publishing the same versions with the same content hashes produce the same
//! revision no matter when or where the index was written. Consequences:
//!
//! - the committed copy cannot trail `HEAD`: it changes exactly when a
//!   published version arrives, disappears, or changes content;
//! - a mirror serving a different catalog is detectable with two tiny GETs,
//!   no index download;
//! - no write-back race: regeneration from the same catalog is byte-identical,
//!   so a bot commit that lands late (or never) cannot leave the tree stale.
//!
//! `updated_at` mirrors the index's `issued_at` — display only; mirrors
//! compare `revision` alone. Write order stays index → per-plugin `meta.json`
//! → `revision.json` last, so a mirror that observes a new revision knows the
//! index beside it is complete.

use anyhow::{Context, Result};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// Options for `buz-plugin revision`.
#[derive(Debug, Clone)]
pub struct Options {
    pub index: PathBuf,
    pub output: PathBuf,
    pub print_only: bool,
}

#[derive(Serialize)]
struct RevisionFile {
    revision: String,
    updated_at: Option<String>,
}

/// Content identity of one catalog: sha256 over sorted (id, version, sha256).
///
/// Deliberately ignores `sequence`, `issued_at`, entry order, and every scalar
/// field outside the content triple: those describe a *build*, and a sentinel
/// that moves on every build would trail its source again.
pub fn catalog_revision(index: &serde_json::Value) -> Result<String> {
    let steps = index
        .get("steps")
        .and_then(|value| value.as_array())
        .context("index.json: expected a `steps` array")?;
    let mut material: Vec<String> = Vec::with_capacity(steps.len());
    for step in steps {
        let Some(step) = step.as_object() else {
            continue;
        };
        let id = step
            .get("id")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        let version = step
            .get("version")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        let sha = step
            .get("sha256")
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .to_lowercase();
        material.push(format!("{id}\u{0}{version}\u{0}{sha}"));
    }
    material.sort();
    let mut hasher = Sha256::new();
    hasher.update(material.join("\n").as_bytes());
    let digest = format!("{:x}", hasher.finalize());
    Ok(digest[..16].to_string())
}

pub fn run(options: &Options) -> Result<()> {
    let raw = std::fs::read_to_string(&options.index)
        .with_context(|| format!("{}: cannot read", options.index.display()))?;
    let index: serde_json::Value = serde_json::from_str(&raw)
        .with_context(|| format!("{}: invalid JSON", options.index.display()))?;
    if !index.is_object() {
        anyhow::bail!("{}: expected a JSON object", options.index.display());
    }
    let revision = catalog_revision(&index)?;
    if options.print_only {
        println!("{revision}");
        return Ok(());
    }
    let file = RevisionFile {
        revision,
        updated_at: index
            .get("issued_at")
            .and_then(|value| value.as_str())
            .map(str::to_string),
    };
    let content = format!("{}\n", serde_json::to_string_pretty(&file)?);
    write_atomic(&options.output, &content)?;
    println!(
        "revision.json: {} written to {}",
        file.revision,
        options.output.display()
    );
    Ok(())
}

/// Write the sentinel atomically: an interrupted command must never leave a
/// truncated sentinel that mirrors would compare against.
fn write_atomic(path: &Path, content: &str) -> Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)
        .with_context(|| format!("cannot create {}", parent.display()))?;
    let temporary = parent.join(format!(".revision.{}.tmp", std::process::id()));
    std::fs::write(&temporary, content)
        .with_context(|| format!("cannot write {}", temporary.display()))?;
    std::fs::rename(&temporary, path)
        .with_context(|| format!("cannot replace {}", path.display()))?;
    Ok(())
}
