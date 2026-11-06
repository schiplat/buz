//! Secret kind classification (`generic`).
//!
//! The legacy kinds are gone: `registry` (JSON credentials for the removed
//! `registry_secret` slot, retired) and `aliyun_oss_auth` (OSS deliver creds,
//! removed in the 2.x retirement with the kernel config.source path). Existing rows keep
//! their stored kind value and normalize to `generic`.

/// Plain / env-injected secret (SSH keys, tokens, passwords as `$NAME`).
pub const SECRET_KIND_GENERIC: &str = "generic";

/// Normalize API/DB kind string. Every value (including the legacy
/// `registry` / `aliyun_oss_auth` kinds on pre-retirement rows) resolves to `generic`.
pub fn normalize_secret_kind(_raw: Option<&str>) -> &'static str {
    SECRET_KIND_GENERIC
}

/// A secret `name` must be UPPER_SNAKE_CASE: it is both the injected env key
/// (run block `$NAME`) and the `${secrets.NAME}` template reference, so it must
/// be a valid POSIX env var name in uppercase.
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
