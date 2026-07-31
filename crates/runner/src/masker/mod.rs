//! Secret masker — replaces sensitive values in log output with `***`.
//!
//! Modeled after GitHub Actions' automatic secret redaction.
//! Redacts Infrastructure Secrets, registry passwords, and trigger inputs marked
//! `masking: true`. Pipeline `env:` and platform Variables stay readable.

use std::collections::{HashMap, HashSet};

/// Collect values for keys marked as secrets (ignore empty values).
pub fn collect_secret_values_for_keys(
    env: &HashMap<String, String>,
    secret_keys: &HashSet<String>,
) -> Vec<String> {
    secret_keys
        .iter()
        .filter_map(|k| env.get(k))
        .filter(|v| !v.is_empty())
        .cloned()
        .collect()
}

/// Collect all values from an env map (legacy helper — prefer [`collect_secret_values_for_keys`]).
#[cfg(test)]
pub fn collect_secret_values(env: &HashMap<String, String>) -> Vec<String> {
    env.values().filter(|v| !v.is_empty()).cloned().collect()
}

/// Mask all secret values in `text`, replacing them with `***`.
///
/// Values shorter than 3 characters are skipped (too likely to produce false
/// positives — e.g. masking `\n` or short common tokens).
pub fn mask_secrets(text: &str, secrets: &[String]) -> String {
    if secrets.is_empty() || text.is_empty() {
        return text.to_string();
    }

    let mut result = text.to_string();
    for secret in secrets {
        if secret.len() < 3 {
            continue;
        }
        // Simple string replacement — sufficient for typical use cases.
        // GitHub Actions uses a similar approach with additional regex fuzziness.
        result = result.replace(secret.as_str(), "***");
    }
    result
}

#[cfg(test)]
#[path = "masker_tests.rs"]
mod tests;
