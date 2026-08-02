//! Aliyun OSS config delivery credentials (`config_source`).
//!
//! YAML / form only stores the **Manager Secret name**. The Secret value (JSON)
//! holds OSS read-only AK/SK. At job dispatch the Manager generates a
//! short-lived signed GET URL (OSS Query String Authentication, V1) and injects
//! it into the step env as `BUZ_CONFIG_URL`. Targets download with plain
//! `curl` — no OSS SDK and no long-lived credentials on the device.

use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;
use hmac::{Hmac, Mac};
use serde::Deserialize;
use sha1::Sha1;

type HmacSha1 = Hmac<Sha1>;

/// Well-known provider for OSS config delivery secrets.
pub const PROVIDER_OSS_RO: &str = "aliyun_oss";

/// Default TTL for signed config URLs (seconds).
pub const DEFAULT_CONFIG_URL_TTL_SECS: i64 = 3600;

/// Resolved OSS config auth passed from Manager → Runner (never stored on target).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OssConfigAuth {
    pub endpoint: String,
    pub bucket: String,
    pub region: Option<String>,
    pub access_key_id: String,
    pub access_key_secret: String,
    pub provider: Option<String>,
}

/// JSON shape stored in Manager Secrets for `config_source`.
#[derive(Debug, Clone, Deserialize)]
struct OssSecretJson {
    /// Optional vendor: `aliyun_oss` | future values.
    #[serde(default)]
    provider: Option<String>,
    #[serde(default)]
    endpoint: Option<String>,
    #[serde(default)]
    bucket: Option<String>,
    #[serde(default)]
    region: Option<String>,
    #[serde(default)]
    access_key_id: Option<String>,
    #[serde(default)]
    access_key_secret: Option<String>,
}

/// Heuristic: value looks like OSS credential JSON (endpoint + bucket + AK/SK).
pub fn looks_like_oss_secret_value(raw: &str) -> bool {
    let raw = raw.trim();
    if raw.is_empty() {
        return false;
    }
    let Ok(parsed) = serde_json::from_str::<OssSecretJson>(raw) else {
        return false;
    };
    let has_endpoint = parsed
        .endpoint
        .as_deref()
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);
    let has_ak = parsed
        .access_key_id
        .as_deref()
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);
    let has_sk = parsed
        .access_key_secret
        .as_deref()
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);
    has_endpoint && has_ak && has_sk
}

/// Parse a Secret value JSON into [`OssConfigAuth`].
pub fn parse_oss_secret_value(raw: &str) -> Result<OssConfigAuth, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("OSS config secret value is empty".into());
    }

    let parsed: OssSecretJson = serde_json::from_str(raw).map_err(|e| {
        format!(
            "OSS config secret must be JSON with endpoint/bucket/access_key_id/access_key_secret \
             (optional region, provider): {e}"
        )
    })?;

    let endpoint = parsed
        .endpoint
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "OSS config secret JSON requires non-empty 'endpoint' (e.g. oss-cn-guangzhou.aliyuncs.com)".to_string())?
        .to_string();

    let bucket = parsed
        .bucket
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "OSS config secret JSON requires non-empty 'bucket'".to_string())?
        .to_string();

    let access_key_id = parsed
        .access_key_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "OSS config secret JSON requires non-empty 'access_key_id'".to_string())?
        .to_string();

    let access_key_secret = parsed
        .access_key_secret
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "OSS config secret JSON requires non-empty 'access_key_secret'".to_string())?
        .to_string();

    let region = parsed
        .region
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    let provider = parsed
        .provider
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_ascii_lowercase());

    Ok(OssConfigAuth {
        endpoint,
        bucket,
        region,
        access_key_id,
        access_key_secret,
        provider,
    })
}

/// Build a pre-signed OSS GET URL (Query String Authentication, V1).
///
/// Returns an HTTPS URL valid for `ttl_secs` (min 60s). The signature only
/// covers this exact object path; the URL cannot be re-pointed at other keys.
pub fn sign_oss_get_url(
    auth: &OssConfigAuth,
    object_key: &str,
    ttl_secs: i64,
) -> Result<String, String> {
    if auth.endpoint.trim().is_empty() || auth.bucket.trim().is_empty() {
        return Err("OSS config secret requires non-empty 'endpoint' and 'bucket'".into());
    }
    if auth.access_key_id.trim().is_empty() || auth.access_key_secret.trim().is_empty() {
        return Err(
            "OSS config secret requires non-empty 'access_key_id' and 'access_key_secret'".into(),
        );
    }

    let object_key = object_key.trim().trim_start_matches('/');
    if object_key.is_empty() {
        return Err("OSS config object key is empty".into());
    }

    let expires = chrono::Utc::now().timestamp() + ttl_secs.max(60);

    // OSS V1 Query String Authentication:
    //   StringToSign = VERB \n Content-MD5 \n Content-Type \n Expires \n CanonicalizedResource
    // GET with no body/headers → empty MD5 and Content-Type lines.
    let canonical_resource = format!("/{}/{}", auth.bucket, object_key);
    let string_to_sign = format!("GET\n\n\n{expires}\n{canonical_resource}");

    let mut mac = HmacSha1::new_from_slice(auth.access_key_secret.trim().as_bytes())
        .map_err(|e| format!("invalid OSS access_key_secret: {e}"))?;
    mac.update(string_to_sign.as_bytes());
    let signature = BASE64_STANDARD.encode(mac.finalize().into_bytes());

    // Signature is query-value encoded; the object key keeps `/` as path separators.
    let sig_encoded = percent_encode_query_value(&signature);
    let key_encoded = percent_encode_path(object_key);

    Ok(format!(
        "https://{bucket}.{endpoint}/{key}?OSSAccessKeyId={ak}&Expires={expires}&Signature={sig_encoded}",
        bucket = auth.bucket.trim(),
        endpoint = auth.endpoint.trim(),
        key = key_encoded,
        ak = auth.access_key_id.trim(),
    ))
}

/// Percent-encode bytes outside the unreserved set, keeping `/` for path segments.
fn percent_encode_path(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => {
                out.push_str(&format!("%{b:02X}"));
            }
        }
    }
    out
}

/// Percent-encode bytes outside the unreserved set (RFC 3986), for query values.
fn percent_encode_query_value(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => {
                out.push_str(&format!("%{b:02X}"));
            }
        }
    }
    out
}

#[cfg(test)]
#[path = "config_auth_tests.rs"]
mod tests;
