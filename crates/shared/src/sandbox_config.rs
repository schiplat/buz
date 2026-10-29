//! Manager-managed sandbox host configuration documents.
//!
//! One desired document per `(host_id, sandboxd_instance_id)` — see the
//! sandbox-host-configuration design record. The Manager is the single source
//! of truth; sandboxd pulls, validates, and applies it atomically. This module
//! owns the typed document and the validation both the Manager ops API (on
//! write) and `buz-sandboxd` (on apply) run — one implementation, two callers.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Reserved profile name every team can use implicitly; operators configure
/// its values but no other profile may take the name.
pub const DEFAULT_PROFILE: &str = "default";

/// Upper bound for `slots`, mirroring the `max_concurrent_jobs` clamp range.
pub const MAX_SLOTS: u32 = 32;

/// One named resource envelope. Resources only — never an image, runtime,
/// tier, network, or mount (those belong to the tiers section).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ResourceProfile {
    /// Docker `--memory` value (`2g`, `512m`, `262144`).
    pub memory: String,
    /// Docker `--cpus` value (positive number; `1.5` = one and a half cores).
    pub cpu: String,
    /// Docker `--pids-limit` value.
    pub pids: u32,
    /// Scheduling weight on the host's slot budget (not a cgroup setting).
    pub slots: u32,
}

/// One tier's delivery: a digest-pinned job image plus the OCI runtime that
/// delivers the isolation boundary (empty runtime = daemon default).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TierBackend {
    pub image: String,
    #[serde(default)]
    pub runtime: String,
}

/// The whole desired document for one sandboxd instance.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct SandboxHostDocument {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub profiles: BTreeMap<String, ResourceProfile>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tiers: BTreeMap<String, TierBackend>,
    /// Docker network for job containers (`bridge` unless configured).
    #[serde(default)]
    pub network: String,
}

/// Profile name rule shared by the document validator, the ops grant API,
/// and pipeline YAML (`runner.resources`): `[a-z0-9][a-z0-9-]{0,31}`.
pub fn validate_profile_name(name: &str) -> Result<(), String> {
    let name = name.trim();
    let valid = !name.is_empty()
        && name.len() <= 32
        && name
            .chars()
            .enumerate()
            .all(|(i, c)| c.is_ascii_digit() || c.is_ascii_lowercase() || (c == '-' && i > 0));
    if valid {
        Ok(())
    } else {
        Err(format!(
            "profile name '{name}' is invalid (expected [a-z0-9][a-z0-9-]{{0,31}})"
        ))
    }
}

/// Validate the whole document. `Ok` returns a normalized copy plus
/// **warnings** (saved, but shown to the operator); `Err` lists hard problems
/// — the ops API rejects the write, and sandboxd refuses to apply.
pub fn validate(
    doc: &SandboxHostDocument,
) -> Result<(SandboxHostDocument, Vec<String>), Vec<String>> {
    let mut problems = Vec::new();
    let mut warnings = Vec::new();

    if doc.profiles.is_empty() {
        problems.push("profiles must define at least the 'default' profile".into());
    }
    if !doc.profiles.contains_key(DEFAULT_PROFILE) {
        problems.push(format!(
            "profiles must define the reserved '{DEFAULT_PROFILE}' profile"
        ));
    }
    let mut normalized_profiles = BTreeMap::new();
    for (name, profile) in &doc.profiles {
        let name = name.trim();
        if name != DEFAULT_PROFILE {
            if let Err(problem) = validate_profile_name(name) {
                problems.push(problem);
            }
        }
        if let Err(problem) = validate_memory(&profile.memory) {
            problems.push(format!("profile '{name}': {problem}"));
        }
        if let Err(problem) = validate_cpu(&profile.cpu) {
            problems.push(format!("profile '{name}': {problem}"));
        }
        if profile.pids == 0 {
            problems.push(format!("profile '{name}': pids must be at least 1"));
        }
        if !(1..=MAX_SLOTS).contains(&profile.slots) {
            problems.push(format!(
                "profile '{name}': slots must be between 1 and {MAX_SLOTS}"
            ));
        }
        normalized_profiles.insert(
            name.to_string(),
            ResourceProfile {
                memory: profile.memory.trim().to_string(),
                cpu: profile.cpu.trim().to_string(),
                pids: profile.pids,
                slots: profile.slots,
            },
        );
    }
    if doc
        .profiles
        .get(DEFAULT_PROFILE)
        .is_some_and(|p| p.slots > 1)
    {
        // Warning, not rejection: every job without `resources` consumes the
        // default envelope, so a heavy default quietly eats the slot budget.
        warnings.push(
            "the 'default' profile consumes more than one slot per job — \
             every job that omits runner.resources will be billed against it"
                .into(),
        );
    }

    let mut normalized_tiers = BTreeMap::new();
    for (name, tier) in &doc.tiers {
        let name = name.trim();
        if !matches!(name, "container" | "sandbox" | "vm") {
            problems.push(format!(
                "tier '{name}' is unknown (expected container, sandbox, or vm)"
            ));
        }
        let image = tier.image.trim();
        if image.is_empty() {
            problems.push(format!(
                "tier '{name}': image must be a digest-pinned reference (repo@sha256:…)"
            ));
        } else if !image.contains("@sha256:") {
            problems.push(format!(
                "tier '{name}': image must be pinned by sha256 digest (repo@sha256:…)"
            ));
        }
        normalized_tiers.insert(
            name.to_string(),
            TierBackend {
                image: image.to_string(),
                runtime: tier.runtime.trim().to_string(),
            },
        );
    }

    if doc.network.trim().is_empty() {
        problems.push("network must not be empty (use 'bridge' unless configured)".into());
    }

    if problems.is_empty() {
        Ok((
            SandboxHostDocument {
                profiles: normalized_profiles,
                tiers: normalized_tiers,
                network: doc.network.trim().to_string(),
            },
            warnings,
        ))
    } else {
        Err(problems)
    }
}

/// Docker accepts a bare byte count or a suffixed value (`b`, `k`, `m`, `g`).
fn validate_memory(raw: &str) -> Result<(), String> {
    let value = raw.trim();
    let (digits, unit) = value.split_at(
        value
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(value.len()),
    );
    if digits.is_empty() {
        return Err(format!(
            "memory '{value}' must be a positive size (e.g. 2g, 512m)"
        ));
    }
    if !matches!(unit, "" | "b" | "k" | "m" | "g") {
        return Err(format!(
            "memory '{value}' must end in b/k/m/g or be a plain byte count"
        ));
    }
    Ok(())
}

/// Docker `--cpus`: a positive decimal number of cores.
fn validate_cpu(raw: &str) -> Result<(), String> {
    let value = raw.trim();
    let parsed: f64 = value
        .parse()
        .map_err(|_| format!("cpu '{value}' must be a positive number of cores (e.g. 2, 1.5)"))?;
    if !parsed.is_finite() || parsed <= 0.0 {
        return Err(format!("cpu '{value}' must be greater than 0"));
    }
    Ok(())
}

/// Canonical on-the-wire form: the serialized typed document. Field order is
/// deterministic (BTreeMap + struct order), so the sha256 recorded with each
/// version is stable across re-writes of the same content.
pub fn canonical_json(doc: &SandboxHostDocument) -> serde_json::Value {
    serde_json::to_value(doc).unwrap_or(serde_json::Value::Null)
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(bytes))
}
