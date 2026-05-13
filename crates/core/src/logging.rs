#![forbid(unsafe_code)]

//! Tracing initialisation and redaction helpers.
//!
//! # Redaction policy (from the technical design security section)
//!
//! The following must never appear in any log output:
//! - API keys or decrypted secrets
//! - Full request URLs (which can contain identifying parameters)
//! - Full HTTP response bodies
//!
//! Enforcement strategy:
//! - Provider impls construct `ProviderError` with sanitised strings only.
//! - HTTP calls log the endpoint *path shortname* (e.g. `"openai/usage/completions"`),
//!   never the full URL with query parameters.
//! - `redact_if_key` can be used in the rare case where a string must be included in a
//!   log message but might accidentally contain a key-shaped value.

use std::borrow::Cow;

use anyhow::Result;
use tracing_subscriber::{fmt, EnvFilter};

/// Initialises the global tracing subscriber.
///
/// Call once at application startup, before any other tracing macros.
/// Subsequent calls are no-ops (the subscriber ignores re-registration).
pub fn init_tracing(level_filter: &str) -> Result<()> {
    let filter = EnvFilter::try_new(level_filter)
        .unwrap_or_else(|_| EnvFilter::new("info"));

    // `try_init` returns an error if a global subscriber is already set.
    // In tests each test module may call init_tracing; the second call fails
    // silently via the unwrap_or here.
    let _ = fmt::Subscriber::builder()
        .with_env_filter(filter)
        .with_target(true)
        .with_thread_ids(false)
        .with_file(false)
        .with_line_number(false)
        .try_init();

    Ok(())
}

/// Redacts any API key–shaped token found anywhere in `s`.
///
/// Patterns recognised (as substrings, not just prefixes):
/// - `sk-...`   OpenAI and Anthropic admin/project keys
///
/// Each matching token is replaced with `[REDACTED]`. Returns the original
/// string borrowed unchanged when no key-shaped token is present (fast path).
///
/// Use this defensively when logging values that might have been user-supplied.
/// The main redaction discipline is "don't pass keys to log macros at all";
/// this is a last-resort safety net.
pub fn redact_if_key(s: &str) -> Cow<'_, str> {
    const PATTERNS: &[&str] = &["sk-"];

    if !PATTERNS.iter().any(|p| s.contains(p)) {
        return Cow::Borrowed(s);
    }

    let mut result = String::with_capacity(s.len());
    let mut remaining = s;

    while !remaining.is_empty() {
        let earliest = PATTERNS
            .iter()
            .filter_map(|p| remaining.find(p).map(|pos| pos))
            .min();

        match earliest {
            None => {
                result.push_str(remaining);
                break;
            }
            Some(pos) => {
                result.push_str(&remaining[..pos]);
                remaining = &remaining[pos..];
                // A key token runs until the next whitespace character.
                let token_end = remaining
                    .find(|c: char| c.is_whitespace())
                    .unwrap_or(remaining.len());
                result.push_str("[REDACTED]");
                remaining = &remaining[token_end..];
            }
        }
    }

    Cow::Owned(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openai_key_is_redacted() {
        assert_eq!(redact_if_key("sk-abc123"), "[REDACTED]");
    }

    #[test]
    fn anthropic_key_is_redacted() {
        assert_eq!(redact_if_key("sk-ant-admin-xyz"), "[REDACTED]");
    }

    #[test]
    fn non_key_string_passes_through() {
        assert_eq!(redact_if_key("some plain value"), "some plain value");
    }

    #[test]
    fn init_tracing_does_not_panic() {
        init_tracing("info").unwrap();
    }

    #[test]
    fn init_tracing_repeated_call_does_not_panic() {
        init_tracing("info").unwrap();
        init_tracing("debug").unwrap(); // second call must not panic
    }

    // ── Redaction contract ────────────────────────────────────────────────────

    #[test]
    fn openai_project_key_is_redacted() {
        let key = "sk-proj-abcdefghijklmnopqrstuvwxyz1234";
        let result = redact_if_key(key);
        assert_eq!(result, "[REDACTED]");
        assert!(!result.contains(key), "raw key must not appear in output");
    }

    #[test]
    fn openai_admin_key_is_redacted() {
        let key = "sk-admin-supersecretkeyvalue";
        let result = redact_if_key(key);
        assert_eq!(result, "[REDACTED]");
        assert!(!result.contains("sk-admin"), "key prefix must not appear in output");
    }

    #[test]
    fn anthropic_admin_key_is_redacted_and_output_contains_no_key_material() {
        let key = "sk-ant-admin-supersecretkey1234567890";
        let result = redact_if_key(key);
        assert_eq!(result, "[REDACTED]");
        assert!(!result.contains("sk-ant"), "Anthropic key prefix must not appear");
    }

    #[test]
    fn redacted_output_never_starts_with_sk() {
        let keys = [
            "sk-abc123",
            "sk-proj-xyz",
            "sk-ant-admin-xyz",
            "sk-",
        ];
        for key in &keys {
            let out = redact_if_key(key);
            assert!(
                !out.starts_with("sk-"),
                "output for key '{key}' still starts with sk-: {out}"
            );
        }
    }

    #[test]
    fn non_key_values_pass_through_unchanged() {
        let safe_values = ["bearer token", "some-config-value", "info", "debug"];
        for v in &safe_values {
            assert_eq!(redact_if_key(v), *v);
        }
    }

    // ── Substring redaction (keys embedded in error messages) ─────────────────

    #[test]
    fn key_embedded_in_error_message_is_redacted() {
        let msg = "HTTP 401 from https://api.openai.com: sk-proj-supersecretkey";
        let out = redact_if_key(msg);
        assert!(!out.contains("sk-proj"), "key must not appear in output");
        assert!(out.contains("HTTP 401"), "non-key prefix must be preserved");
        assert!(out.contains("[REDACTED]"));
    }

    #[test]
    fn key_mid_sentence_is_redacted() {
        let msg = "invalid key sk-ant-admin-xyz encountered";
        let out = redact_if_key(msg);
        assert!(!out.contains("sk-ant"), "key must not appear in output");
        assert!(out.contains("invalid key"), "prefix text must survive");
        assert!(out.contains("encountered"), "suffix text must survive");
    }

    #[test]
    fn multiple_keys_in_one_string_are_all_redacted() {
        let msg = "key1=sk-aaaa key2=sk-bbbb";
        let out = redact_if_key(msg);
        assert!(!out.contains("sk-aaaa"));
        assert!(!out.contains("sk-bbbb"));
        assert_eq!(out.matches("[REDACTED]").count(), 2);
    }

    #[test]
    fn no_key_pattern_returns_borrowed() {
        let msg = "plain error message with no key";
        let result = redact_if_key(msg);
        // Borrowed variant means no allocation happened.
        assert!(matches!(result, std::borrow::Cow::Borrowed(_)));
        assert_eq!(result, msg);
    }
}
