//! API token generation and verification.
//!
//! API tokens are random bearer credentials that substitute a user JWT. Only
//! the SHA-256 hash is stored; the plaintext is returned once at creation.

use sha2::{Digest, Sha256};

/// Prefix for every API token so they are recognizable in logs and DB dumps.
pub const API_TOKEN_PREFIX: &str = "buz_";

/// Length of the random suffix in hex chars (32 bytes -> 64 hex chars).
const TOKEN_RANDOM_BYTES: usize = 32;

/// Generate a new random API token, e.g. `buz_a1b2c3…`.
pub fn generate_token() -> String {
    let mut bytes = [0u8; TOKEN_RANDOM_BYTES];
    getrandom::getrandom(&mut bytes).expect("os random for api token");
    format!("{API_TOKEN_PREFIX}{}", hex::encode(bytes))
}

/// SHA-256 hex digest of a token, used as the stored credential.
pub fn hash_token(token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    hex::encode(hasher.finalize())
}

/// Display prefix shown in list UIs: `buz_ab12…`.
pub fn display_prefix(token: &str) -> String {
    let visible = token.chars().take(10).collect::<String>();
    format!("{visible}…")
}

/// Extract the user id from a raw API token (unused — kept for parity with JWT).
#[allow(dead_code)]
pub fn is_api_token(token: &str) -> bool {
    token.starts_with(API_TOKEN_PREFIX)
}
