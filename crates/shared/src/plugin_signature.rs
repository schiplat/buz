//! Detached signatures for a source's trust documents (`index.json.sig`,
//! `reviews.json.sig`).
//!
//! The index pins every template by content hash, but an unsigned index can be
//! rewritten **together with** those hashes — the chain stays self-consistent
//! while the content is attacker-chosen. A detached Ed25519 signature over the
//! exact bytes closes that gap: the publisher signs, a Manager verifies
//! against a configured public key (typically one per source). The review
//! ledger gets the same treatment: under a trust anchor, an unsigned ledger
//! could resurrect "reviewed" verdicts the publisher has since revoked.
//!
//! Format (`<document>.sig`, JSON so the key id and diagnostics are readable):
//!
//! ```json
//! { "alg": "ed25519", "key_id": "buz-official-2026",
//!   "signature": "<base64 64-byte signature>", "sha256": "<hex of document bytes>" }
//! ```
//!
//! `sha256` duplicates what the signature proves; it exists so a mismatch can
//! be reported as "signed a different document" instead of a bare crypto error.

use base64::Engine as _;
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};

/// Signature file next to `index.json`; mirrors must copy it along.
pub const SIGNATURE_FILE: &str = "index.json.sig";

/// Signature file next to `reviews.json`, produced by the same
/// `buz-plugin index --sign-key` run.
pub const REVIEWS_SIGNATURE_FILE: &str = "reviews.json.sig";

pub const ALG: &str = "ed25519";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IndexSignature {
    pub alg: String,
    pub key_id: String,
    /// Base64 of the 64-byte Ed25519 signature over the index bytes.
    pub signature: String,
    /// Hex SHA-256 of the signed bytes (diagnostic only).
    pub sha256: String,
}

fn b64() -> base64::engine::general_purpose::GeneralPurpose {
    base64::engine::general_purpose::STANDARD
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(bytes))
}

fn decode_key(raw: &str, what: &str) -> Result<[u8; 32], String> {
    let bytes = b64()
        .decode(raw.trim())
        .map_err(|e| format!("{what} is not valid base64: {e}"))?;
    let len = bytes.len();
    let array: [u8; 32] = bytes
        .try_into()
        .map_err(|_| format!("{what} must decode to 32 bytes (got {len})"))?;
    Ok(array)
}

/// Public half of a signing key, base64 — what an operator pastes into
/// `plugin_index.public_key`.
pub fn public_key_base64(secret_key_base64: &str) -> Result<String, String> {
    let seed = decode_key(secret_key_base64, "secret key")?;
    Ok(b64().encode(SigningKey::from_bytes(&seed).verifying_key().to_bytes()))
}

/// Fresh keypair as `(secret_key_base64, public_key_base64)`. Seeds come from
/// the OS RNG directly (the workspace already depends on `getrandom`), so the
/// signing crate needs no extra feature.
pub fn generate_keypair() -> Result<(String, String), String> {
    let mut seed = [0u8; 32];
    getrandom::getrandom(&mut seed).map_err(|e| format!("OS random source failed: {e}"))?;
    let signing = SigningKey::from_bytes(&seed);
    Ok((
        b64().encode(signing.to_bytes()),
        b64().encode(signing.verifying_key().to_bytes()),
    ))
}

/// Sign document bytes (an index, a review ledger); returns the serialized
/// `<document>.sig` content.
pub fn sign_document(
    document_bytes: &[u8],
    secret_key_base64: &str,
    key_id: &str,
) -> Result<String, String> {
    let seed = decode_key(secret_key_base64, "secret key")?;
    let signing = SigningKey::from_bytes(&seed);
    let signature = signing.sign(document_bytes);
    let file = IndexSignature {
        alg: ALG.to_string(),
        key_id: key_id.trim().to_string(),
        signature: b64().encode(signature.to_bytes()),
        sha256: sha256_hex(document_bytes),
    };
    serde_json::to_string_pretty(&file).map_err(|e| format!("cannot serialize signature: {e}"))
}

/// Verify document bytes against a signature file and a trusted public key.
/// `signature_file` names the `.sig` document in error messages so a
/// reviews-ledger failure does not read as an index failure. Returns the
/// signature's `key_id` so callers can log which key matched.
pub fn verify_document(
    document_bytes: &[u8],
    signature_json: &str,
    public_key_base64: &str,
    signature_file: &str,
) -> Result<String, String> {
    let file: IndexSignature = serde_json::from_str(signature_json)
        .map_err(|e| format!("{signature_file} is not a signature document: {e}"))?;
    if file.alg != ALG {
        return Err(format!(
            "unsupported signature algorithm '{}' — only '{ALG}' is accepted",
            file.alg
        ));
    }
    let key = decode_key(public_key_base64, "configured public key")?;
    let verifying = VerifyingKey::from_bytes(&key)
        .map_err(|e| format!("configured public key is not a valid Ed25519 key: {e}"))?;
    let raw = b64()
        .decode(file.signature.trim())
        .map_err(|e| format!("signature is not valid base64: {e}"))?;
    let signature =
        Signature::from_slice(&raw).map_err(|e| format!("signature must be 64 bytes: {e}"))?;
    verifying
        .verify_strict(document_bytes, &signature)
        .map_err(|_| {
            let actual = sha256_hex(document_bytes);
            if actual == file.sha256 {
                // Same bytes, bad signature → wrong key rather than tampering.
                format!(
                    "signature does not match the configured public key (key '{}')",
                    file.key_id
                )
            } else {
                format!(
                    "document does not match its signature — signed sha256 {}, received {} \
                     (the document and its signature come from different revisions)",
                    file.sha256, actual
                )
            }
        })?;
    Ok(file.key_id)
}
