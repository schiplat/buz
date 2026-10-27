//! Source-root manifest generation: `plugins/index.json` (+ per-plugin
//! `meta.json` + the `revision.json` freshness sentinel). One source root
//! carries exactly one namespace. Byte-compatible with the Python
//! `gen_index.py` output this command supersedes: sorted entries, ordered
//! fields, two-space JSON, trailing newline.

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

#[derive(Clone)]
pub struct Options {
    pub plugins_dir: PathBuf,
    pub revision: Option<String>,
    pub updated_at: Option<String>,
    pub no_meta: bool,
    /// Verify instead of write: compare what generation would produce with
    /// the manifests on disk and fail on any difference.
    pub check: bool,
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

fn current_buz() -> &'static str {
    env!("CARGO_PKG_VERSION")
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

fn parse_spec(path: &Path, source_root: &Path) -> anyhow::Result<ParsedSpec> {
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
    // `file:` is relative to the source root — the manager joins it against
    // the source base URL (the index URL minus `index.json`).
    let rel = path
        .strip_prefix(source_root)
        .map_err(|_| {
            anyhow::anyhow!(
                "{display}: specs must live under the source root {}",
                source_root.display()
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

/// Serialized manifest text: pretty JSON, declaration-order keys, trailing
/// newline. Byte-deterministic — the whole point of a mirror drift check.
fn render_json<T: serde::Serialize>(value: &T) -> anyhow::Result<String> {
    Ok(format!("{}\n", serde_json::to_string_pretty(value)?))
}

fn write_json(path: &Path, content: &str) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
    }
    std::fs::write(path, content).with_context(|| format!("cannot write {}", path.display()))?;
    Ok(())
}

pub fn run(options: &Options) -> anyhow::Result<()> {
    let source_root = std::env::current_dir()
        .context("cannot determine the working directory")?
        .join(&options.plugins_dir);
    let source_root = source_root.canonicalize().with_context(|| {
        format!(
            "plugin directory {} not found",
            options.plugins_dir.display()
        )
    })?;

    // Plan first, then either write or verify — both paths see the exact same
    // bytes, so `--check` cannot disagree with what generation would write.
    let planned = plan_manifests(&source_root, options)?;

    if options.check {
        verify_manifests(&source_root, &planned, options)?;
    } else {
        for (path, content) in &planned {
            write_json(path, content)?;
        }
        report_written(&planned);
    }
    Ok(())
}

/// Path each manifest the run produces to its exact bytes, in write order
/// (index → per-plugin meta → revision sentinel last).
fn plan_manifests(source_root: &Path, options: &Options) -> anyhow::Result<Vec<(PathBuf, String)>> {
    let files = layout::discover_spec_files(source_root)?;
    if files.is_empty() {
        anyhow::bail!(
            "{}: no *.yaml/*.yml step specs found",
            source_root.display()
        );
    }

    // One source root = one namespace. This repository publishes the official
    // `buz/` namespace; third-party plugins publish from their own
    // repositories with this same layout (the manager rejects indexes that
    // mix namespaces, so the boundary is enforced on consumption too).
    let mut seen: BTreeMap<(String, String), PathBuf> = BTreeMap::new();
    let mut by_ns: BTreeMap<String, Vec<ParsedSpec>> = BTreeMap::new();
    for path in &files {
        let parsed = parse_spec(path, source_root)?;
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
    if by_ns.len() > 1 {
        anyhow::bail!(
            "{}: several namespaces ({}) — one source root per namespace; \
             third-party plugins publish from their own repositories with this same layout",
            source_root.display(),
            by_ns.keys().cloned().collect::<Vec<_>>().join(", ")
        );
    }

    let mut planned: Vec<(PathBuf, String)> = Vec::new();

    let mut entries: Vec<IndexEntry> = by_ns
        .values()
        .flat_map(|specs| specs.iter().map(|parsed| parsed.entry.clone()))
        .collect();
    entries.sort_by(|a, b| (&a.id, &a.version).cmp(&(&b.id, &b.version)));
    planned.push((
        source_root.join(layout::INDEX_FILE),
        render_json(&IndexFile { steps: entries })?,
    ));

    if !options.no_meta {
        let mut metas: BTreeMap<PathBuf, (String, Vec<MetaVersion>)> = BTreeMap::new();
        for specs in by_ns.values() {
            for parsed in specs {
                let rel = parsed
                    .path
                    .strip_prefix(source_root)
                    .map_err(|_| {
                        anyhow::anyhow!(
                            "{}: specs must live under the source root {}",
                            parsed.path.display(),
                            source_root.display()
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
                let plugin_meta_dir = parsed
                    .path
                    .parent()
                    .expect("spec has a parent")
                    .to_path_buf();
                metas
                    .entry(plugin_meta_dir)
                    .or_insert_with(|| (entry.id, Vec::new()))
                    .1
                    .push(version);
            }
        }
        for (plugin_meta_dir, (id, versions)) in &metas {
            let mut versions = versions.clone();
            versions.sort_by(|a, b| a.version.cmp(&b.version));
            planned.push((
                plugin_meta_dir.join(layout::META_FILE),
                render_json(&MetaFile {
                    id: id.clone(),
                    versions,
                })?,
            ));
        }
    }

    // Revision sentinel — always last: mirrors publish content first and the
    // sentinel commits the sync. Skipped without --revision so local runs
    // never fabricate a version marker.
    if let Some(revision) = &options.revision {
        planned.push((
            source_root.join(layout::REVISION_FILE),
            render_json(&RevisionSentinelFile {
                revision: revision.clone(),
                updated_at: options.updated_at.clone(),
            })?,
        ));
    }

    Ok(planned)
}

fn report_written(planned: &[(PathBuf, String)]) {
    for (path, content) in planned {
        // Count entries from the rendered text: one "version" key per step or
        // per plugin version, one sentinel.
        let detail = if content.contains("\"steps\": [") {
            format!("{} step(s)", content.matches("\"version\":").count())
        } else if content.contains("\"versions\": [") {
            format!("{} version(s)", content.matches("\"version\":").count())
        } else {
            "sentinel".to_string()
        };
        println!("wrote {} ({detail})", path.display());
    }
}

/// Compare the planned bytes with what is on disk. Any missing, differing, or
/// stale manifest fails the run: in CI this turns "the committed manifests
/// must equal what the tool produces" into a hard gate.
fn verify_manifests(
    source_root: &Path,
    planned: &[(PathBuf, String)],
    options: &Options,
) -> anyhow::Result<()> {
    let mut problems: Vec<String> = Vec::new();
    for (path, expected) in planned {
        match std::fs::read_to_string(path) {
            Ok(actual) if &actual == expected => {}
            Ok(_) => problems.push(format!(
                "{} is out of date — run `buz-plugin index{}` and commit the result",
                path.display(),
                if options.revision.is_some() {
                    ""
                } else {
                    " --revision <sha>"
                }
            )),
            Err(_) => problems.push(format!(
                "{} is missing — run `buz-plugin index`",
                path.display()
            )),
        }
    }

    // Stale manifests: a plugin directory whose spec was deleted still carries
    // its meta.json, and nothing else would ever remove it.
    let planned_paths: std::collections::BTreeSet<&Path> =
        planned.iter().map(|(p, _)| p.as_path()).collect();
    for entry in walk_manifests(source_root)? {
        let is_sentinel = entry.file_name().and_then(|n| n.to_str()) == Some(layout::REVISION_FILE);
        if is_sentinel && options.revision.is_none() {
            continue; // sentinel not managed without --revision
        }
        if !planned_paths.contains(entry.as_path()) {
            problems.push(format!(
                "{} is stale — its plugin no longer exists; delete it",
                entry.display()
            ));
        }
    }

    if problems.is_empty() {
        println!("manifests up to date ({} file(s) checked)", planned.len());
        return Ok(());
    }
    anyhow::bail!(
        "manifest check failed:\n{}",
        problems
            .iter()
            .map(|p| format!("  - {p}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// Every manifest file currently under the source root.
fn walk_manifests(dir: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> anyhow::Result<()> {
        for entry in
            std::fs::read_dir(dir).with_context(|| format!("cannot read {}", dir.display()))?
        {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('.') {
                continue;
            }
            let path = entry.path();
            let file_type = entry.file_type()?;
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                walk(&path, out)?;
            } else if matches!(
                name.as_ref(),
                layout::INDEX_FILE | layout::META_FILE | layout::REVISION_FILE
            ) {
                out.push(path);
            }
        }
        Ok(())
    }
    walk(dir, &mut out)?;
    out.sort();
    Ok(out)
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

    fn options(root: &Path) -> Options {
        Options {
            plugins_dir: root.join("plugins"),
            revision: None,
            updated_at: None,
            no_meta: false,
            check: false,
        }
    }

    fn fixtures() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
    }

    fn copy_tree(from: &Path, to: &Path) {
        fs::create_dir_all(to).unwrap();
        for entry in fs::read_dir(from).unwrap() {
            let entry = entry.unwrap();
            let target = to.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                copy_tree(&entry.path(), &target);
            } else {
                fs::copy(entry.path(), target).unwrap();
            }
        }
    }

    /// Byte-for-byte comparison against committed golden files. This is the
    /// compatibility contract in one artifact: indentation, key order,
    /// trailing newline, entry sorting, and every field value. A diff here
    /// shows exactly which of those changed, and reviewing the golden file
    /// diff is reviewing the contract.
    #[test]
    fn generated_manifests_match_the_golden_files() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        copy_tree(&fixtures().join("repo"), &repo);
        run(&Options {
            revision: Some("8d367d8".into()),
            updated_at: Some("2026-10-27T00:00:00Z".into()),
            ..options(&repo)
        })
        .unwrap();

        let golden_root = fixtures().join("golden");
        let expected_files = [
            "plugins/index.json",
            "plugins/revision.json",
            "plugins/alice/hello/meta.json",
            "plugins/alice/world/meta.json",
        ];
        for relative in expected_files {
            let expected = fs::read_to_string(golden_root.join(relative))
                .unwrap_or_else(|e| panic!("golden {relative}: {e}"));
            let actual = fs::read_to_string(repo.join(relative))
                .unwrap_or_else(|e| panic!("generated {relative}: {e}"));
            assert_eq!(
                actual, expected,
                "{relative} differs from its golden file — if the change is intended, \
                 update tests/fixtures/golden/{relative}"
            );
        }

        // Nothing else may be produced.
        let mut produced: Vec<String> = walk_manifests(&repo.join("plugins"))
            .unwrap()
            .iter()
            .map(|p| {
                p.strip_prefix(&repo)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        produced.sort();
        let mut expected = expected_files.map(str::to_string).to_vec();
        expected.sort();
        assert_eq!(produced, expected, "unexpected manifest files");
    }

    #[test]
    fn check_passes_on_fresh_output_and_names_what_drifted() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        copy_tree(&fixtures().join("repo"), &repo);
        let mut opts = Options {
            revision: Some("8d367d8".into()),
            updated_at: Some("2026-10-27T00:00:00Z".into()),
            ..options(&repo)
        };
        run(&opts).unwrap();

        // Fresh output → check passes.
        opts.check = true;
        run(&opts).unwrap();

        // A hand-edited manifest → check fails naming that file.
        let index = repo.join("plugins/index.json");
        let tampered = fs::read_to_string(&index).unwrap().replacen(
            "\"version\": \"1.0.0\"",
            "\"version\": \"9.9.9\"",
            1,
        );
        fs::write(&index, tampered).unwrap();
        let error = run(&opts).unwrap_err().to_string();
        assert!(
            error.contains("plugins/index.json is out of date"),
            "{error}"
        );

        // A deleted spec leaves a stale meta.json → check fails naming it too.
        run(&Options {
            check: false,
            ..opts.clone()
        })
        .ok();
        fs::remove_file(repo.join("plugins/alice/world/1.0.0.yaml")).unwrap();
        let error = run(&opts).unwrap_err().to_string();
        assert!(error.contains("alice/world/meta.json is stale"), "{error}");
    }

    #[test]
    fn rejects_duplicate_id_version() {
        let dir = tempfile::tempdir().unwrap();
        let root = write_repo(dir.path());
        let plugin = root.join("plugins/alice/oss-upload");
        fs::write(plugin.join("1.0.0-dup.yaml"), SPEC).unwrap();
        let error = run(&options(&root)).unwrap_err().to_string();
        assert!(
            error.contains("duplicate id+version alice/oss-upload@1.0.0"),
            "{error}"
        );
    }

    #[test]
    fn rejects_multiple_namespaces_in_one_source_root() {
        let dir = tempfile::tempdir().unwrap();
        let root = write_repo(dir.path());
        let other = root.join("plugins/bob/upload");
        fs::create_dir_all(&other).unwrap();
        fs::write(
            other.join("1.0.0.yaml"),
            SPEC.replacen("alice/oss-upload", "bob/upload", 1),
        )
        .unwrap();
        let error = run(&options(&root)).unwrap_err().to_string();
        assert!(error.contains("several namespaces (alice, bob)"), "{error}");
        assert!(error.contains("one source root per namespace"), "{error}");
    }
}
