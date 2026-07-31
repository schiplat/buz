//! Secret kind classification (`generic` | `registry`).

/// Plain / env-injected secret (SSH keys, tokens, passwords as `$NAME`).
pub const SECRET_KIND_GENERIC: &str = "generic";
/// JSON registry credentials for `registry_secret` / `docker login` only.
pub const SECRET_KIND_REGISTRY: &str = "registry";

/// Normalize API/DB kind string. Unknown values fall back to `generic`.
pub fn normalize_secret_kind(raw: Option<&str>) -> &'static str {
    match raw
        .map(str::trim)
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        SECRET_KIND_REGISTRY => SECRET_KIND_REGISTRY,
        _ => SECRET_KIND_GENERIC,
    }
}

/// True when `kind` is the registry credentials kind.
pub fn is_registry_kind(kind: &str) -> bool {
    normalize_secret_kind(Some(kind)) == SECRET_KIND_REGISTRY
}
