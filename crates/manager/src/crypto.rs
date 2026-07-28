//! AES-256-GCM encryption for sensitive data at rest (e.g. credential_data).
//!
//! The encryption key is derived from the Manager's `jwt_secret` via SHA-256,
//! ensuring that if the database file is leaked, the encrypted values remain
//! unreadable without the secret.

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
    let cipher = Aes256Gcm::new_from_slice(&key)
        .map_err(|e| format!("AES-256-GCM init failed: {e}"))?;

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
/// Returns the original plaintext, or an empty string if the input is empty
/// (backward-compatible with unencrypted rows).
pub fn decrypt(encoded: &str, jwt_secret: &str) -> Result<String, String> {
    if encoded.is_empty() {
        return Ok(String::new());
    }

    let key = derive_key(jwt_secret);
    let cipher = Aes256Gcm::new_from_slice(&key)
        .map_err(|e| format!("AES-256-GCM init failed: {e}"))?;

    let combined = match base64::Engine::decode(
        &base64::engine::general_purpose::STANDARD,
        encoded,
    ) {
        Ok(data) => data,
        Err(_) => {
            // Not valid base64 — legacy plaintext value (backward-compatible)
            return Ok(encoded.to_string());
        }
    };

    if combined.len() < 12 {
        // Not encrypted — legacy plaintext value (backward-compatible)
        return Ok(encoded.to_string());
    }

    let (nonce_bytes, ciphertext) = combined.split_at(12);
    let nonce = Nonce::from_slice(nonce_bytes);

    let plaintext = cipher
        .decrypt(nonce, ciphertext)
        .map_err(|e| format!("AES-256-GCM decrypt failed: {e}"))?;

    String::from_utf8(plaintext).map_err(|e| format!("UTF-8 decode failed: {e}"))
}

#[cfg(test)]
#[path = "crypto_tests.rs"]
mod tests;
