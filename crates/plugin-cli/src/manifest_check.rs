//! Verify the generated manifests agree with each other.
//!
//! `index.json` answers *what exists*; `reviews.json` answers *what was
//! approved*; `revision.json` names *which catalog this is*. Every version
//! published in the index must carry a review entry bound to the **same**
//! content sha256, and the sentinel must equal the catalog digest:
//!
//! - a missing review entry means `require_reviewed` deployments reject that
//!   version (and a mirror trusting the index ships it unreviewed);
//! - a mismatched sha256 means the review predates a content change, so the
//!   badge no longer applies;
//! - a stale sentinel means mirrors compare a value that no longer describes
//!   this catalog.
//!
//! `meta.json` is checked too: it is the Manager's single-plugin refresh path,
//! so a stale or missing copy advertises a version (or hash) the index no
//! longer agrees with.
//!
//! This runs in the `validate` job. Reviews are refreshed by
//! `buz-plugin reviews --auto` (blessed ids) or a manual first review; the fix
//! for a failure is always to regenerate and commit the manifest, never to
//! edit hashes by hand.

use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::revision::catalog_revision;

pub struct Options {
    pub plugins_dir: PathBuf,
}

pub fn run(options: &Options) -> Result<()> {
    let index = load_json(&options.plugins_dir.join("index.json"))?;
    let reviews = load_json(&options.plugins_dir.join("reviews.json"))?;
    let revision = load_json(&options.plugins_dir.join("revision.json"))?;

    let (mut errors, mut warnings) = check(&index, &reviews)?;
    errors.extend(check_revision(&index, &revision)?);
    let (meta_errors, meta_warnings) = check_meta(&index, &options.plugins_dir)?;
    errors.extend(meta_errors);
    warnings.extend(meta_warnings);

    for warning in &warnings {
        eprintln!("warning: {warning}");
    }
    if !errors.is_empty() {
        for error in &errors {
            eprintln!("error: {error}");
        }
        bail!(
            "{} manifest inconsistency(ies) — index, reviews, and meta must agree",
            errors.len()
        );
    }
    println!("manifests consistent (reviews cover every indexed version)");
    Ok(())
}

fn load_json(path: &Path) -> Result<Value> {
    let raw = std::fs::read_to_string(path).with_context(|| {
        format!(
            "{}: missing — generated manifests must be committed",
            path.display()
        )
    })?;
    let value: Value =
        serde_json::from_str(&raw).with_context(|| format!("{}: invalid JSON", path.display()))?;
    if !value.is_object() {
        bail!("{}: expected a JSON object", path.display());
    }
    Ok(value)
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn pair_of(value: &Value) -> (String, String) {
    (
        value
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        value
            .get("version")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
    )
}

fn show(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "?".into())
}

pub fn check(index: &Value, reviews: &Value) -> Result<(Vec<String>, Vec<String>)> {
    let mut errors = Vec::new();
    let mut warnings = Vec::new();

    let Some(steps) = index.get("steps").and_then(Value::as_array) else {
        return Ok((
            vec!["index.json: expected a `steps` array".into()],
            warnings,
        ));
    };
    let Some(entries) = reviews.get("reviews").and_then(Value::as_array) else {
        return Ok((
            vec!["reviews.json: expected a `reviews` array".into()],
            warnings,
        ));
    };

    let mut published: BTreeMap<(String, String), String> = BTreeMap::new();
    for step in steps {
        let complete = step.get("id").is_some()
            && step.get("version").is_some()
            && step.get("sha256").is_some();
        if !step.is_object() || !complete {
            errors.push(format!(
                "index.json: entry without id/version/sha256: {}",
                show(step)
            ));
            continue;
        }
        let pair = pair_of(step);
        let sha = step["sha256"].as_str().unwrap_or("").to_lowercase();
        if !is_sha256(&sha) {
            errors.push(format!(
                "{}@{}: index sha256 is not 64 lowercase hex chars: {sha:?}",
                pair.0, pair.1
            ));
        }
        if published.contains_key(&pair) {
            errors.push(format!(
                "{}@{}: duplicate index entry — one (id, version) must be one file",
                pair.0, pair.1
            ));
        }
        published.insert(pair, sha);
    }

    let mut reviewed: BTreeMap<(String, String), String> = BTreeMap::new();
    for entry in entries {
        if !entry.is_object() {
            errors.push(format!(
                "reviews.json: entry is not an object: {}",
                show(entry)
            ));
            continue;
        }
        let pair = pair_of(entry);
        let sha = entry
            .get("sha256")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_lowercase();
        if let Some(previous) = reviewed.get(&pair) {
            if previous != &sha {
                errors.push(format!(
                    "{}@{}: conflicting review entries bind different sha256 ({previous}, {sha}) — keep exactly one",
                    pair.0, pair.1
                ));
            }
        }
        reviewed.insert(pair, sha);
    }

    let reviewed_ids: BTreeSet<String> = entries
        .iter()
        .filter_map(|entry| entry.get("id").and_then(Value::as_str).map(str::to_string))
        .collect();

    for (pair, sha) in &published {
        match reviewed.get(pair) {
            None if reviewed_ids.contains(&pair.0) => errors.push(format!(
                "{}@{}: no review entry — refresh the ledger with `buz-plugin reviews --auto` and commit reviews.json",
                pair.0, pair.1
            )),
            None => errors.push(format!(
                "{}@{}: no review entry and this id has no first review — review the exact file, then record it with `buz-plugin reviews --date <YYYY-MM-DD> --commit <sha> --scope '<what you checked>' {}`",
                pair.0, pair.1, pair.0
            )),
            Some(reviewed_sha) if reviewed_sha != sha => errors.push(format!(
                "{}@{}: review binds sha256 {reviewed_sha} but the index publishes {sha} — the content changed after review; re-review and refresh the ledger",
                pair.0, pair.1
            )),
            Some(_) => {}
        }
    }

    for pair in reviewed.keys() {
        if !published.contains_key(pair) {
            warnings.push(format!(
                "{}@{}: review entry with no matching index version (removed or pinned version) — kept for audit",
                pair.0, pair.1
            ));
        }
    }

    Ok((errors, warnings))
}

pub fn check_revision(index: &Value, revision: &Value) -> Result<Vec<String>> {
    let expected = catalog_revision(index)?;
    let actual = revision.get("revision").and_then(Value::as_str);
    if actual == Some(expected.as_str()) {
        return Ok(Vec::new());
    }
    Ok(vec![format!(
        "revision.json: stale sentinel {actual:?}, expected {expected:?} for this catalog — run `buz-plugin revision` and commit the result"
    )])
}

pub fn check_meta(index: &Value, plugins_root: &Path) -> Result<(Vec<String>, Vec<String>)> {
    let mut errors = Vec::new();
    let warnings = Vec::new();
    let Some(steps) = index.get("steps").and_then(Value::as_array) else {
        return Ok((errors, warnings));
    };
    for step in steps {
        if !step.is_object() {
            continue;
        }
        let pair = pair_of(step);
        let Some(file_rel) = step.get("file").and_then(Value::as_str) else {
            errors.push(format!(
                "{}@{}: index entry has no `file` path",
                pair.0, pair.1
            ));
            continue;
        };
        if file_rel.is_empty() {
            errors.push(format!(
                "{}@{}: index entry has no `file` path",
                pair.0, pair.1
            ));
            continue;
        }
        let meta_path = plugins_root
            .join(
                Path::new(file_rel)
                    .parent()
                    .unwrap_or_else(|| Path::new("")),
            )
            .join("meta.json");
        if !meta_path.exists() {
            errors.push(format!(
                "{}@{}: {} is missing — run `buz-plugin index` and commit it",
                pair.0,
                pair.1,
                meta_path.display()
            ));
            continue;
        }
        let raw = std::fs::read_to_string(&meta_path)?;
        let meta: Value = match serde_json::from_str(&raw) {
            Ok(meta) => meta,
            Err(error) => {
                errors.push(format!("{}: invalid JSON: {error}", meta_path.display()));
                continue;
            }
        };
        if meta.get("id").and_then(Value::as_str) != Some(pair.0.as_str()) {
            errors.push(format!(
                "{}: declares id {:?}, expected {:?}",
                meta_path.display(),
                meta.get("id"),
                pair.0
            ));
        }
        let Some(versions) = meta.get("versions").and_then(Value::as_array) else {
            errors.push(format!(
                "{}: expected a `versions` array",
                meta_path.display()
            ));
            continue;
        };
        let entry = versions.iter().find(|candidate| {
            candidate
                .get("version")
                .and_then(Value::as_str)
                .map(str::to_string)
                .as_deref()
                == Some(pair.1.as_str())
        });
        let Some(entry) = entry else {
            errors.push(format!(
                "{}@{}: not listed in {} — run `buz-plugin index` and commit it",
                pair.0,
                pair.1,
                meta_path.display()
            ));
            continue;
        };
        let meta_sha = entry
            .get("sha256")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_lowercase();
        let index_sha = step
            .get("sha256")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_lowercase();
        if meta_sha != index_sha {
            errors.push(format!(
                "{}@{}: meta.json binds sha256 {meta_sha} but the index publishes {index_sha} — regenerate the manifest",
                pair.0, pair.1
            ));
        }
    }
    Ok((errors, warnings))
}
