//! Secret masker — replaces sensitive values in log output with `***`.
//!
//! Modeled after GitHub Actions' automatic secret redaction.
//! Any value present in the job's environment (secrets, trigger inputs) is
//! automatically masked when it appears in log output.

/// Collect all secret values from an env map (ignore empty values).
pub fn collect_secret_values(env: &std::collections::HashMap<String, String>) -> Vec<String> {
    env.values()
        .filter(|v| !v.is_empty())
        .map(|v| v.clone())
        .collect()
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
