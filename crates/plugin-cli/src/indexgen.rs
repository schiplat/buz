//! Per-namespace index generation: `plugins/<ns>/index.json` (+ per-plugin
//! `meta.json` + `revision.json` sentinels + optional merged site view).
//! Byte-compatible with the Python `gen_index.py` output this command
//! supersedes: sorted entries, ordered fields, two-space JSON, trailing
//! newline.

use crate::layout;
use anyhow::Context;
use serde::Serialize;
use shared::step_spec::{validate_step_spec, StepSpec};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Index meta is deliberately scalar-only and in this exact field order —
/// the manager persists these onto pool rows so the dashboard can describe
/// a plugin without downloading the template.
const META_FIELDS: [&str; 10] = [
    "name",
    "description",
    "description_zh",
    "author",
    "publisher",
    "license",
    "source_repo",
    "engine",
    "default_timeout",
    "requires_buz",
];

pub struct Options {
    pub plugins_dir: PathBuf,
    pub merged: Option<PathBuf>,
    pub revision: Option<String>,
    pub updated_at: Option<String>,
    pub namespace: Option<String>,
    pub no_meta: bool,
}

#[derive(Serialize, Clone)]
struct IndexEntry {
    id: String,
    version: String,
    file: String,
    sha256: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    category: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    description_zh: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    author: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    publisher: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    license: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_repo: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    engine: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    default_timeout: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    requires_buz: Option<String>,
}

#[derive(Serialize, Clone)]
struct MetaVersion {
    version: String,
    file: String,
    sha256: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    category: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    description_zh: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    author: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    publisher: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    license: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_repo: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    engine: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    default_timeout: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    requires_buz: Option<String>,
}

fn current_buz() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[derive(Serialize)]
struct IndexFile {
    steps: Vec<IndexEntry>,
}

#[derive(Serialize)]
struct MetaFile {
    id: String,
    versions: Vec<MetaVersion>,
}

#[derive(Serialize)]
struct RevisionSentinelFile {
    revision: String,
    updated_at: Option<String>,
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(bytes))
}

/// Top-level scalar meta, extracted in `META_FIELDS` order. Multi-line
/// values (block scalars) stay out — the full spec remains the source of
/// truth, the index carries browsing scalars only.
fn scalar_meta(spec_value: &serde_yaml::Value) -> Vec<Option<String>> {
    let mut out: Vec<Option<String>> = vec![None; META_FIELDS.len()];
    let serde_yaml::Value::Mapping(mapping) = spec_value else {
        return out;
    };
    for (order, field) in META_FIELDS.iter().enumerate() {
        let Some(serde_yaml::Value::String(raw)) = mapping.get(*field) else {
            continue;
        };
        if raw.contains('\n') || raw.trim().is_empty() {
            continue;
        }
        out[order] = Some(raw.trim().to_string());
    }
    out
}

struct ParsedSpec {
    path: PathBuf,
    namespace: String,
    entry: IndexEntry,
}

fn parse_spec(path: &Path, ns_dir: &Path) -> anyhow::Result<ParsedSpec> {
    let display = path.display().to_string();
    let raw =
        std::fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
    let spec: StepSpec = serde_yaml::from_str(&raw)
        .with_context(|| format!("{}: invalid step spec YAML", path.display()))?;
    validate_step_spec(&spec, current_buz()).map_err(|errors| {
        anyhow::anyhow!(
            "{}: invalid step spec:\n{}",
            path.display(),
            errors
                .iter()
                .map(|error| format!("  - {error}"))
                .collect::<Vec<_>>()
                .join("\n")
        )
    })?;
    if !layout::is_valid_id(&spec.id) {
        anyhow::bail!(
            "{display}: id {:?} must be '<namespace>/<name>' with lowercase [a-z0-9-] parts",
            spec.id
        );
    }
    if !layout::is_semver(&spec.version) {
        anyhow::bail!(
            "{display}: version {:?} must be three-part semver",
            spec.version
        );
    }
    if let Some(requires) = &spec.requires_buz {
        let rest = requires.strip_prefix(">=").unwrap_or(requires);
        if !layout::is_semver(rest) {
            anyhow::bail!("{display}: requires_buz {requires:?} is not '>=X.Y.Z' or exact 'X.Y.Z'");
        }
    }
    let namespace = spec
        .id
        .split('/')
        .next()
        .expect("validated two-part id")
        .to_string();
    // Layout convention: a namespace's specs live under plugins/<ns>/ and
    // `file:` is relative to that directory (the manager's source base URL).
    let rel = path
        .strip_prefix(ns_dir)
        .map_err(|_| {
            anyhow::anyhow!(
                "{display}: namespace '{namespace}' specs must live under {}",
                ns_dir.display()
            )
        })?
        .to_string_lossy()
        .to_string();
    let spec_value: serde_yaml::Value = serde_yaml::from_str(&raw).expect("spec parsed above");
    let meta = scalar_meta(&spec_value);
    let take = |index: usize| meta[index].clone();
    Ok(ParsedSpec {
        path: path.to_path_buf(),
        namespace,
        entry: IndexEntry {
            id: spec.id.clone(),
            version: spec.version.clone(),
            file: rel,
            sha256: sha256_hex(raw.as_bytes()),
            category: spec.category.clone(),
            name: take(0),
            description: take(1),
            description_zh: take(2),
            author: take(3),
            publisher: take(4),
            license: take(5),
            source_repo: take(6),
            engine: take(7),
            default_timeout: take(8),
            requires_buz: take(9),
        },
    })
}

fn root_relative(path: &Path, root: &Path) -> anyhow::Result<String> {
    let display = path.display().to_string();
    path.strip_prefix(root)
        .map(|rel| rel.to_string_lossy().to_string())
        .map_err(|_| anyhow::anyhow!("{display}: path must be inside {}", root.display()))
}

fn emit_json<T: serde::Serialize>(path: &Path, value: &T) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
    }
    std::fs::write(path, format!("{}\n", serde_json::to_string_pretty(value)?))
        .with_context(|| format!("cannot write {}", path.display()))?;
    Ok(())
}

pub fn run(options: &Options) -> anyhow::Result<()> {
    let plugin_dir = std::env::current_dir()
        .context("cannot determine the working directory")?
        .join(&options.plugins_dir);
    let plugin_dir = plugin_dir.canonicalize().with_context(|| {
        format!(
            "plugin directory {} not found",
            options.plugins_dir.display()
        )
    })?;
    let root = plugin_dir.parent().unwrap_or(Path::new(".")).to_path_buf();

    let files = layout::discover_spec_files(&plugin_dir)?;
    if files.is_empty() {
        anyhow::bail!("{}: no *.yaml/*.yml step specs found", plugin_dir.display());
    }

    // First pass: parse + validate every spec with the Manager's save-time
    // gate, then group by id namespace.
    let mut seen: BTreeMap<(String, String), PathBuf> = BTreeMap::new();
    let mut by_ns: BTreeMap<String, Vec<ParsedSpec>> = BTreeMap::new();
    for path in &files {
        // A spec's namespace is only known after parsing; validate against
        // both candidate locations so the error names the convention.
        let parsed = {
            let provisional: StepSpec = serde_yaml::from_str(
                &std::fs::read_to_string(path)
                    .with_context(|| format!("cannot read {}", path.display()))?,
            )
            .with_context(|| format!("{}: invalid step spec YAML", path.display()))?;
            let namespace = provisional.id.split('/').next().unwrap_or("").to_string();
            parse_spec(path, &plugin_dir.join(&namespace))
        };
        let parsed = match parsed {
            Ok(parsed) => parsed,
            Err(first_error) => {
                // Generic location succeeds → the namespace-layout error is
                // the honest one; it fails → its error names the real issue.
                match parse_spec(path, &plugin_dir) {
                    Ok(_) => return Err(first_error),
                    Err(generic_error) => return Err(generic_error),
                }
            }
        };
        let key = (parsed.entry.id.clone(), parsed.entry.version.clone());
        if let Some(previous) = seen.get(&key) {
            anyhow::bail!(
                "{}: duplicate id+version {}@{} (already in {})",
                parsed.path.display(),
                parsed.entry.id,
                parsed.entry.version,
                previous.display()
            );
        }
        seen.insert(key, path.clone());
        by_ns
            .entry(parsed.namespace.clone())
            .or_default()
            .push(parsed);
    }

    let namespaces: Vec<String> = match &options.namespace {
        Some(namespace) => {
            if !by_ns.contains_key(namespace) {
                anyhow::bail!(
                    "{}: no specs for namespace '{namespace}' (found: {})",
                    plugin_dir.display(),
                    by_ns.keys().cloned().collect::<Vec<_>>().join(", ")
                );
            }
            vec![namespace.clone()]
        }
        None => {
            if by_ns.len() > 1 {
                anyhow::bail!(
                    "{}: several namespaces ({}) — one index per namespace; pass --namespace",
                    plugin_dir.display(),
                    by_ns.keys().cloned().collect::<Vec<_>>().join(", ")
                );
            }
            by_ns.keys().cloned().collect()
        }
    };

    for namespace in &namespaces {
        let mut entries: Vec<IndexEntry> = by_ns[namespace]
            .iter()
            .map(|parsed| parsed.entry.clone())
            .collect();
        entries.sort_by(|a, b| (&a.id, &a.version).cmp(&(&b.id, &b.version)));
        let index_path = plugin_dir.join(namespace).join(layout::INDEX_FILE);
        let count = entries.len();
        emit_json(&index_path, &IndexFile { steps: entries })?;
        println!(
            "index.json: {count} step(s) written to {}",
            index_path.display()
        );
    }

    if let Some(merged_path) = &options.merged {
        let mut merged: Vec<IndexEntry> = Vec::new();
        for specs in by_ns.values() {
            for parsed in specs {
                let mut entry = parsed.entry.clone();
                entry.file = root_relative(&parsed.path, &root)?;
                merged.push(entry);
            }
        }
        merged.sort_by(|a, b| (&a.id, &a.version).cmp(&(&b.id, &b.version)));
        let merged_path = if merged_path.is_absolute() {
            merged_path.clone()
        } else {
            root.join(merged_path)
        };
        let merged_count = merged.len();
        emit_json(&merged_path, &IndexFile { steps: merged })?;
        println!(
            "merged index: {merged_count} step(s) written to {}",
            merged_path.display()
        );
    }

    if !options.no_meta {
        let mut metas: BTreeMap<PathBuf, (String, Vec<MetaVersion>)> = BTreeMap::new();
        for specs in by_ns.values() {
            let ns_dir = plugin_dir.join(&specs.first().expect("non-empty").namespace);
            for parsed in specs {
                let rel = parsed
                    .path
                    .strip_prefix(&ns_dir)
                    .map_err(|_| {
                        anyhow::anyhow!(
                            "{}: namespace '{}' specs must live under {}",
                            parsed.path.display(),
                            parsed.namespace,
                            ns_dir.display()
                        )
                    })?
                    .to_string_lossy()
                    .to_string();
                let entry = parsed.entry.clone();
                let version = MetaVersion {
                    version: entry.version,
                    file: rel,
                    sha256: entry.sha256,
                    category: entry.category,
                    name: entry.name,
                    description: entry.description,
                    description_zh: entry.description_zh,
                    author: entry.author,
                    publisher: entry.publisher,
                    license: entry.license,
                    source_repo: entry.source_repo,
                    engine: entry.engine,
                    default_timeout: entry.default_timeout,
                    requires_buz: entry.requires_buz,
                };
                let plugin_dir_path = parsed
                    .path
                    .parent()
                    .expect("spec has a parent")
                    .to_path_buf();
                metas
                    .entry(plugin_dir_path)
                    .or_insert_with(|| (entry.id, Vec::new()))
                    .1
                    .push(version);
            }
        }
        for (plugin_meta_dir, (id, versions)) in &metas {
            let mut versions = versions.clone();
            versions.sort_by(|a, b| a.version.cmp(&b.version));
            let meta_path = plugin_meta_dir.join(layout::META_FILE);
            emit_json(
                &meta_path,
                &MetaFile {
                    id: id.clone(),
                    versions,
                },
            )?;
        }
        println!("meta.json: {} plugin(s) written", metas.len());
    }

    // Namespace revision sentinel — written LAST: mirrors publish content
    // first and the sentinel commits the sync. Skipped without --revision so
    // local runs never fabricate a version marker.
    if let Some(revision) = &options.revision {
        for namespace in &namespaces {
            let sentinel_path = plugin_dir.join(namespace).join(layout::REVISION_FILE);
            emit_json(
                &sentinel_path,
                &RevisionSentinelFile {
                    revision: revision.clone(),
                    updated_at: options.updated_at.clone(),
                },
            )?;
        }
        println!(
            "revision.json: written for {} namespace(s) (revision {revision})",
            namespaces.len()
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    const SPEC: &str = r#"id: alice/oss-upload
version: 1.0.0
publisher: community
author: alice
source_repo: https://github.com/alice/my-buz-steps
engine: shell
requires_buz: ">=1.0.0"
inputs:
  bucket:
    type: string
    required: true
  file:
    type: string
    required: true
run: echo ok
"#;

    fn write_repo(dir: &Path) -> PathBuf {
        let root = dir.join("repo");
        let plugin = root.join("plugins").join("alice").join("oss-upload");
        fs::create_dir_all(&plugin).unwrap();
        fs::write(plugin.join("1.0.0.yaml"), SPEC).unwrap();
        root
    }

    fn sha_of(content: &str) -> String {
        sha256_hex(content.as_bytes())
    }

    #[test]
    fn generates_namespace_index_meta_and_sentinel() {
        let dir = tempfile::tempdir().unwrap();
        let root = write_repo(dir.path());
        let options = Options {
            plugins_dir: root.join("plugins"),
            merged: Some(PathBuf::from("index.json")),
            revision: Some("8d367d8".into()),
            updated_at: Some("2026-10-27T00:00:00Z".into()),
            namespace: None,
            no_meta: false,
        };
        run(&options).unwrap();

        let sha = sha_of(SPEC);
        let expected_index = format!(
            r#"{{
  "steps": [
    {{
      "id": "alice/oss-upload",
      "version": "1.0.0",
      "file": "oss-upload/1.0.0.yaml",
      "sha256": "{sha}",
      "author": "alice",
      "publisher": "community",
      "source_repo": "https://github.com/alice/my-buz-steps",
      "engine": "shell",
      "requires_buz": ">=1.0.0"
    }}
  ]
}}
"#
        );
        assert_eq!(
            fs::read_to_string(root.join("plugins/alice/index.json")).unwrap(),
            expected_index
        );

        let expected_meta = format!(
            r#"{{
  "id": "alice/oss-upload",
  "versions": [
    {{
      "version": "1.0.0",
      "file": "oss-upload/1.0.0.yaml",
      "sha256": "{sha}",
      "author": "alice",
      "publisher": "community",
      "source_repo": "https://github.com/alice/my-buz-steps",
      "engine": "shell",
      "requires_buz": ">=1.0.0"
    }}
  ]
}}
"#
        );
        assert_eq!(
            fs::read_to_string(root.join("plugins/alice/oss-upload/meta.json")).unwrap(),
            expected_meta
        );

        let expected_sentinel = r#"{
  "revision": "8d367d8",
  "updated_at": "2026-10-27T00:00:00Z"
}
"#;
        assert_eq!(
            fs::read_to_string(root.join("plugins/alice/revision.json")).unwrap(),
            expected_sentinel
        );

        // Merged view: repo-root-relative file paths, identical content.
        let merged = fs::read_to_string(root.join("index.json")).unwrap();
        assert!(merged.contains("\"file\": \"plugins/alice/oss-upload/1.0.0.yaml\""));
        assert!(merged.contains("alice/oss-upload"));
    }

    #[test]
    fn rejects_duplicate_id_version() {
        let dir = tempfile::tempdir().unwrap();
        let root = write_repo(dir.path());
        let plugin = root.join("plugins/alice/oss-upload");
        fs::write(plugin.join("1.0.0-dup.yaml"), SPEC).unwrap();
        let error = run(&Options {
            plugins_dir: root.join("plugins"),
            merged: None,
            revision: None,
            updated_at: None,
            namespace: None,
            no_meta: true,
        })
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("duplicate id+version alice/oss-upload@1.0.0"),
            "{error}"
        );
    }

    #[test]
    fn requires_namespace_flag_for_several_namespaces() {
        let dir = tempfile::tempdir().unwrap();
        let root = write_repo(dir.path());
        let other = root.join("plugins/bob/upload");
        fs::create_dir_all(&other).unwrap();
        fs::write(
            other.join("1.0.0.yaml"),
            SPEC.replacen("alice/oss-upload", "bob/upload", 1),
        )
        .unwrap();
        let error = run(&Options {
            plugins_dir: root.join("plugins"),
            merged: None,
            revision: None,
            updated_at: None,
            namespace: None,
            no_meta: true,
        })
        .unwrap_err()
        .to_string();
        assert!(error.contains("several namespaces (alice, bob)"), "{error}");

        // --namespace generates exactly one index.
        run(&Options {
            plugins_dir: root.join("plugins"),
            merged: None,
            revision: None,
            updated_at: None,
            namespace: Some("bob".into()),
            no_meta: true,
        })
        .unwrap();
        assert!(root.join("plugins/bob/index.json").exists());
        assert!(!root.join("plugins/alice/index.json").exists());
    }
}
