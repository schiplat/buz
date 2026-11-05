use reqwest::header::{HeaderValue, ACCEPT, AUTHORIZATION, RANGE, WWW_AUTHENTICATE};
use reqwest::{Client, Method, Response, StatusCode};
use serde::Deserialize;
use std::time::Duration;
use url::Url;

const MANIFEST_ACCEPT: &str = concat!(
    "application/vnd.oci.image.index.v1+json, ",
    "application/vnd.oci.image.manifest.v1+json, ",
    "application/vnd.docker.distribution.manifest.list.v2+json, ",
    "application/vnd.docker.distribution.manifest.v2+json, ",
    "application/vnd.docker.distribution.manifest.v1+prettyjws"
);
const MAX_TOKEN_RESPONSE_BYTES: usize = 64 * 1024;
const MAX_MANIFEST_FALLBACK_BYTES: u64 = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageReference {
    pub registry: String,
    pub repository: String,
    pub reference: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImageAvailability {
    Exists,
    NotFound,
    Unverified(String),
}

impl ImageAvailability {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Exists => "exists",
            Self::NotFound => "not found",
            Self::Unverified(_) => "unverified",
        }
    }

    pub fn detail(&self) -> Option<&str> {
        match self {
            Self::Unverified(detail) => Some(detail),
            Self::Exists | Self::NotFound => None,
        }
    }

    pub fn is_not_found(&self) -> bool {
        matches!(self, Self::NotFound)
    }
}

#[derive(Debug)]
pub struct BearerChallenge {
    pub realm: String,
    pub service: Option<String>,
    pub scope: Option<String>,
}

#[derive(Deserialize)]
struct TokenResponse {
    token: Option<String>,
    access_token: Option<String>,
}

impl ImageReference {
    pub fn parse(raw: &str) -> Result<Self, String> {
        let raw = raw.trim();
        if raw.is_empty() || raw.contains(char::is_whitespace) {
            return Err("image reference is empty or contains whitespace".into());
        }

        let (name, reference) = if let Some((name, digest)) = raw.split_once('@') {
            let Some(hex) = digest.strip_prefix("sha256:") else {
                return Err("only sha256 image digests are supported".into());
            };
            if hex.len() != 64 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err("image sha256 digest must contain 64 hexadecimal characters".into());
            }
            (name, format!("sha256:{}", hex.to_ascii_lowercase()))
        } else {
            let last_slash = raw.rfind('/').unwrap_or(0);
            match raw.rfind(':') {
                Some(colon) if colon > last_slash => {
                    let tag = &raw[colon + 1..];
                    if tag.is_empty()
                        || tag.len() > 128
                        || !tag
                            .bytes()
                            .all(|byte| byte.is_ascii_alphanumeric() || b"_.-".contains(&byte))
                    {
                        return Err("image tag is invalid".into());
                    }
                    (&raw[..colon], tag.to_string())
                }
                _ => (raw, "latest".to_string()),
            }
        };

        if name.is_empty() || name.contains('@') {
            return Err("image name is invalid".into());
        }
        let mut parts = name.split('/').collect::<Vec<_>>();
        if parts.iter().any(|part| part.is_empty()) {
            return Err("image repository contains an empty path component".into());
        }
        let first = parts[0];
        let explicit_registry = first.contains('.') || first.contains(':') || first == "localhost";
        let registry = if explicit_registry {
            parts.remove(0).to_ascii_lowercase()
        } else {
            "docker.io".to_string()
        };
        if registry.contains('@') || registry.contains(' ') || registry.contains('/') {
            return Err("image registry host is invalid".into());
        }
        let mut repository = parts.join("/");
        if repository.is_empty() {
            return Err("image repository is missing".into());
        }
        if !repository.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"/_.-".contains(&byte)
        }) {
            return Err("image repository must use lowercase Docker name characters".into());
        }
        if registry == "docker.io" && !repository.contains('/') {
            repository = format!("library/{repository}");
        }
        Ok(Self {
            registry,
            repository,
            reference,
        })
    }

    pub fn registry_base(&self) -> Result<Url, String> {
        let host = match self.registry.as_str() {
            "docker.io" | "index.docker.io" => "registry-1.docker.io",
            other => other,
        };
        Url::parse(&format!("https://{host}/"))
            .map_err(|_| "image registry host is not a valid HTTPS host".to_string())
            .and_then(|url| {
                if url.scheme() != "https"
                    || url.host_str().is_none()
                    || !url.username().is_empty()
                    || url.password().is_some()
                    || url.path() != "/"
                    || url.query().is_some()
                    || url.fragment().is_some()
                {
                    return Err("image registry host is not a valid HTTPS host".into());
                }
                Ok(url)
            })
    }

    pub fn manifest_url(&self, base: &Url) -> Result<Url, String> {
        let mut url = base.clone();
        {
            let mut segments = url
                .path_segments_mut()
                .map_err(|_| "registry URL cannot accept a manifest path".to_string())?;
            segments.pop_if_empty();
            segments.push("v2");
            for part in self.repository.split('/') {
                segments.push(part);
            }
            segments.push("manifests");
            segments.push(&self.reference);
        }
        Ok(url)
    }
}

fn registry_client() -> Result<Client, String> {
    // Built once: the config is static and `reqwest::Client` wraps a
    // connection pool meant to be reused — the first build initializes the
    // TLS backend, which on macOS can take many seconds.
    static CLIENT: std::sync::OnceLock<Client> = std::sync::OnceLock::new();
    Ok(CLIENT
        .get_or_init(|| {
            crate::http_util::client_builder()
                .connect_timeout(Duration::from_secs(3))
                .timeout(Duration::from_secs(7))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .expect("static registry client config")
        })
        .clone())
}

pub async fn check_image(raw: &str) -> ImageAvailability {
    let image = match ImageReference::parse(raw) {
        Ok(image) => image,
        Err(error) => return ImageAvailability::Unverified(error),
    };
    let base = match image.registry_base() {
        Ok(base) => base,
        Err(error) => return ImageAvailability::Unverified(error),
    };
    let client = match registry_client() {
        Ok(client) => client,
        Err(error) => return ImageAvailability::Unverified(error),
    };
    match tokio::time::timeout(
        Duration::from_secs(10),
        check_image_at(&client, &image, &base, false),
    )
    .await
    {
        Ok(result) => result,
        Err(_) => ImageAvailability::Unverified("registry check exceeded 10 seconds".into()),
    }
}

/// Resolve `repo/image:tag` to its digest-pinned form `repo/image@sha256:…`.
/// Already-pinned references round-trip unchanged.
///
/// The digest pins what the operator approved: a tag can be re-pushed, so the
/// host document must record the immutable reference the registry answered
/// with rather than the mutable name the human typed.
pub async fn resolve_pinned(raw: &str) -> Result<String, String> {
    let image = ImageReference::parse(raw)?;
    if image.reference.starts_with("sha256:") {
        return Ok(pinned_reference(&image, &image.reference));
    }
    let base = image.registry_base()?;
    let client = registry_client()?;
    match tokio::time::timeout(
        Duration::from_secs(10),
        resolve_digest_at(&client, &image, &base, false),
    )
    .await
    {
        Ok(result) => result,
        Err(_) => Err("registry digest lookup exceeded 10 seconds".into()),
    }
}

async fn resolve_digest_at(
    client: &Client,
    image: &ImageReference,
    base: &Url,
    allow_local_http_auth: bool,
) -> Result<String, String> {
    let manifest_url = image.manifest_url(base)?;
    let response = request_manifest(
        client,
        &manifest_url,
        Method::HEAD,
        allow_local_http_auth,
        true,
    )
    .await?;
    match response.status() {
        status if status.is_success() => {
            if let Some(digest) = response
                .headers()
                .get("docker-content-digest")
                .and_then(|value| value.to_str().ok())
                .and_then(parse_sha256_digest)
            {
                return Ok(pinned_reference(image, &digest));
            }
            // Some registries omit the header on HEAD. The digest is defined
            // as sha256 over the exact manifest bytes, so fetch the full body
            // (no Range) and hash it.
            let full = request_manifest(
                client,
                &manifest_url,
                Method::GET,
                allow_local_http_auth,
                false,
            )
            .await?;
            if !full.status().is_success() {
                return Err(format!(
                    "registry returned HTTP {} for the manifest",
                    full.status()
                ));
            }
            let body = read_response_bytes_limited(full, MAX_MANIFEST_FALLBACK_BYTES).await?;
            Ok(pinned_reference(
                image,
                &format!("sha256:{}", shared::sandbox_config::sha256_hex(&body)),
            ))
        }
        StatusCode::NOT_FOUND => Err(format!(
            "image '{}:{}' was not found in the registry",
            image.repository, image.reference
        )),
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
            Err("registry requires credentials or denied access".into())
        }
        status => Err(format!("registry returned HTTP {status}")),
    }
}

fn parse_sha256_digest(raw: &str) -> Option<String> {
    let raw = raw.trim().to_ascii_lowercase();
    let hex = raw.strip_prefix("sha256:")?;
    (hex.len() == 64 && hex.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then(|| format!("sha256:{hex}"))
}

/// Render the reference in the operator's own spelling: `docker.io/library/*`
/// is the implicit form, so its default registry prefix is dropped instead of
/// expanding a name the operator never wrote.
fn pinned_reference(image: &ImageReference, digest: &str) -> String {
    if image.registry == "docker.io" {
        let repository = image
            .repository
            .strip_prefix("library/")
            .unwrap_or(&image.repository);
        return format!("{repository}@{digest}");
    }
    format!("{}/{}@{}", image.registry, image.repository, digest)
}

pub async fn check_image_at(
    client: &Client,
    image: &ImageReference,
    base: &Url,
    allow_local_http_auth: bool,
) -> ImageAvailability {
    let manifest_url = match image.manifest_url(base) {
        Ok(url) => url,
        Err(error) => return ImageAvailability::Unverified(error),
    };
    let mut response = match request_manifest(
        client,
        &manifest_url,
        Method::HEAD,
        allow_local_http_auth,
        true,
    )
    .await
    {
        Ok(response) => response,
        Err(error) => return ImageAvailability::Unverified(error),
    };
    let mut used_get_fallback = false;
    if response.status() == StatusCode::METHOD_NOT_ALLOWED
        || response.status() == StatusCode::NOT_IMPLEMENTED
    {
        used_get_fallback = true;
        response = match request_manifest(
            client,
            &manifest_url,
            Method::GET,
            allow_local_http_auth,
            true,
        )
        .await
        {
            Ok(response) => response,
            Err(error) => return ImageAvailability::Unverified(error),
        };
    }
    match response.status() {
        status if status.is_success() => {
            if used_get_fallback {
                if let Err(error) =
                    read_response_limited(response, MAX_MANIFEST_FALLBACK_BYTES).await
                {
                    return ImageAvailability::Unverified(error);
                }
            }
            ImageAvailability::Exists
        }
        StatusCode::NOT_FOUND => ImageAvailability::NotFound,
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
            ImageAvailability::Unverified("registry requires credentials or denied access".into())
        }
        status => ImageAvailability::Unverified(format!("registry returned HTTP {status}")),
    }
}

async fn read_response_limited(response: Response, limit: u64) -> Result<(), String> {
    read_response_bytes_limited(response, limit)
        .await
        .map(|_| ())
}

async fn read_response_bytes_limited(
    mut response: Response,
    limit: u64,
) -> Result<Vec<u8>, String> {
    if response
        .content_length()
        .is_some_and(|length| length > limit)
    {
        return Err("registry response exceeded the 64 KiB size limit".into());
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "registry response could not be read".to_string())?
    {
        if body.len().saturating_add(chunk.len()) as u64 > limit {
            return Err("registry response exceeded the 64 KiB size limit".into());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

async fn request_manifest(
    client: &Client,
    url: &Url,
    method: Method,
    allow_local_http_auth: bool,
    bounded_range: bool,
) -> Result<Response, String> {
    let first = send_manifest_request(client, url, method.clone(), None, bounded_range).await?;
    if first.status() != StatusCode::UNAUTHORIZED {
        return Ok(first);
    }
    let Some(challenge) = first
        .headers()
        .get(WWW_AUTHENTICATE)
        .and_then(|value| value.to_str().ok())
        .and_then(parse_bearer_challenge)
    else {
        return Ok(first);
    };
    let token = fetch_bearer_token(client, &challenge, url, allow_local_http_auth).await?;
    send_manifest_request(client, url, method, Some(&token), bounded_range).await
}

async fn send_manifest_request(
    client: &Client,
    url: &Url,
    method: Method,
    token: Option<&str>,
    bounded_range: bool,
) -> Result<Response, String> {
    let bounded_get = bounded_range && method == Method::GET;
    let mut request = client
        .request(method, url.clone())
        .header(ACCEPT, MANIFEST_ACCEPT);
    if bounded_get {
        request = request.header(RANGE, "bytes=0-0");
    }
    if let Some(token) = token {
        let value = HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|_| "registry returned an invalid bearer token".to_string())?;
        request = request.header(AUTHORIZATION, value);
    }
    request
        .send()
        .await
        .map_err(|error| format!("registry request failed: {error}"))
}

pub async fn fetch_bearer_token(
    client: &Client,
    challenge: &BearerChallenge,
    registry_url: &Url,
    allow_local_http_auth: bool,
) -> Result<String, String> {
    let mut url = Url::parse(&challenge.realm)
        .map_err(|_| "registry returned an invalid bearer-token realm".to_string())?;
    let local_http_test = allow_local_http_auth
        && url.scheme() == "http"
        && url
            .host_str()
            .is_some_and(|host| host == "127.0.0.1" || host == "localhost");
    if url.scheme() != "https" && !local_http_test {
        return Err("registry bearer-token realm must use HTTPS".into());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("registry bearer-token realm must not contain credentials".into());
    }
    let same_origin = url.scheme() == registry_url.scheme()
        && url.host_str() == registry_url.host_str()
        && url.port_or_known_default() == registry_url.port_or_known_default();
    let docker_hub_auth = registry_url.host_str() == Some("registry-1.docker.io")
        && url.scheme() == "https"
        && url.host_str() == Some("auth.docker.io")
        && url.port_or_known_default() == Some(443);
    if !same_origin && !docker_hub_auth {
        return Err("registry bearer-token realm is not an approved authentication host".into());
    }
    {
        let mut query = url.query_pairs_mut();
        if let Some(service) = &challenge.service {
            query.append_pair("service", service);
        }
        if let Some(scope) = &challenge.scope {
            query.append_pair("scope", scope);
        }
    }
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|error| format!("registry token request failed: {error}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "registry token service returned HTTP {}",
            response.status()
        ));
    }
    let body = read_response_bytes_limited(response, MAX_TOKEN_RESPONSE_BYTES as u64).await?;
    let token = serde_json::from_slice::<TokenResponse>(&body)
        .map_err(|_| "registry token service returned invalid JSON".to_string())?;
    token
        .token
        .or(token.access_token)
        .filter(|token| !token.is_empty())
        .ok_or_else(|| "registry token response did not include a token".into())
}

pub fn parse_bearer_challenge(raw: &str) -> Option<BearerChallenge> {
    let (scheme, params) = raw.trim().split_once(char::is_whitespace)?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    let mut values = std::collections::HashMap::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut escaped = false;
    let mut parts = Vec::new();
    for ch in params.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
        } else if ch == '\\' && quoted {
            escaped = true;
        } else if ch == '"' {
            quoted = !quoted;
            current.push(ch);
        } else if ch == ',' && !quoted {
            parts.push(std::mem::take(&mut current));
        } else {
            current.push(ch);
        }
    }
    parts.push(current);
    for part in parts {
        let (key, value) = part.trim().split_once('=')?;
        let value = value.trim();
        let unquoted = value
            .strip_prefix('"')
            .and_then(|value| value.strip_suffix('"'))
            .unwrap_or(value)
            .replace("\\\"", "\"")
            .replace("\\\\", "\\");
        values.insert(key.trim().to_ascii_lowercase(), unquoted);
    }
    Some(BearerChallenge {
        realm: values.remove("realm")?,
        service: values.remove("service"),
        scope: values.remove("scope"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIGEST: &str = "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    /// The pinned reference keeps the operator's spelling: a bare Docker Hub
    /// name stays short, an explicit registry keeps its host.
    #[test]
    fn pinned_reference_keeps_the_operator_spelling() {
        let bare = ImageReference::parse("nginx:1.25").unwrap();
        assert_eq!(pinned_reference(&bare, DIGEST), format!("nginx@{DIGEST}"));

        let namespaced = ImageReference::parse("ghcr.io/buz/envelope-sandbox:v1").unwrap();
        assert_eq!(
            pinned_reference(&namespaced, DIGEST),
            format!("ghcr.io/buz/envelope-sandbox@{DIGEST}")
        );
    }

    /// An already-pinned reference round-trips unchanged.
    #[test]
    fn pinned_input_round_trips_through_the_parser() {
        let image = ImageReference::parse(&format!("ghcr.io/buz/envelope@{DIGEST}")).unwrap();
        assert_eq!(
            pinned_reference(&image, &image.reference),
            format!("ghcr.io/buz/envelope@{DIGEST}")
        );
    }

    /// Only well-formed sha256 digests are accepted from the registry header.
    #[test]
    fn digest_header_parser_rejects_malformed_values() {
        assert_eq!(parse_sha256_digest(DIGEST), Some(DIGEST.to_string()));
        assert_eq!(
            parse_sha256_digest(&format!("  {}  ", DIGEST.to_uppercase())),
            Some(DIGEST.to_string())
        );
        assert_eq!(parse_sha256_digest("sha256:xyz"), None);
        assert_eq!(parse_sha256_digest("md5:whatever"), None);
        assert_eq!(parse_sha256_digest(""), None);
    }
}
