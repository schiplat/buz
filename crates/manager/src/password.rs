//! Password hashing helpers (Argon2id).

use argon2::{
    password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};

/// Strong password policy for first-admin setup (and recommended for new admins).
///
/// Requires: length ≥ 8, and at least one of each: lowercase, uppercase, digit,
/// and a non-alphanumeric symbol.
pub fn validate_password_complexity(password: &str) -> Result<(), String> {
    if password.len() < 8 {
        return Err("password must be at least 8 characters".into());
    }
    let mut has_lower = false;
    let mut has_upper = false;
    let mut has_digit = false;
    let mut has_symbol = false;
    for c in password.chars() {
        if c.is_ascii_lowercase() {
            has_lower = true;
        } else if c.is_ascii_uppercase() {
            has_upper = true;
        } else if c.is_ascii_digit() {
            has_digit = true;
        } else if !c.is_whitespace() {
            has_symbol = true;
        }
    }
    if !(has_lower && has_upper && has_digit && has_symbol) {
        return Err(
            "password must include uppercase, lowercase, a digit, and a symbol (e.g. !@#$%)"
                .into(),
        );
    }
    if password.chars().any(|c| c.is_whitespace()) {
        return Err("password must not contain whitespace".into());
    }
    Ok(())
}

pub fn hash_password(password: &str) -> Result<String, String> {
    let salt = SaltString::generate(&mut OsRng);
    let hash = Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map_err(|e| format!("password hash failed: {e}"))?;
    Ok(hash.to_string())
}

pub fn verify_password(password: &str, password_hash: &str) -> bool {
    let Ok(parsed) = PasswordHash::new(password_hash) else {
        // Not AES/Secrets crypto — user passwords must be Argon2 PHC strings.
        tracing::error!(
            hash_len = password_hash.len(),
            "password_hash is not a valid Argon2 PHC string"
        );
        return false;
    };
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok()
}

#[cfg(test)]
#[path = "password_tests.rs"]
mod tests;
