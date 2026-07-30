//! Registry credentials for `docker login` (`registry_secret`).
//!
//! YAML / form only stores the **Manager Secret name**. The Secret value (JSON) is
//! encrypted at rest and resolved at job dispatch — passwords never appear in
//! pipeline YAML or the `tasks` table.
//!
//! Used by docker (image pull), shell, and SSH (e.g. `docker push`) engines.

use serde::{Deserialize, Serialize};

/// Well-known providers for future auth strategies (token refresh, etc.).
/// Unknown values are accepted for forward compatibility; today all use
/// static username/password `docker login`.
pub const PROVIDER_ACR_PERSONAL: &str = "acr_personal";
pub const PROVIDER_GENERIC: &str = "generic";

/// Resolved auth passed from Manager → Runner (never injected as container env).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RegistryAuth {
    pub registry: String,
    pub username: String,
    pub password: String,
    /// Optional vendor hint (`acr_personal`, `generic`, …). Ignored by login today.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
}

/// JSON shape stored in Manager Secrets for `registry_secret`.
#[derive(Debug, Clone, Deserialize)]
struct RegistrySecretJson {
    /// Optional vendor: `acr_personal` | `generic` | future values.
    #[serde(default)]
    provider: Option<String>,
    #[serde(default)]
    registry: Option<String>,
    username: String,
    password: String,
}

/// Extract the registry host (and optional port) from a container image reference.
///
/// Returns `None` for Docker Hub short names (`alpine:3.20`, `user/repo:tag`).
pub fn registry_host_from_image(image: &str) -> Option<String> {
    let image = image.trim();
    if image.is_empty() {
        return None;
    }
    let without_digest = image.split('@').next().unwrap_or(image);
    let slash = without_digest.find('/')?;
    let first = &without_digest[..slash];
    if first.contains('.') || first.contains(':') || first.eq_ignore_ascii_case("localhost") {
        Some(first.to_string())
    } else {
        None
    }
}

/// Normalize optional provider string (empty → None).
pub fn normalize_provider(raw: Option<&str>) -> Option<String> {
    raw.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_ascii_lowercase())
}

/// Parse a Secret value JSON into [`RegistryAuth`].
///
/// When `registry` is omitted in JSON, it is inferred from `image` (docker steps).
/// Shell/SSH push steps typically omit `image` and must set `registry` in JSON.
pub fn parse_registry_secret_value(
    raw: &str,
    image: Option<&str>,
) -> Result<RegistryAuth, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("registry secret value is empty".into());
    }

    let parsed: RegistrySecretJson = serde_json::from_str(raw).map_err(|e| {
        format!(
            "registry secret must be JSON with username/password \
             (optional registry, provider): {e}"
        )
    })?;

    let username = parsed.username.trim().to_string();
    let password = parsed.password.trim().to_string();
    if username.is_empty() {
        return Err("registry secret JSON requires non-empty 'username'".into());
    }
    if password.is_empty() {
        return Err("registry secret JSON requires non-empty 'password'".into());
    }

    let provider = normalize_provider(parsed.provider.as_deref());

    let registry = parsed
        .registry
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| image.and_then(registry_host_from_image))
        .ok_or_else(|| {
            "registry secret has no 'registry' and no image host to infer from \
             (set JSON registry, e.g. crpi-xxxx.cn-hangzhou.personal.cr.aliyuncs.com)"
                .to_string()
        })?;

    Ok(RegistryAuth {
        registry,
        username,
        password,
        provider,
    })
}
