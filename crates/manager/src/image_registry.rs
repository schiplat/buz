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

pub async fn check_image(raw: &str) -> ImageAvailability {
    let image = match ImageReference::parse(raw) {
        Ok(image) => image,
        Err(error) => return ImageAvailability::Unverified(error),
    };
    let base = match image.registry_base() {
        Ok(base) => base,
        Err(error) => return ImageAvailability::Unverified(error),
    };
    let client = match Client::builder()
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(7))
        .redirect(reqwest::redirect::Policy::none())
        .build()
    {
        Ok(client) => client,
        Err(error) => {
            return ImageAvailability::Unverified(format!("cannot create registry client: {error}"))
        }
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
    let mut response =
        match request_manifest(client, &manifest_url, Method::HEAD, allow_local_http_auth).await {
            Ok(response) => response,
            Err(error) => return ImageAvailability::Unverified(error),
        };
    let mut used_get_fallback = false;
    if response.status() == StatusCode::METHOD_NOT_ALLOWED
        || response.status() == StatusCode::NOT_IMPLEMENTED
    {
        used_get_fallback = true;
        response = match request_manifest(client, &manifest_url, Method::GET, allow_local_http_auth)
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
) -> Result<Response, String> {
    let first = send_manifest_request(client, url, method.clone(), None).await?;
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
    send_manifest_request(client, url, method, Some(&token)).await
}

async fn send_manifest_request(
    client: &Client,
    url: &Url,
    method: Method,
    token: Option<&str>,
) -> Result<Response, String> {
    let bounded_get = method == Method::GET;
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
