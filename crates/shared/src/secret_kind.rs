//! Secret kind classification (`generic` | `registry` | `aliyun_oss_auth`).

/// Plain / env-injected secret (SSH keys, tokens, passwords as `$NAME`).
pub const SECRET_KIND_GENERIC: &str = "generic";
/// JSON registry credentials for `registry_secret` / `docker login` only.
pub const SECRET_KIND_REGISTRY: &str = "registry";
/// JSON Aliyun OSS read-only credentials for `config_source` / signed-URL downloads.
pub const SECRET_KIND_ALIYUN_OSS_AUTH: &str = "aliyun_oss_auth";

/// Normalize API/DB kind string. Unknown values fall back to `generic`.
pub fn normalize_secret_kind(raw: Option<&str>) -> &'static str {
    match raw
        .map(str::trim)
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        SECRET_KIND_REGISTRY => SECRET_KIND_REGISTRY,
        SECRET_KIND_ALIYUN_OSS_AUTH => SECRET_KIND_ALIYUN_OSS_AUTH,
        _ => SECRET_KIND_GENERIC,
    }
}

/// True when `kind` is the registry credentials kind.
pub fn is_registry_kind(kind: &str) -> bool {
    normalize_secret_kind(Some(kind)) == SECRET_KIND_REGISTRY
}

/// True when `kind` is the Aliyun OSS read-only credentials kind (`config_source`).
pub fn is_aliyun_oss_auth_kind(kind: &str) -> bool {
    normalize_secret_kind(Some(kind)) == SECRET_KIND_ALIYUN_OSS_AUTH
}
