//! AES-256-GCM encryption for sensitive data at rest
//! (host credentials, Manager Secrets values).
//!
//! The encryption key is derived from the Manager's `jwt_secret` via SHA-256,
//! ensuring that if the database file is leaked, the encrypted values remain
//! unreadable without the secret.
//!
//! Plaintext rows are **not** accepted by [`decrypt`] — re-save via the UI/API
//! so values are stored as ciphertext.

use aes_gcm::{
    aead::{Aead, KeyInit, OsRng},
    Aes256Gcm, Nonce,
};
use rand::RngCore;
use sha2::{Digest, Sha256};

/// Derive a 32-byte AES-256 key from the JWT secret.
fn derive_key(jwt_secret: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(jwt_secret.as_bytes());
    let result = hasher.finalize();
    let mut key = [0u8; 32];
    key.copy_from_slice(&result);
    key
}

/// Encrypt `plaintext` with AES-256-GCM, keyed by `jwt_secret`.
///
/// Returns a base64-encoded string containing `nonce (12 bytes) || ciphertext+tag`.
pub fn encrypt(plaintext: &str, jwt_secret: &str) -> Result<String, String> {
    if plaintext.is_empty() {
        return Ok(String::new());
    }

    let key = derive_key(jwt_secret);
    let cipher =
        Aes256Gcm::new_from_slice(&key).map_err(|e| format!("AES-256-GCM init failed: {e}"))?;

    let mut nonce_bytes = [0u8; 12];
    OsRng.fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);

    let ciphertext = cipher
        .encrypt(nonce, plaintext.as_bytes())
        .map_err(|e| format!("AES-256-GCM encrypt failed: {e}"))?;

    // Prepend nonce to ciphertext, then base64-encode
    let mut combined = nonce_bytes.to_vec();
    combined.extend_from_slice(&ciphertext);

    Ok(base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        &combined,
    ))
}

/// Decrypt a value previously produced by [`encrypt`].
///
/// Empty input → empty output. Plaintext / corrupt rows return `Err` (no legacy
/// plaintext passthrough).
pub fn decrypt(encoded: &str, jwt_secret: &str) -> Result<String, String> {
    if encoded.is_empty() {
        return Ok(String::new());
    }

    let key = derive_key(jwt_secret);
    let cipher =
        Aes256Gcm::new_from_slice(&key).map_err(|e| format!("AES-256-GCM init failed: {e}"))?;

    let combined = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, encoded)
        .map_err(|_| {
            "stored value is not encrypted ciphertext; re-save the secret or host credential \
             via the Manager UI"
                .to_string()
        })?;

    if combined.len() < 12 + 16 {
        // AES-GCM tag is 16 bytes; nonce is 12. Shorter payloads are invalid.
        return Err(
            "stored value is not a valid encrypted blob; re-save via the Manager UI".into(),
        );
    }

    let (nonce_bytes, ciphertext) = combined.split_at(12);
    let nonce = Nonce::from_slice(nonce_bytes);

    let plaintext = cipher.decrypt(nonce, ciphertext).map_err(|_| {
        "failed to decrypt stored value (wrong jwt_secret, or plaintext row); \
         re-save via the Manager UI"
            .to_string()
    })?;

    String::from_utf8(plaintext).map_err(|e| format!("UTF-8 decode failed: {e}"))
}

/// Keys whose string values are partially redacted when masking JSON secrets.
const SENSITIVE_JSON_KEYS: &[&str] = &[
    "password",
    "passwd",
    "secret",
    "token",
    "access_key",
    "secret_key",
    "credential",
    "credentials",
    "passphrase",
    "private_key",
    "api_key",
];

/// Partial redaction for API display: keep a short head/tail, hide the middle.
///
/// For JSON objects, sensitive keys (`password`, `token`, …) are redacted in
/// place so operators can still see non-sensitive fields (e.g. `provider`,
/// `registry`, `username`).
pub fn mask_for_display(plaintext: &str) -> String {
    let trimmed = plaintext.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    if let Ok(serde_json::Value::Object(mut map)) = serde_json::from_str(trimmed) {
        for (key, value) in map.iter_mut() {
            if is_sensitive_key(key) {
                if let Some(s) = value.as_str() {
                    *value = serde_json::Value::String(mask_string(s));
                } else {
                    *value = serde_json::Value::String("****".into());
                }
            }
        }
        return serde_json::to_string(&serde_json::Value::Object(map))
            .unwrap_or_else(|_| mask_string(trimmed));
    }

    mask_string(trimmed)
}

fn is_sensitive_key(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    SENSITIVE_JSON_KEYS
        .iter()
        .any(|k| lower == *k || lower.ends_with(&format!("_{k}")) || lower.ends_with(k))
}

/// `ab****yz` style masking for a single string.
pub fn mask_string(raw: &str) -> String {
    let chars: Vec<char> = raw.chars().collect();
    let n = chars.len();
    if n == 0 {
        return String::new();
    }
    if n <= 4 {
        return "****".to_string();
    }
    if n <= 8 {
        let head: String = chars[..1].iter().collect();
        let tail: String = chars[n - 1..].iter().collect();
        return format!("{head}****{tail}");
    }
    let head: String = chars[..2].iter().collect();
    let tail: String = chars[n - 2..].iter().collect();
    format!("{head}****{tail}")
}

#[cfg(test)]
#[path = "crypto_tests.rs"]
mod tests;
