//! Source-root manifest generation: `plugins/index.json` (+ per-plugin
//! `meta.json` + the `revision.json` freshness sentinel). One source root
//! carries exactly one namespace. Byte-compatible with the Python
//! `gen_index.py` output this command supersedes: sorted entries, ordered
//! fields, two-space JSON, trailing newline.

use crate::layout;
use anyhow::Context;
use serde::Serialize;
use shared::plugin_spec::{validate_plugin_spec, PluginSpec};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Index meta is deliberately scalar-only and in this exact field order —
/// the manager persists these onto pool rows so the dashboard can describe
/// a plugin without downloading the template.
const META_FIELDS: [&str; 11] = [
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
    "deprecated",
];

#[derive(Clone)]
pub struct Options {
    pub plugins_dir: PathBuf,
    pub revision: Option<String>,
    pub updated_at: Option<String>,
    /// Pin the catalog sequence instead of deriving it from the committed
    /// index. Publishing pipelines that keep their own counter use this.
    pub sequence: Option<u64>,
    pub no_meta: bool,
    /// Verify instead of write: compare what generation would produce with
    /// the manifests on disk and fail on any difference.
    pub check: bool,
    /// Sign `index.json` with this keypair file (from `buz-plugin keygen`).
    pub sign_key: Option<PathBuf>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    deprecated: Option<String>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    deprecated: Option<String>,
}

/// The signed catalog header. `sequence` is the anti-rollback anchor: a
/// Manager that already accepted sequence N refuses any index claiming less
/// than N, so replaying an older (validly signed) catalog is detected. It
/// lives inside `index.json` — the bytes `index.json.sig` covers — because a
/// number stored next to the signature could be raised without breaking it.
#[derive(Serialize)]
struct IndexFile {
    sequence: u64,
    issued_at: String,
    steps: Vec<IndexEntry>,
}

/// The anti-rollback part of an index already on disk.
#[derive(serde::Deserialize, Default)]
struct PreviousIndex {
    #[serde(default)]
    sequence: Option<u64>,
    #[serde(default)]
    issued_at: Option<String>,
    #[serde(default)]
    steps: serde_json::Value,
}

/// The header generation decided on: keep it byte-stable when the entries did
/// not change, otherwise advance the sequence.
fn catalog_header(
    source_root: &Path,
    entries: &[IndexEntry],
    options: &Options,
) -> anyhow::Result<(u64, String)> {
    let index_path = source_root.join(layout::INDEX_FILE);
    let previous: Option<PreviousIndex> = std::fs::read_to_string(&index_path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok());
    let planned_steps = serde_json::to_value(entries).context("cannot serialize index entries")?;

    // No entry changed and the committed index already carries a sequence:
    // keep sequence AND issue time so a no-op regeneration produces the same
    // bytes — and therefore the same signature. `--sequence` is a re-issue
    // and never takes this path.
    if options.sequence.is_none() {
        if let Some(prev) = previous.as_ref().filter(|prev| prev.steps == planned_steps) {
            if let Some(sequence) = prev.sequence {
                let issued_at = prev.issued_at.clone().unwrap_or_else(now_rfc3339);
                return Ok((sequence, issued_at));
            }
        }
    }

    let previous_sequence = previous
        .as_ref()
        .and_then(|prev| prev.sequence)
        .unwrap_or(0);
    let sequence = options.sequence.unwrap_or(previous_sequence + 1);
    let issued_at = options.updated_at.clone().unwrap_or_else(now_rfc3339);
    Ok((sequence, issued_at))
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
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
    let spec: PluginSpec = serde_yaml::from_str(&raw)
        .with_context(|| format!("{}: invalid plugin spec YAML", path.display()))?;
    validate_plugin_spec(&spec, current_buz()).map_err(|errors| {
        anyhow::anyhow!(
            "{}: invalid plugin spec:\n{}",
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
            deprecated: take(10),
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
    if options.sequence == Some(0) {
        anyhow::bail!("--sequence must be at least 1 (0 never names a catalog version)");
    }
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

    if let Some(key_path) = &options.sign_key {
        let key_path = if key_path.is_absolute() {
            key_path.clone()
        } else {
            std::env::current_dir()?.join(key_path)
        };
        let key: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(&key_path)
                .with_context(|| format!("cannot read {}", key_path.display()))?,
        )
        .with_context(|| format!("{} is not a keypair document", key_path.display()))?;
        let secret = key["secret_key"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("{} has no `secret_key`", key_path.display()))?;
        let key_id = key["key_id"].as_str().unwrap_or("default");
        let index_content = planned
            .iter()
            .find(|(path, _)| path == &source_root.join(layout::INDEX_FILE))
            .map(|(_, content)| content.as_str())
            .ok_or_else(|| anyhow::anyhow!("index.json was not planned"))?;
        sign_one(
            &source_root,
            shared::plugin_signature::SIGNATURE_FILE,
            index_content.as_bytes(),
            secret,
            key_id,
            options.check,
        )?;
        // The review ledger is trusted under the same trust anchor as the
        // index, so a source that publishes one signs it in the same run —
        // otherwise the badges could be forged by rewriting the ledger alone.
        let reviews_path = source_root.join(layout::REVIEWS_FILE);
        match std::fs::read_to_string(&reviews_path) {
            Ok(ledger) => sign_one(
                &source_root,
                shared::plugin_signature::REVIEWS_SIGNATURE_FILE,
                ledger.as_bytes(),
                secret,
                key_id,
                options.check,
            )?,
            Err(error) if reviews_path.exists() => {
                return Err(error)
                    .with_context(|| format!("cannot read {}", reviews_path.display()));
            }
            Err(_) => {}
        }
    } else if options.check {
        // An existing signature must not survive a manifest change.
        for name in [
            shared::plugin_signature::SIGNATURE_FILE,
            shared::plugin_signature::REVIEWS_SIGNATURE_FILE,
        ] {
            let sig_path = source_root.join(name);
            if sig_path.exists() {
                println!(
                    "note: {} exists but --sign-key was not given — signature not re-checked",
                    sig_path.display()
                );
            }
        }
    }
    Ok(())
}

/// Sign (or, with `check`, re-verify) one document next to the source root.
fn sign_one(
    source_root: &Path,
    signature_file: &str,
    document: &[u8],
    secret: &str,
    key_id: &str,
    check: bool,
) -> anyhow::Result<()> {
    let signature = shared::plugin_signature::sign_document(document, secret, key_id)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let sig_path = source_root.join(signature_file);
    if check {
        let existing = std::fs::read_to_string(&sig_path).unwrap_or_default();
        if existing != signature {
            anyhow::bail!(
                "{} is out of date — re-run without --check to sign",
                sig_path.display()
            );
        }
        println!("signature up to date ({})", sig_path.display());
    } else {
        write_json(&sig_path, &signature)?;
        println!("signed {} (key id: {key_id})", sig_path.display());
    }
    Ok(())
}

/// `deprecations.json` at the source root: metadata-only flags for versions
/// whose YAML must stay byte-identical. Editing a published spec would change
/// its content hash and break `@sha256:` pins, so deprecation is carried by
/// the generated manifests instead.
#[derive(serde::Deserialize, Default)]
struct Deprecations {
    #[serde(default)]
    deprecations: Vec<Deprecation>,
}

#[derive(serde::Deserialize, Clone)]
struct Deprecation {
    id: String,
    version: String,
    reason: String,
}

fn load_deprecations(source_root: &Path) -> anyhow::Result<Vec<Deprecation>> {
    let path = source_root.join("deprecations.json");
    if !path.exists() {
        return Ok(Vec::new());
    }
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("cannot read {}", path.display()))?;
    let parsed: Deprecations = serde_json::from_str(&raw)
        .with_context(|| format!("{}: invalid deprecations JSON", path.display()))?;
    let mut out: Vec<Deprecation> = Vec::with_capacity(parsed.deprecations.len());
    let mut seen: std::collections::BTreeSet<(String, String)> = std::collections::BTreeSet::new();
    for entry in parsed.deprecations {
        if entry.reason.trim().is_empty() {
            anyhow::bail!(
                "{}: {}@{} has an empty reason",
                path.display(),
                entry.id,
                entry.version
            );
        }
        if !seen.insert((entry.id.clone(), entry.version.clone())) {
            anyhow::bail!(
                "{}: duplicate deprecation for {}@{}",
                path.display(),
                entry.id,
                entry.version
            );
        }
        out.push(entry);
    }
    Ok(out)
}

/// Path each manifest the run produces to its exact bytes, in write order
/// (index → per-plugin meta → revision sentinel last).
fn plan_manifests(source_root: &Path, options: &Options) -> anyhow::Result<Vec<(PathBuf, String)>> {
    let files = layout::discover_spec_files(source_root)?;
    if files.is_empty() {
        anyhow::bail!(
            "{}: no *.yaml/*.yml plugin specs found",
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

    // Metadata overlay: deprecation flags live outside the spec so published
    // content (and its hash) stays immutable. Applied to the parsed specs so
    // both the index and the per-plugin meta carry it. An overlay entry that
    // matches nothing is a typo — fail rather than silently never applying it.
    let deprecations = load_deprecations(source_root)?;
    let known: std::collections::BTreeSet<(String, String)> = by_ns
        .values()
        .flatten()
        .map(|parsed| (parsed.entry.id.clone(), parsed.entry.version.clone()))
        .collect();
    let unknown: Vec<String> = deprecations
        .iter()
        .filter(|flag| !known.contains(&(flag.id.clone(), flag.version.clone())))
        .map(|flag| format!("{}@{}", flag.id, flag.version))
        .collect();
    if !unknown.is_empty() {
        anyhow::bail!(
            "{}: deprecation entries match no published version: {}",
            source_root.join("deprecations.json").display(),
            unknown.join(", ")
        );
    }
    for specs in by_ns.values_mut() {
        for parsed in specs.iter_mut() {
            if let Some(flag) = deprecations
                .iter()
                .find(|flag| flag.id == parsed.entry.id && flag.version == parsed.entry.version)
            {
                if parsed.entry.deprecated.is_none() {
                    parsed.entry.deprecated = Some(flag.reason.trim().to_string());
                }
            }
        }
    }

    let mut entries: Vec<IndexEntry> = by_ns
        .values()
        .flat_map(|specs| specs.iter().map(|parsed| parsed.entry.clone()))
        .collect();

    entries.sort_by(|a, b| (&a.id, &a.version).cmp(&(&b.id, &b.version)));
    let (sequence, issued_at) = catalog_header(source_root, &entries, options)?;
    planned.push((
        source_root.join(layout::INDEX_FILE),
        render_json(&IndexFile {
            sequence,
            issued_at,
            steps: entries,
        })?,
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
                    deprecated: entry.deprecated,
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
        // Count entries from the rendered text: one "version" key per entry or
        // per plugin version, one sentinel.
        let detail = if content.contains("\"steps\": [") {
            format!(
                "{} plugin version(s)",
                content.matches("\"version\":").count()
            )
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
pub fn walk_manifests(dir: &Path) -> anyhow::Result<Vec<PathBuf>> {
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
