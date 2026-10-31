//! Validation tests for `manager::services::sandbox_config` documents.

use manager::services::sandbox_config::{
    self, ResourceProfile, SandboxHostDocument, TierBackend, DEFAULT_PROFILE,
};
use std::collections::BTreeMap;

fn base_doc() -> SandboxHostDocument {
    let mut profiles = BTreeMap::new();
    profiles.insert(
        DEFAULT_PROFILE.to_string(),
        ResourceProfile {
            memory: "2g".into(),
            cpu: "2".into(),
            pids: 256,
            slots: 1,
        },
    );
    SandboxHostDocument {
        profiles,
        exclude_profiles: Default::default(),
        tiers: BTreeMap::new(),
        network: "bridge".into(),
    }
}

#[test]
fn accepts_a_minimal_valid_document() {
    let doc = base_doc();
    let (normalized, warnings) = sandbox_config::validate(&doc).expect("valid");
    assert_eq!(normalized.network, "bridge");
    assert_eq!(normalized.profiles[DEFAULT_PROFILE].memory, "2g");
    assert!(warnings.is_empty());
}

#[test]
fn accepts_a_selection_only_document_without_default() {
    // Host documents are a pure selection: an empty profiles section inherits
    // the platform catalog (including the implicit 'default' fallback), so
    // neither a default row nor a network value is required any more.
    let doc = SandboxHostDocument {
        profiles: BTreeMap::new(),
        exclude_profiles: Default::default(),
        tiers: BTreeMap::new(),
        network: String::new(),
    };
    let (normalized, warnings) = sandbox_config::validate(&doc).expect("valid");
    assert!(normalized.profiles.is_empty());
    assert_eq!(normalized.network, "");
    assert!(warnings.is_empty());
}

#[test]
fn rejects_excluding_the_default_profile_and_bad_exclusion_names() {
    let mut doc = base_doc();
    doc.exclude_profiles.insert(DEFAULT_PROFILE.into());
    doc.exclude_profiles.insert("GPU-8".into());
    let problems = sandbox_config::validate(&doc).unwrap_err();
    let joined = problems.join("; ");
    assert!(
        joined.contains("'default' profile cannot be excluded"),
        "{joined}"
    );
    assert!(joined.contains("profile name 'GPU-8'"), "{joined}");
}

#[test]
fn normalizes_valid_exclusions() {
    let mut doc = base_doc();
    doc.exclude_profiles.insert("  gpu-8  ".into());
    let (normalized, _) = sandbox_config::validate(&doc).expect("valid");
    assert!(normalized.exclude_profiles.contains("gpu-8"));
    assert_eq!(normalized.exclude_profiles.len(), 1);
}

#[test]
fn rejects_invalid_profile_names_and_values() {
    let mut doc = base_doc();
    doc.profiles.insert(
        "Large!".into(),
        ResourceProfile {
            memory: "eight-gigs".into(),
            cpu: "-1".into(),
            pids: 0,
            slots: 0,
        },
    );
    let problems = sandbox_config::validate(&doc).unwrap_err();
    let joined = problems.join("; ");
    assert!(joined.contains("profile name 'Large!'"), "{joined}");
    assert!(joined.contains("memory"), "{joined}");
    assert!(joined.contains("cpu"), "{joined}");
    assert!(joined.contains("pids"), "{joined}");
    assert!(joined.contains("slots"), "{joined}");
}

#[test]
fn warns_when_default_profile_is_heavy() {
    let mut doc = base_doc();
    doc.profiles.get_mut(DEFAULT_PROFILE).unwrap().slots = 4;
    let (_, warnings) = sandbox_config::validate(&doc).expect("heavy default saves with warning");
    assert!(
        warnings.iter().any(|p| p.contains("default")),
        "expected a default-slots warning, got: {warnings:?}"
    );
}

#[test]
fn rejects_unknown_or_unpinned_tiers() {
    let mut doc = base_doc();
    doc.tiers.insert(
        "container".into(),
        TierBackend {
            image: "debian:latest".into(),
            runtime: String::new(),
        },
    );
    doc.tiers.insert(
        "hypervisor".into(),
        TierBackend {
            image: "repo@sha256:abc".into(),
            runtime: "kata-fc".into(),
        },
    );
    let problems = sandbox_config::validate(&doc).unwrap_err();
    let joined = problems.join("; ");
    assert!(joined.contains("must be pinned by sha256"), "{joined}");
    assert!(joined.contains("tier 'hypervisor' is unknown"), "{joined}");
}

#[test]
fn accepts_all_known_tiers_when_digest_pinned() {
    let mut doc = base_doc();
    for name in ["container", "sandbox", "vm"] {
        doc.tiers.insert(
            name.into(),
            TierBackend {
                image: "repo@sha256:abc".into(),
                runtime: String::new(),
            },
        );
    }
    sandbox_config::validate(&doc).expect("all known tiers accepted");
}

#[test]
fn canonical_json_is_stable_across_rewrites() {
    let doc = base_doc();
    let first = sandbox_config::canonical_json(&doc).to_string();
    let second = sandbox_config::canonical_json(&doc).to_string();
    assert_eq!(first, second);
    let hash = sandbox_config::sha256_hex(first.as_bytes());
    assert_eq!(hash.len(), 64);
}
