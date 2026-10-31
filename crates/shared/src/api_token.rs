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
    generate_with_prefix(API_TOKEN_PREFIX)
}

/// Generate a random bearer token with a custom prefix (used for sandboxd
/// instance tokens, `buzsd_…`, so the credential type is recognizable in
/// logs and token lists).
pub fn generate_with_prefix(prefix: &str) -> String {
    let mut bytes = [0u8; TOKEN_RANDOM_BYTES];
    getrandom::getrandom(&mut bytes).expect("os random for api token");
    format!("{prefix}{}", hex::encode(bytes))
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

/// Constant-time equality for secret material (register tokens and similar).
/// Comparisons on raw secrets must never use `==` — timing leaks how many
/// leading bytes match. Both sides are hashed and the fixed-size digests are
/// compared via `subtle::ConstantTimeEq`.
pub fn ct_eq(a: &str, b: &str) -> bool {
    use subtle::ConstantTimeEq;
    let da = Sha256::digest(a.as_bytes());
    let db = Sha256::digest(b.as_bytes());
    bool::from(da.as_slice().ct_eq(db.as_slice()))
}
