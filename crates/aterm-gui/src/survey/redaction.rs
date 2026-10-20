// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Secret-shape detection for the survey's excerpts, the text that leaves the
//! session ([`super::redact`]): assignment keys, structured labels, URL
//! userinfo, and raw credential/JWT shapes. Kept from the retired Smart
//! Titles' privacy boundary (`title_summary::redaction`, 64b1d6833), whose
//! context lines it guarded the same way; its endpoint checks went with it.

/// A line longer than this is withheld whole rather than scanned (Smart
/// Titles' context cap, 12 KiB): the work stays bounded, and no credential
/// hides past the inspected region.
const MAX_SENSITIVE_SCAN_BYTES: usize = 12 * 1024;
const MAX_SENSITIVE_FIELDS: usize = 64;
const MAX_SENSITIVE_LABEL_BYTES: usize = 128;

/// Best-effort privacy boundary for an excerpt that leaves the session, after
/// [`super::redact`]'s word pass: a sensitive marker, a sensitive assignment
/// key, URL userinfo, or more than can be scanned. The token-shape rule —
/// each token split at [`is_token_break`] and judged by
/// [`looks_like_raw_credential`] — is that word pass, which replaces a token
/// instead of the line and judges a path a segment at a time, so it is not
/// run over the line again.
///
/// This recognizes common credential shapes and structured labels, but no lexical
/// heuristic can guarantee semantic secret classification. Oversized or excessively
/// structured input is therefore rejected conservatively so work stays bounded and a
/// hostile line cannot move a credential beyond the inspected region.
pub(super) fn contains_sensitive_phrase(text: &str) -> bool {
    text.len() > MAX_SENSITIVE_SCAN_BYTES
        || has_sensitive_marker(text)
        || has_sensitive_assignment_key(text)
        || has_url_userinfo(text)
}

fn has_sensitive_assignment_key(text: &str) -> bool {
    let mut fields = 0usize;
    for (separator, _) in text.match_indices(['=', ':']) {
        fields = fields.saturating_add(1);
        if fields > MAX_SENSITIVE_FIELDS {
            // Fail closed rather than let an attacker hide a key after the work cap.
            return true;
        }
        if has_structured_sensitive_label(&text[..separator]) {
            return true;
        }
    }
    false
}

fn has_structured_sensitive_label(prefix: &str) -> bool {
    let prefix = prefix.trim_end();
    let Some(last) = prefix.as_bytes().last().copied() else {
        return false;
    };

    if matches!(last, b'\'' | b'"') {
        let before_quote = &prefix[..prefix.len() - 1];
        if let Some(open) = before_quote.rfind(char::from(last)) {
            let label = &before_quote[open + 1..];
            return label.len() > MAX_SENSITIVE_LABEL_BYTES
                || is_sensitive_identifier_label(label)
                || is_legacy_sensitive_assignment_label(label);
        }
    }

    let Some((label, start, overlong)) = trailing_identifier(prefix) else {
        return false;
    };
    if overlong
        || is_sensitive_identifier_label(label)
        || is_legacy_sensitive_assignment_label(label)
    {
        return true;
    }

    // YAML permits unquoted multi-word keys (`private key: value`). Only extend a
    // bare `key` by one identifier so unrelated prose before an assignment does not
    // become part of the label.
    if label.eq_ignore_ascii_case("key")
        && let Some((modifier, _, modifier_overlong)) = trailing_identifier(&prefix[..start])
    {
        return modifier_overlong
            || matches_sensitive_key_modifier(modifier)
            || is_legacy_sensitive_assignment_label(modifier);
    }
    false
}

fn trailing_identifier(text: &str) -> Option<(&str, usize, bool)> {
    let text = text.trim_end();
    let end = text.len();
    let bytes = text.as_bytes();
    let mut start = end;
    while start > 0
        && (bytes[start - 1].is_ascii_alphanumeric()
            || matches!(bytes[start - 1], b'_' | b'-' | b'.'))
    {
        start -= 1;
    }
    (start < end).then(|| {
        (
            &text[start..end],
            start,
            end.saturating_sub(start) > MAX_SENSITIVE_LABEL_BYTES,
        )
    })
}

fn identifier_words(label: &str) -> Vec<String> {
    let mut words = Vec::with_capacity(4);
    let mut word = String::new();
    let mut previous = None;
    let mut chars = label.chars().peekable();
    while let Some(ch) = chars.next() {
        if !ch.is_ascii_alphanumeric() {
            if !word.is_empty() {
                words.push(std::mem::take(&mut word));
            }
            previous = None;
            continue;
        }
        let camel_boundary = !word.is_empty()
            && ch.is_ascii_uppercase()
            && (previous
                .is_some_and(|prev: char| prev.is_ascii_lowercase() || prev.is_ascii_digit())
                || (previous.is_some_and(|prev: char| prev.is_ascii_uppercase())
                    && chars.peek().is_some_and(|next| next.is_ascii_lowercase())));
        if camel_boundary {
            words.push(std::mem::take(&mut word));
        }
        word.push(ch.to_ascii_lowercase());
        previous = Some(ch);
    }
    if !word.is_empty() {
        words.push(word);
    }
    words
}

fn is_sensitive_identifier_label(label: &str) -> bool {
    let words = identifier_words(label);
    let sensitive_word = words.iter().any(|word| {
        matches!(
            word.as_str(),
            "secret"
                | "secrets"
                | "password"
                | "passwd"
                | "token"
                | "credential"
                | "credentials"
                | "apikey"
                | "privatekey"
                | "accesskey"
                | "secretkey"
                | "authorization"
        )
    });
    sensitive_word
        || words.windows(2).any(|pair| {
            pair[1] == "key" && matches!(pair[0].as_str(), "api" | "private" | "access" | "secret")
        })
}

fn matches_sensitive_key_modifier(label: &str) -> bool {
    matches!(
        label.to_ascii_lowercase().as_str(),
        "api" | "private" | "access" | "secret"
    )
}

fn is_legacy_sensitive_assignment_label(label: &str) -> bool {
    let key = label.trim().to_ascii_lowercase();
    matches!(
        key.as_str(),
        "database_url"
            | "database_uri"
            | "db_url"
            | "db_uri"
            | "redis_url"
            | "redis_uri"
            | "mongodb_url"
            | "mongodb_uri"
            | "mongo_url"
            | "mongo_uri"
            | "dsn"
            | "connection_string"
            | "mysql_pwd"
            | "pgpassword"
    )
}

fn has_url_userinfo(text: &str) -> bool {
    let mut remainder = text;
    while let Some((_, after_scheme)) = remainder.split_once("://") {
        let authority = after_scheme
            .split(|ch: char| ch.is_whitespace() || matches!(ch, '/' | '?' | '#'))
            .next()
            .unwrap_or_default();
        if authority
            .split_once('@')
            .is_some_and(|(userinfo, host)| !userinfo.is_empty() && !host.is_empty())
        {
            return true;
        }
        remainder = after_scheme;
    }
    false
}

fn has_sensitive_marker(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    const SENSITIVE: &[&str] = &["sk_live_", "sk-proj-"];
    SENSITIVE.iter().any(|needle| lower.contains(needle))
        || contains_ascii_word(&lower, "bearer")
        || contains_ascii_word(&lower, "password")
        || contains_ascii_word(&lower, "passwd")
        || contains_ascii_word(&lower, "apikey")
        || contains_ascii_word_pair(&lower, "api", "key")
        || contains_ascii_word_pair(&lower, "access", "token")
        || contains_ascii_word_pair(&lower, "auth", "token")
        || contains_ascii_word_pair(&lower, "private", "key")
}

fn contains_ascii_word(text: &str, wanted: &str) -> bool {
    text.split(|ch: char| !ch.is_ascii_alphanumeric())
        .any(|word| word == wanted)
}

fn contains_ascii_word_pair(text: &str, first: &str, second: &str) -> bool {
    let mut previous = None;
    for word in text
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
    {
        if previous == Some(first) && word == second {
            return true;
        }
        previous = Some(word);
    }
    false
}

/// Where the token-shape rule splits a line into the tokens it judges with
/// [`looks_like_raw_credential`]: whitespace, quotes and the punctuation that
/// wraps a value in prose or structured text. An excerpt is natural-language
/// prose, so a token-shaped high-entropy string is taken for a credential even
/// without a key.
pub(super) fn is_token_break(ch: char) -> bool {
    ch.is_whitespace()
        || matches!(
            ch,
            '"' | '\'' | ',' | ';' | '(' | ')' | '[' | ']' | '{' | '}' | '<' | '>'
        )
}

/// Conservative credential-shape detector for one word. It deliberately
/// recognizes common fixed prefixes, compact JWTs, exact SHA/token hex, and long
/// lowercase+digit, uppercase+digit and mixed-case opaque tokens.
pub(super) fn looks_like_raw_credential(value: &str) -> bool {
    let value = value.trim().trim_matches(|ch: char| {
        !ch.is_ascii_alphanumeric() && !matches!(ch, '_' | '-' | '.' | '+' | '/' | '=')
    });
    if value.is_empty() || value.chars().any(char::is_whitespace) {
        return false;
    }
    let bytes = value.as_bytes();
    let token_chars = || {
        bytes.iter().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(*byte, b'_' | b'-' | b'+' | b'/' | b'=')
        })
    };
    let lower = value.to_ascii_lowercase();
    let known_prefix = [
        "sk-",
        "sk_",
        "rk-",
        "rk_",
        "xoxb-",
        "xoxb_",
        "xoxp-",
        "xoxp_",
        "ghp_",
        "github_pat_",
    ]
    .iter()
    .any(|prefix| lower.starts_with(prefix))
        && value.len() >= 16
        && token_chars();
    let aws_access_key = value.len() == 20
        && (value.starts_with("AKIA") || value.starts_with("ASIA"))
        && bytes[4..]
            .iter()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit());
    let google_api_key = value.starts_with("AIza") && value.len() >= 20 && token_chars();
    let exact_hex = aterm_codec::hex::is_hex(value, 64);
    let lowercase_digit = value.len() >= 32
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        && bytes.iter().any(u8::is_ascii_lowercase)
        && bytes.iter().any(u8::is_ascii_digit);
    let uppercase_digit = value.len() >= 32
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
        && bytes.iter().any(u8::is_ascii_uppercase)
        && bytes.iter().any(u8::is_ascii_digit);
    let mixed_opaque = value.len() >= 32
        && token_chars()
        && bytes.iter().any(u8::is_ascii_lowercase)
        && bytes.iter().any(u8::is_ascii_uppercase)
        && bytes.iter().any(u8::is_ascii_digit);
    known_prefix
        || aws_access_key
        || google_api_key
        || exact_hex
        || lowercase_digit
        || uppercase_digit
        || mixed_opaque
        || looks_like_compact_jwt(value)
}

fn looks_like_compact_jwt(value: &str) -> bool {
    if value.len() < 32 {
        return false;
    }
    let mut parts = value.split('.');
    let Some(header) = parts.next() else {
        return false;
    };
    let Some(payload) = parts.next() else {
        return false;
    };
    let Some(signature) = parts.next() else {
        return false;
    };
    if parts.next().is_some()
        || !header.starts_with("eyJ")
        || payload.is_empty()
        || signature.is_empty()
    {
        return false;
    }
    [header, payload, signature].iter().all(|part| {
        part.bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    })
}

#[cfg(test)]
mod tests {
    //! The corpora of the retired Smart Titles' privacy tests
    //! (`title_summary.rs`'s `likely_secret_lines_are_removed_before_prompting`
    //! and `structured_secret_labels_are_identifier_aware_and_bounded`), kept
    //! on the predicates here and asserted through the survey's own
    //! [`redact`] — the path every excerpt takes — so a change to its word
    //! pass that lets a corpus credential through fails here; the asserts on
    //! what went with Smart Titles (its context lines, prompt and provider
    //! checks) went with it.

    use super::super::redact;
    use super::*;

    /// Whether [`redact`] changed `text`: it replaced a word or withheld the
    /// line.
    fn redacted(text: &str) -> bool {
        redact(text) != text
    }

    #[test]
    fn credential_shapes_and_userinfo_lines_are_sensitive_and_prose_is_not() {
        let stripe_shaped = concat!("sk_", "live_A1B2C3D4E5F6G7H8I9J0K1L2M3N4");
        assert!(redacted("export API_KEY=very-secret"));
        assert!(redacted(stripe_shaped));
        assert!(redacted(&format!("Completed {stripe_shaped}")));
        assert!(!redacted("cargo test"));

        for secret_line in [
            "DATABASE_URL=postgres://alice:p4ssw0rd@db.example/prod",
            "export REDIS_URL=redis://cache.example/0",
            "connecting to postgres://alice:p4ssw0rd@db.example/prod",
            "API_KEY=very-secret cargo test",
            concat!("sk-", "proj-abcdefghijklmnopqrstuvwxyz0123456789"),
        ] {
            assert!(redacted(secret_line), "missed {secret_line}");
        }

        for credential in [
            concat!("sk-", "abcdefghijklmnopqrstuvwxyz012345"),
            concat!("sk", "_abcdefghijklmnopqrstuvwxyz012345"),
            "rk-abcdefghijklmnopqrstuvwxyz012345",
            "RK_ABCDEFGHIJKLMNOPQRSTUVWXYZ012345",
            concat!("xoxb-", "1234567890-abcdefghijklmnopqrstuvwxyz"),
            "xoxb_1234567890_abcdefghijklmnopqrstuvwxyz",
            concat!("xoxp-", "1234567890-abcdefghijklmnopqrstuvwxyz"),
            "xoxp_1234567890_abcdefghijklmnopqrstuvwxyz",
            concat!("ghp_", "abcdefghijklmnopqrstuvwxyz0123456789"),
            concat!("github_pat_", "abcdefghijklmnopqrstuvwxyz0123456789"),
            concat!("AKIA", "IOSFODNN7EXAMPLE"),
            "ASIAIOSFODNN7EXAMPLE",
            concat!("AIza", "SyD-abcdefghijklmnopqrstuvwxyz012345"),
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            "abcdefghijklmnopqrstuvwxyz0123456789",
            concat!(
                "eyJhbGciOiJIUzI1NiJ9",
                ".eyJzdWIiOiIxMjM0NTY3ODkwIn0",
                ".signature_value"
            ),
        ] {
            assert!(looks_like_raw_credential(credential), "missed {credential}");
            assert!(redacted(credential), "did not redact {credential}");
            // At a sentence's end, in a path, and quoted: never out whole.
            for line in [
                format!("pushed with {credential}."),
                format!("cat /tmp/{credential}/env"),
                format!("({credential}),"),
            ] {
                let shown = redact(&line);
                assert!(!shown.contains(credential), "{line} -> {shown}");
            }
        }
        for benign in [
            "api-version",
            "running-rust-tests",
            "0123456789abcdef",
            "Ready in project2026",
            "three.part.words",
            "risk-analysis",
        ] {
            assert!(
                !looks_like_raw_credential(benign),
                "false positive {benign}"
            );
            assert!(!redacted(benign), "privacy false positive in {benign}");
        }
    }

    #[test]
    fn structured_secret_labels_are_identifier_aware_and_bounded() {
        for sensitive in [
            "STRIPE_SECRET = sample-value",
            "export STRIPE_SECRET=sample-value",
            r#"{"STRIPE_SECRET":"sample-value"}"#,
            r#"{"stripeSecret":"sample-value"}"#,
            "dbPassword: sample-value",
            "service_passwd = sample-value",
            "refresh_token: sample-value",
            "api key: sample-value",
            r#"{"private key":"sample-value"}"#,
            "AWS_ACCESS_KEY_ID: sample-value",
            "GOOGLE_APPLICATION_CREDENTIALS=sample-value",
            "authorization: sample-value",
            "DATABASE_URL = sample-value",
        ] {
            assert!(
                has_sensitive_assignment_key(sensitive),
                "missed structured label in {sensitive}"
            );
            assert!(
                redacted(sensitive),
                "did not redact structured label in {sensitive}"
            );
        }

        for benign in [
            "keyboard_layout=ansi",
            "monkey: banana",
            r#"{"keyboard":"ansi"}"#,
            "tokenizer: enabled",
            "access_tokenizer: enabled",
            "auth_tokenizer: enabled",
            "apikeyboard=true",
            "bearerless=true",
            "passwordless=true",
            "secretary=present",
            "private keyboard: enabled",
            "access keyboard: enabled",
            "api keyboard: enabled",
            "A monkey uses a keyboard",
            "cargo test --package token-parser",
        ] {
            assert!(
                !has_sensitive_assignment_key(benign),
                "identifier false positive in {benign}"
            );
            assert!(!redacted(benign), "privacy false positive in {benign}");
        }

        // Past the bounds a line is withheld whole, its content unread.
        let oversized = "keyboard ".repeat(MAX_SENSITIVE_SCAN_BYTES / 2);
        assert!(oversized.len() > MAX_SENSITIVE_SCAN_BYTES);
        assert!(redacted(&oversized));
        assert!(!redacted(&oversized[..MAX_SENSITIVE_SCAN_BYTES]));

        let excessive_fields = "safe=1:".repeat(MAX_SENSITIVE_FIELDS + 1);
        assert!(has_sensitive_assignment_key(&excessive_fields));
        assert!(redacted(&excessive_fields));
        let within = "safe=1:".repeat(MAX_SENSITIVE_FIELDS / 2);
        assert!(!has_sensitive_assignment_key(&within));
    }
}
