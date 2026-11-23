//! Username derivation for SSO-provisioned accounts.
//!
//! Two code paths create a local account from an external identity: JIT
//! provisioning at first sign-in (`api::auth::provision_oidc_user`) and an
//! administrator approving a queued identity (`api::oidc_identities`). They
//! must agree — one policy, two callers — so an account approved later gets the
//! name the JIT path would have given.
//!
//! The preference order is: email local-part, then display name, then the
//! subject. The first two are human-meaningful. The third is the provider's
//! stable key: it exists so two identities with neither attribute never start
//! from the same literal base, because the numeric deconflict suffix that
//! separates them (`-2`, `-3`) would then erase every trace of which external
//! identity owns the account.

/// How many characters of the subject the fallback base keeps.
const SUBJECT_PREFIX_CHARS: usize = 12;

/// Longest base before the collision suffix.
pub const MAX_USERNAME_CHARS: usize = 48;

/// The preferred, unsanitised name source for an identity.
pub fn username_base(email: Option<&str>, display_name: Option<&str>, subject: &str) -> String {
    if let Some(local) = email
        .and_then(|email| email.split('@').next())
        .filter(|local| !local.is_empty())
    {
        return local.to_string();
    }
    if let Some(name) = display_name.map(str::trim).filter(|name| !name.is_empty()) {
        return name.to_string();
    }
    subject_fallback(subject)
}

/// The subject-derived last resort, already restricted to the characters
/// [`sanitize_username`] keeps — so it survives sanitisation as itself rather
/// than collapsing to a leftover empty string.
///
/// Counts **characters**, not bytes: `sub` is opaque provider text and slicing
/// it by byte offset can panic on a multi-byte boundary or split a glyph.
pub fn subject_fallback(subject: &str) -> String {
    let cleaned: String = subject
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        .take(SUBJECT_PREFIX_CHARS)
        .collect();
    if cleaned.is_empty() {
        // A provider that sends nothing usable has nothing to anchor on. This
        // literal is shared on purpose: the collision suffix owns uniqueness,
        // and the sign-in binding still keys on the full `sub` column rather
        // than on this display name.
        "sso-user".to_string()
    } else {
        format!("sso-{cleaned}")
    }
}

/// Sanitise a base name for storage: lowercase, ASCII alphanumerics plus
/// `.`, `-` and `_`, bounded to [`MAX_USERNAME_CHARS`].
///
/// `None` when nothing survives.
pub fn sanitize_username(base: &str) -> Option<String> {
    let sanitized: String = base
        .to_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        .collect();
    if sanitized.is_empty() {
        return None;
    }
    Some(sanitized.chars().take(MAX_USERNAME_CHARS).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefers_email_local_part() {
        assert_eq!(
            username_base(Some("Alice@Example.com"), Some("Alice Doe"), "sub-1"),
            "Alice"
        );
    }

    #[test]
    fn falls_back_to_display_name() {
        assert_eq!(username_base(None, Some("Alice Doe"), "sub-1"), "Alice Doe");
        assert_eq!(
            username_base(Some(""), Some("Alice Doe"), "sub-1"),
            "Alice Doe"
        );
    }

    #[test]
    fn distinct_subjects_do_not_share_one_literal_base() {
        let first = username_base(None, None, "aaa-111");
        let second = username_base(None, None, "bbb-222");
        assert_eq!(first, "sso-aaa-111");
        assert_eq!(second, "sso-bbb-222");
        assert_ne!(first, second);
    }

    #[test]
    fn subject_fallback_is_bounded_and_char_safe() {
        // Truncated by characters, never by bytes.
        assert_eq!(
            subject_fallback("cc02857e-959d-4c1e-9a7f"),
            "sso-cc02857e-959"
        );
        // Multi-byte input is kept whole or dropped, never panics.
        assert_eq!(subject_fallback("日本語の識別子です"), "sso-user");
        assert_eq!(subject_fallback("日本語の識別子です"), "sso-user");
        assert_eq!(subject_fallback(""), "sso-user");
    }

    #[test]
    fn subject_fallback_survives_sanitisation_unchanged() {
        let fallback = subject_fallback("cc02857e-959d-4c1e");
        assert_eq!(
            sanitize_username(&fallback).as_deref(),
            Some(fallback.as_str())
        );
        let anonymous = subject_fallback("");
        assert_eq!(
            sanitize_username(&anonymous).as_deref(),
            Some(anonymous.as_str())
        );
    }

    #[test]
    fn sanitize_lowercases_and_filters() {
        assert_eq!(sanitize_username("Alice.Doe"), Some("alice.doe".into()));
        assert_eq!(sanitize_username("a b@c!d"), Some("abcd".into()));
        assert_eq!(sanitize_username("@@@"), None);
    }

    #[test]
    fn sanitize_bounds_the_length() {
        let sanitized = sanitize_username(&"a".repeat(120)).unwrap();
        assert_eq!(sanitized.chars().count(), MAX_USERNAME_CHARS);
    }
}
