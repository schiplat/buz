//! Per-tier backend descriptors shared by both halves of the sandbox protocol.
//!
//! The Runner client and the `buz-sandboxd` supervisor both derive their
//! tier→(image, OCI runtime) view from this module, so adding a backend means
//! extending one table — never re-threading mode matches through client and
//! server code. Prerequisite rules (Linux, `/dev/kvm`) attach to the
//! descriptor, not to call sites.

use std::path::Path;

use shared::protocol::{ExecutionBackendStatus, RunnerExecutionCapabilities, RunnerExecutionMode};

/// How one sandboxed tier is delivered on this host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendDescriptor {
    pub mode: RunnerExecutionMode,
    /// Digest-pinned job image; must provide `sh` and `timeout`.
    pub image: String,
    /// OCI runtime passed as `docker --runtime`; empty uses the daemon default (runc).
    pub oci_runtime: String,
    pub requires_linux: bool,
    pub requires_kvm: bool,
}

impl BackendDescriptor {
    /// Whether the host operator configured this tier at all.
    pub fn is_configured(&self) -> bool {
        !self.image.trim().is_empty()
    }

    /// The configured OCI runtime, if any.
    pub fn runtime(&self) -> Option<&str> {
        let trimmed = self.oci_runtime.trim();
        (!trimmed.is_empty()).then_some(trimmed)
    }
}

/// Per-tier backend table assembled from host configuration; the single
/// lookup both protocol halves use instead of per-mode matches.
#[derive(Debug, Clone, Default)]
pub struct TierBackends {
    entries: Vec<BackendDescriptor>,
}

impl TierBackends {
    /// Builds one descriptor per sandboxed tier, in ladder order.
    pub fn from_tiers<S: AsRef<str>, R: AsRef<str>>(
        tiers: impl IntoIterator<Item = (RunnerExecutionMode, S, R)>,
    ) -> Self {
        Self {
            entries: tiers
                .into_iter()
                .map(|(mode, image, oci_runtime)| {
                    Self::descriptor(mode, image.as_ref(), oci_runtime.as_ref())
                })
                .collect(),
        }
    }

    fn descriptor(mode: RunnerExecutionMode, image: &str, oci_runtime: &str) -> BackendDescriptor {
        let (requires_linux, requires_kvm) = match mode {
            RunnerExecutionMode::Container => (false, false),
            // runsc is Linux-only but needs no hardware virtualization.
            RunnerExecutionMode::Sandbox => (true, false),
            RunnerExecutionMode::Vm => (true, true),
            RunnerExecutionMode::Host => (false, false),
        };
        BackendDescriptor {
            mode,
            image: image.to_string(),
            oci_runtime: oci_runtime.to_string(),
            requires_linux,
            requires_kvm,
        }
    }

    /// The backend for one tier; `None` for Host, which has no sandbox backend.
    pub fn get(&self, mode: RunnerExecutionMode) -> Option<&BackendDescriptor> {
        self.entries.iter().find(|entry| entry.mode == mode)
    }

    /// Configured sandboxed tiers, weakest isolation first — the probe order.
    pub fn configured(&self) -> impl Iterator<Item = &BackendDescriptor> {
        self.entries
            .iter()
            .filter(|entry| entry.is_configured())
            .filter(|entry| entry.mode.is_sandboxed())
    }
}

/// Fails closed unless the host satisfies exactly what this descriptor demands.
///
/// `requested_runtime` is the Runner's locally configured runtime for the tier;
/// the supervisor only proceeds when it matches its own policy, so a Runner can
/// never talk a stricter supervisor into a weaker runtime (or vice versa).
pub fn validate_prerequisites(
    descriptor: &BackendDescriptor,
    os: &str,
    kvm_path: &Path,
    requested_runtime: &str,
) -> anyhow::Result<()> {
    let requested = requested_runtime.trim();
    match descriptor.runtime() {
        Some(configured) => anyhow::ensure!(
            requested == configured,
            "requested runtime '{requested}' for '{}' tier does not match supervisor policy \
             '{configured}'",
            descriptor.mode.as_str()
        ),
        None => anyhow::ensure!(
            requested.is_empty(),
            "requested runtime '{requested}' for '{}' tier does not match supervisor policy \
             (the supervisor configured none)",
            descriptor.mode.as_str()
        ),
    }
    if descriptor.requires_linux {
        anyhow::ensure!(
            os == "linux",
            "'{}' tier requires a Linux host",
            descriptor.mode.as_str()
        );
    }
    if descriptor.requires_kvm {
        anyhow::ensure!(
            kvm_path.exists(),
            "'{}' tier requires an available /dev/kvm device",
            descriptor.mode.as_str()
        );
    }
    Ok(())
}

/// Records one tier's probe outcome into the capability report.
pub fn record_probe(
    capabilities: &mut RunnerExecutionCapabilities,
    descriptor: &BackendDescriptor,
    outcome: anyhow::Result<()>,
) {
    let (status, reason) = match outcome {
        Ok(()) => (ExecutionBackendStatus::Supported, None),
        Err(error) => (ExecutionBackendStatus::Unsupported, Some(error.to_string())),
    };
    capabilities.set_backend(descriptor.mode, status, reason);
    if let Some(runtime) = descriptor.runtime() {
        capabilities
            .runtimes
            .insert(descriptor.mode.as_str().to_string(), runtime.to_string());
    }
}
