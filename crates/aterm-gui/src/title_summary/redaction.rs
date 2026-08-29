// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Secret-shape detection for the context lines and endpoints that may leave
//! this process: assignment keys, structured labels, URL userinfo, and raw
//! credential/JWT shapes.

use super::MAX_CONTEXT_BYTES;
use super::transport::{endpoint_authority, endpoint_host_is_valid};

pub(super) const MAX_SENSITIVE_SCAN_BYTES: usize = MAX_CONTEXT_BYTES;
pub(super) const MAX_SENSITIVE_FIELDS: usize = 64;
const MAX_SENSITIVE_LABEL_BYTES: usize = 128;

pub(super) fn redact_context_line(line: &str) -> String {
    if contains_sensitive_text(line) {
        "[redacted potentially sensitive line]".to_string()
    } else {
        line.to_string()
    }
}

/// Best-effort privacy boundary for terminal context and provider output.
///
/// This recognizes common credential shapes and structured labels, but no lexical
/// heuristic can guarantee semantic secret classification. Oversized or excessively
/// structured input is therefore rejected conservatively so work stays bounded and a
/// hostile line cannot move a credential beyond the inspected region.
pub(super) fn contains_sensitive_text(text: &str) -> bool {
    text.len() > MAX_SENSITIVE_SCAN_BYTES
        || has_sensitive_marker(text)
        || has_sensitive_assignment_key(text)
        || has_url_userinfo(text)
        || has_token_shape(text)
}

pub(super) fn has_sensitive_assignment_key(text: &str) -> bool {
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

/// Shared settings/runtime policy: endpoint parameters are never needed by the
/// supported APIs and are too easy to use as credential-bearing URL material.
pub(crate) fn endpoint_has_query_or_fragment(endpoint: &str) -> bool {
    endpoint.contains(['?', '#'])
}

/// Lexical Settings boundary for provider endpoints. Credentials belong only in
/// the private token-file field: reject userinfo, parameters, malformed schemes,
/// whitespace/control characters, invalid ports, and invalid host authorities
/// before an endpoint can be persisted to TOML.
pub(crate) fn endpoint_is_credential_free_absolute_url(endpoint: &str) -> bool {
    if endpoint.is_empty()
        || endpoint_has_query_or_fragment(endpoint)
        || endpoint.contains(['%', '\\'])
        || endpoint.chars().any(char::is_whitespace)
        || endpoint.chars().any(char::is_control)
        || contains_sensitive_text(endpoint)
    {
        return false;
    }
    let Some(uri) = aterm_http::Uri::parse(endpoint) else {
        return false;
    };
    let Some((raw_scheme, rest)) = endpoint.split_once("://") else {
        return false;
    };
    let raw_authority = rest.split('/').next().unwrap_or_default();
    let raw_path = rest.strip_prefix(raw_authority).unwrap_or_default();
    let path_is_rfc3986_ascii = raw_path.bytes().all(|byte| {
        byte.is_ascii_alphanumeric()
            || matches!(
                byte,
                b'-' | b'.'
                    | b'_'
                    | b'~'
                    | b'/'
                    | b'!'
                    | b'$'
                    | b'&'
                    | b'\''
                    | b'('
                    | b')'
                    | b'*'
                    | b'+'
                    | b','
                    | b';'
                    | b'='
                    | b':'
                    | b'@'
            )
    });
    // The parse must agree with the naive textual split of the SAME string.
    // An endpoint the two read differently is ambiguous, and an ambiguous
    // endpoint is refused rather than guessed at.
    if uri.scheme().as_str() != raw_scheme
        || uri.authority_as_written() != raw_authority
        || !(uri.path() == raw_path || (raw_path.is_empty() && uri.path() == "/"))
        || !path_is_rfc3986_ascii
    {
        return false;
    }
    let Some((_scheme, host, port)) = endpoint_authority(endpoint) else {
        return false;
    };
    let path = endpoint
        .split_once("://")
        .and_then(|(_, rest)| rest.find('/').map(|start| &rest[start..]))
        .unwrap_or_default();
    let credential_component = host
        .split('.')
        .chain(path.split('/'))
        .filter(|component| !component.is_empty())
        .any(looks_like_raw_credential);
    port != Some(0) && endpoint_host_is_valid(host) && !credential_component
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

fn has_token_shape(text: &str) -> bool {
    // Context lines and model descriptions should be natural-language phrases.
    // Conservatively redact token-shaped high-entropy strings even without a key.
    text.split(|ch: char| {
        ch.is_whitespace()
            || matches!(
                ch,
                '"' | '\'' | ',' | ';' | '(' | ')' | '[' | ']' | '{' | '}' | '<' | '>'
            )
    })
    .any(looks_like_raw_credential)
}

/// Shared conservative credential-shape detector for Settings and all prompt /
/// provider-output boundaries. It deliberately recognizes common fixed prefixes,
/// compact JWTs, exact SHA/token hex, and long lowercase+digit opaque tokens.
pub(crate) fn looks_like_raw_credential(value: &str) -> bool {
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
    let exact_hex = value.len() == 64 && bytes.iter().all(u8::is_ascii_hexdigit);
    let lowercase_digit = value.len() >= 32
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        && bytes.iter().any(u8::is_ascii_lowercase)
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
