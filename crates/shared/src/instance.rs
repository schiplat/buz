//! Instance identity resolution: env-first, validated, stable across restarts.
//!
//! Five identities name the deployment topology end to end (see the
//! sandbox-host-configuration design record): the Manager instance, the host
//! machine, the Runner deployment instance, the Manager-issued runner auth id,
//! and the sandboxd instance. IDs are public identifiers — never credentials —
//! and they appear in URL paths, logs, audit rows, and job records, so the
//! format is strict: `[A-Za-z0-9][A-Za-z0-9._-]{0,63}`.
//!
//! Resolution order is always **env > persisted state > generated default**.
//! A generated ID is written once and reused; changing an ID changes the
//! identity (a sandboxd needs re-enrollment, a Runner appears as a new
//! deployment), so env overrides must be deliberate.

use std::path::Path;

use uuid::Uuid;

/// Maximum length of an instance identity.
pub const ID_MAX_LEN: usize = 64;

/// Where a resolved identity came from. Callers log this so operators can see
/// whether an ID is pinned by env or fell back to a machine-derived default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdSource {
    /// Set explicitly via the environment variable.
    Env,
    /// Derived from the machine's `/etc/machine-id` (or platform equivalent).
    MachineId,
    /// Read back from a persisted state file written on first boot.
    StateFile,
    /// Generated this boot and persisted by the caller.
    Generated,
    /// Fell back to the OS hostname — works, but cloned machines share it.
    Hostname,
}

impl IdSource {
    pub fn as_str(self) -> &'static str {
        match self {
            IdSource::Env => "env",
            IdSource::MachineId => "machine-id",
            IdSource::StateFile => "state-file",
            IdSource::Generated => "generated",
            IdSource::Hostname => "hostname",
        }
    }
}

/// Validate and normalize an instance identity. Returns the trimmed ID.
pub fn validate(raw: &str) -> Result<String, String> {
    let id = raw.trim();
    if id.is_empty() {
        return Err("instance id must not be empty".into());
    }
    if id.len() > ID_MAX_LEN {
        return Err(format!(
            "instance id must be at most {ID_MAX_LEN} bytes (got {})",
            id.len()
        ));
    }
    let mut chars = id.chars();
    let first = chars.next().expect("non-empty checked above");
    if !first.is_ascii_alphanumeric() {
        return Err(format!(
            "instance id must start with an ASCII letter or digit (found {first:?})"
        ));
    }
    for c in chars {
        if !(c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-') {
            return Err(format!(
                "instance id may only contain ASCII letters, digits, '.', '_' and '-' (found {c:?})"
            ));
        }
    }
    Ok(id.to_string())
}

/// Read and validate an identity from an environment variable.
/// `None` when unset or empty (deployments commonly export empty strings);
/// `Err` when set to something that fails [`validate`].
pub fn from_env(var: &str) -> Result<Option<String>, String> {
    match std::env::var(var) {
        Ok(raw) => {
            let raw = raw.trim().to_string();
            if raw.is_empty() {
                Ok(None)
            } else {
                validate(&raw).map(Some)
            }
        }
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => {
            Err(format!("environment variable {var} is not valid UTF-8"))
        }
    }
}

/// Machine-derived identity: `/etc/machine-id`, the dbus variant, or the
/// macOS `IOPlatformUUID`. `None` when the platform offers none.
pub fn machine_id() -> Option<String> {
    for path in ["/etc/machine-id", "/var/lib/dbus/machine-id"] {
        if let Ok(raw) = std::fs::read_to_string(path) {
            let raw = raw.trim();
            if !raw.is_empty() {
                return Some(raw.to_string());
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        if let Ok(output) = std::process::Command::new("ioreg")
            .args(["-rd1", "-c", "IOPlatformExpertDevice"])
            .output()
        {
            // Line shape: "IOPlatformUUID" = "XXXXXXXX-…"
            let stdout = String::from_utf8_lossy(&output.stdout);
            for line in stdout.lines() {
                if let Some(value) = line.split('"').nth(3) {
                    let value = value.trim();
                    if line.contains("IOPlatformUUID") && !value.is_empty() {
                        return Some(value.to_string());
                    }
                }
            }
        }
    }
    None
}

/// OS hostname as a last-resort identity source.
pub fn hostname() -> Option<String> {
    if let Ok(output) = std::process::Command::new("hostname").output() {
        let raw = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !raw.is_empty() {
            return Some(raw);
        }
    }
    None
}

/// Resolve a host identity: `env_var` > machine-id > hostname. The hostname
/// fallback is deliberate but flagged via [`IdSource::Hostname`] so callers
/// warn (cloned VMs share `/etc/machine-id` and hostnames alike; the design
/// record tells container/VM templates to set the env var).
pub fn resolve_host_id(env_var: &str) -> Result<(String, IdSource), String> {
    if let Some(id) = from_env(env_var)? {
        return Ok((id, IdSource::Env));
    }
    if let Some(id) = machine_id() {
        return validate(&id).map(|id| (id, IdSource::MachineId));
    }
    if let Some(id) = hostname() {
        return validate(&id).map(|id| (id, IdSource::Hostname));
    }
    Err(format!(
        "no host identity available: set {env_var} explicitly on this machine"
    ))
}

/// Resolve a process/deployment identity with persistence:
/// `env_var` > state file > generate `{prefix}-{uuid}` and persist.
///
/// The state file lives in the caller's state directory (Runner state volume,
/// Manager DB is handled separately by the Manager itself). Write failures are
/// an error: an identity that silently changes every boot breaks correlation.
pub fn ensure_persistent(
    state_dir: &Path,
    filename: &str,
    env_var: &str,
    prefix: &str,
) -> Result<(String, IdSource), String> {
    if let Some(id) = from_env(env_var)? {
        return Ok((id, IdSource::Env));
    }
    let path = state_dir.join(filename);
    if let Ok(raw) = std::fs::read_to_string(&path) {
        let raw = raw.trim();
        if !raw.is_empty() {
            return validate(raw).map(|id| (id, IdSource::StateFile));
        }
    }
    let id = validate(&format!("{prefix}-{}", Uuid::new_v4()))?;
    std::fs::create_dir_all(state_dir)
        .map_err(|e| format!("cannot create state dir {}: {e}", state_dir.display()))?;
    let tmp = state_dir.join(format!(".{filename}.{}", Uuid::new_v4().simple()));
    std::fs::write(&tmp, &id).map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&tmp, &path).map_err(|e| format!("cannot persist {filename}: {e}"))?;
    Ok((id, IdSource::Generated))
}
