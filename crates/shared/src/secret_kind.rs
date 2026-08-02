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

/// A secret `name` must be UPPER_SNAKE_CASE: it is both the injected env key
/// (run block `$NAME`) and the `${secrets.NAME}` template reference, so it must
/// be a valid POSIX env var name in uppercase. `registry` / `aliyun_oss_auth`
/// secrets are referenced by name too (registry_secret / config_source), so the
/// rule applies to every kind.
pub fn validate_secret_name(raw: &str) -> Result<(), String> {
    let name = raw.trim();
    let mut chars = name.chars();
    let valid = matches!(chars.next(), Some(c) if c.is_ascii_uppercase() || c == '_')
        && chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
    if !valid {
        return Err(format!(
            "secret name '{name}' must be UPPER_SNAKE_CASE: start with an uppercase letter or '_', \
             then only uppercase letters, digits, and '_' (e.g. POSTGRES_PASSWORD)"
        ));
    }
    Ok(())
}
