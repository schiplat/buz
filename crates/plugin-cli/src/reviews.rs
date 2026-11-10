//! Generate `reviews.json` entries.
//!
//! Two entry kinds live in the ledger, distinguished by `source`:
//!
//! - `ci`   — official templates (`buz/*`) whose id already carries a review.
//!   The FIRST review of an official id is human: a brand-new official id is
//!   never auto-blessed. Once the id is blessed, green CI is the tracking gate
//!   and every version of that id is refreshed on each run.
//! - `human`— community templates, and the bootstrap entry for new official
//!   ids. The entry binds the review to the content sha256, so any later
//!   content change invalidates it automatically.
//!
//! Both modes read the source root's `index.json`; run `buz-plugin index`
//! first. Auto mode keeps human entries for the same `(id, version)` — the
//! fresh `ci` badge and the first-review record coexist, both bound to the
//! same content hash. Manual mode replaces the record it emits (a fresh human
//! re-review supersedes the old one).
//!
//! Serialization keeps the declared field order (`id`, `version`, `sha256`,
//! `reviewed_at`, `review_commit`, `scope`, `source`, `category`) so the
//! ledger stays diff-stable and the signed bytes are reproducible.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

pub const AUTO_SOURCE: &str = "ci";
pub const AUTO_SCOPE: &str = "official-ci: validate + lint_runs + egress allowlist on green master";

/// Options for `buz-plugin reviews`.
#[derive(Debug, Clone)]
pub struct Options {
    pub plugins_dir: PathBuf,
    pub auto: bool,
    pub date: Option<String>,
    pub commit: Option<String>,
    pub scope: String,
    pub ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ReviewEntry {
    id: String,
    version: String,
    sha256: String,
    reviewed_at: String,
    review_commit: String,
    scope: String,
    source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    category: Option<String>,
    /// Unknown fields are preserved verbatim (future ledger extensions).
    #[serde(flatten)]
    extra: BTreeMap<String, Value>,
}

#[derive(Debug, Deserialize, Serialize)]
struct Ledger {
    reviews: Vec<ReviewEntry>,
}

impl ReviewEntry {
    fn pair(&self) -> (String, String) {
        (self.id.clone(), self.version.clone())
    }
}

pub fn run(options: &Options) -> Result<()> {
    if options.auto && !options.ids.is_empty() {
        bail!("ids are manual-mode only — use --auto without ids");
    }
    if !options.auto && options.ids.is_empty() {
        bail!("manual mode needs at least one template id");
    }

    let date = match &options.date {
        Some(date) => date.clone(),
        None if options.auto => chrono::Utc::now().format("%Y-%m-%d").to_string(),
        None => bail!("--date is required for manual mode"),
    };
    let commit = match &options.commit {
        Some(commit) => commit.clone(),
        None if options.auto => current_commit()?,
        None => bail!("--commit is required for manual mode"),
    };

    let index = load_index(&options.plugins_dir)?;
    let existing = load_existing(&options.plugins_dir)?;

    let (new_entries, keep_manual) = if options.auto {
        (auto_entries(&index, &existing, &date, &commit), true)
    } else {
        (
            manual_entries(&index, &options.ids, &date, &commit, &options.scope)?,
            false,
        )
    };
    let merged = merge(existing, new_entries, keep_manual);
    write_ledger(&options.plugins_dir, &merged)?;
    let auto_n = merged
        .iter()
        .filter(|entry| entry.source == AUTO_SOURCE)
        .count();
    println!(
        "reviews.json: {} entr(ies) ({} ci, {} human)",
        merged.len(),
        auto_n,
        merged.len() - auto_n
    );
    Ok(())
}

fn index_path(plugins_dir: &Path) -> PathBuf {
    plugins_dir.join("index.json")
}

fn ledger_path(plugins_dir: &Path) -> PathBuf {
    plugins_dir.join("reviews.json")
}

fn load_index(plugins_dir: &Path) -> Result<Value> {
    let path = index_path(plugins_dir);
    let raw = std::fs::read_to_string(&path).with_context(|| {
        format!(
            "{}: cannot read — run `buz-plugin index` first",
            path.display()
        )
    })?;
    let index: Value =
        serde_json::from_str(&raw).with_context(|| format!("{}: invalid JSON", path.display()))?;
    let steps = index
        .get("steps")
        .and_then(Value::as_array)
        .with_context(|| format!("{}: expected a `steps` array", path.display()))?;
    for step in steps {
        let Some(step) = step.as_object() else {
            bail!("{}: every step must be an object", path.display());
        };
        for key in ["id", "version", "sha256"] {
            if !step.get(key).is_some_and(Value::is_string) {
                bail!("{}: each step needs {key}", path.display());
            }
        }
    }
    Ok(index)
}

fn load_existing(plugins_dir: &Path) -> Result<Vec<ReviewEntry>> {
    let path = ledger_path(plugins_dir);
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(error).with_context(|| format!("{}: cannot read", path.display()))
        }
    };
    let ledger: Ledger = serde_json::from_str(&raw)
        .with_context(|| format!("{}: expected a `reviews` array of objects", path.display()))?;
    Ok(ledger.reviews)
}

fn auto_entries(
    index: &Value,
    existing: &[ReviewEntry],
    date: &str,
    commit: &str,
) -> Vec<ReviewEntry> {
    // An id counts as blessed when any review entry for it already exists.
    // All versions of a blessed id stay fresh; a brand-new official id is
    // skipped and must be recorded manually after its first human review.
    let blessed: BTreeSet<&str> = existing.iter().map(|entry| entry.id.as_str()).collect();
    let mut out = Vec::new();
    let mut new_ids = BTreeSet::new();
    for step in index["steps"].as_array().expect("validated steps array") {
        let id = step["id"].as_str().expect("validated id");
        if !id.starts_with("buz/") {
            continue;
        }
        if !blessed.contains(id) {
            new_ids.insert(id.to_string());
            continue;
        }
        out.push(review_entry(step, date, commit, AUTO_SCOPE, AUTO_SOURCE));
    }
    if !new_ids.is_empty() {
        eprintln!(
            "first review is manual — new official id(s) not auto-blessed: {}",
            new_ids.into_iter().collect::<Vec<_>>().join(", ")
        );
    }
    out
}

fn manual_entries(
    index: &Value,
    ids: &[String],
    date: &str,
    commit: &str,
    scope: &str,
) -> Result<Vec<ReviewEntry>> {
    let mut out = Vec::new();
    for id in ids {
        let mut best: Option<(u64, u64, u64, &Value)> = None;
        for step in index["steps"].as_array().expect("validated steps array") {
            if step["id"].as_str() != Some(id.as_str()) {
                continue;
            }
            let version = step["version"].as_str().expect("validated version");
            let key = semver_key(version)?;
            if best
                .as_ref()
                .is_none_or(|(major, minor, patch, _)| key > (*major, *minor, *patch))
            {
                best = Some((key.0, key.1, key.2, step));
            }
        }
        let Some((_, _, _, step)) = best else {
            bail!("id {id:?} not present in index.json");
        };
        out.push(review_entry(step, date, commit, scope, "human"));
    }
    Ok(out)
}

fn review_entry(step: &Value, date: &str, commit: &str, scope: &str, source: &str) -> ReviewEntry {
    ReviewEntry {
        id: step["id"].as_str().unwrap_or_default().to_string(),
        version: step["version"].as_str().unwrap_or_default().to_string(),
        sha256: step["sha256"].as_str().unwrap_or_default().to_string(),
        reviewed_at: date.to_string(),
        review_commit: commit.to_string(),
        scope: scope.to_string(),
        source: source.to_string(),
        category: step
            .get("category")
            .and_then(Value::as_str)
            .map(str::to_string),
        extra: BTreeMap::new(),
    }
}

fn semver_key(version: &str) -> Result<(u64, u64, u64)> {
    let parts: Vec<&str> = version.split('.').collect();
    let parsed: Option<Vec<u64>> = parts.iter().map(|part| part.parse::<u64>().ok()).collect();
    match parsed {
        Some(numbers) if numbers.len() == 3 => Ok((numbers[0], numbers[1], numbers[2])),
        _ => bail!("index.json: invalid semantic version {version:?}"),
    }
}

/// Upsert exact content reviews without erasing human evidence.
///
/// A `human` entry is the org's first-review record — an auto badge refresh
/// never replaces it (`keep_manual`): the fresh `ci` badge and the human
/// first-review coexist, both bound to the same content hash. Manual mode
/// (`keep_manual == false`) replaces the pair it emits.
fn merge(
    existing: Vec<ReviewEntry>,
    new_entries: Vec<ReviewEntry>,
    keep_manual: bool,
) -> Vec<ReviewEntry> {
    let replaced: BTreeSet<(String, String)> = new_entries.iter().map(ReviewEntry::pair).collect();
    let retained: Vec<ReviewEntry> = existing
        .into_iter()
        .filter(|entry| {
            !replaced.contains(&entry.pair()) || (keep_manual && entry.source == "human")
        })
        .collect();
    let mut merged = new_entries;
    merged.extend(retained);
    merged
}

fn write_ledger(plugins_dir: &Path, entries: &[ReviewEntry]) -> Result<()> {
    let path = ledger_path(plugins_dir);
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)
        .with_context(|| format!("cannot create {}", parent.display()))?;
    let content = format!(
        "{}\n",
        serde_json::to_string_pretty(&Ledger {
            reviews: entries.to_vec(),
        })?
    );
    // Write atomically so an interrupted command cannot truncate the trust file.
    let temporary = parent.join(format!(".reviews.{}.tmp", std::process::id()));
    std::fs::write(&temporary, content)
        .with_context(|| format!("cannot write {}", temporary.display()))?;
    std::fs::rename(&temporary, &path)
        .with_context(|| format!("cannot replace {}", path.display()))?;
    Ok(())
}

fn current_commit() -> Result<String> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output();
    match output {
        Ok(output) if output.status.success() => {
            Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
        }
        _ => bail!("--commit required outside a git repository"),
    }
}
