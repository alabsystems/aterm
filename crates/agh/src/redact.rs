// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! The Redactor (design §6.6): token shapes, and the exact bytes of every credential this
//! process loaded.

use std::sync::{Arc, Mutex, PoisonError};

use aterm_uds::secret::Secret;

use crate::wire::door;

const REDACTED_TEXT: &str = "[redacted]";
const REDACTED: &[u8] = REDACTED_TEXT.as_bytes();
const TOKEN_PREFIXES: [&[u8]; 6] = [b"ghp_", b"gho_", b"ghu_", b"ghs_", b"ghr_", b"github_pat_"];
const SCHEMES: [&[u8]; 3] = [b"bearer", b"token", b"basic"];
const LEARN_MIN: usize = 8;
const BEARER_MIN: usize = 8;

/// Rewrites secrets out of text. Clones share one learned set.
#[derive(Clone, Default)]
pub struct Redactor {
    exact: Arc<Mutex<Vec<Secret>>>,
}

/// Whether `bytes` at `at` start with `prefix`, ASCII case-insensitively.
#[inline]
fn starts_ci(bytes: &[u8], at: usize, prefix: &[u8]) -> bool {
    bytes
        .get(at..at.saturating_add(prefix.len()))
        .is_some_and(|window| window.eq_ignore_ascii_case(prefix))
}

/// Finds the end of a token run starting at `at`, stopping at ASCII whitespace or an unescaped `"`.
fn run_end(bytes: &[u8], at: usize) -> usize {
    let mut end = at;
    let mut backslashes = 0usize;
    while let Some(&b) = bytes.get(end) {
        if b.is_ascii_whitespace() || (b == b'"' && backslashes.is_multiple_of(2)) {
            break;
        }
        backslashes = if b == b'\\' { backslashes + 1 } else { 0 };
        end += 1;
    }
    end
}

const ESCAPED_BLANKS: [&[u8]; 3] = [b"\\t", b"\\u0009", b"\\u0020"];

fn blank_at(bytes: &[u8], at: usize) -> usize {
    match bytes.get(at) {
        Some(b' ' | b'\t') => 1,
        _ => ESCAPED_BLANKS
            .iter()
            .find(|escape| bytes.get(at..).is_some_and(|tail| tail.starts_with(escape)))
            .map_or(0, |escape| escape.len()),
    }
}

fn skip_blanks(bytes: &[u8], at: usize) -> usize {
    let mut j = at;
    loop {
        match blank_at(bytes, j) {
            0 => return j,
            n => j += n,
        }
    }
}

/// The quote that may open a value: none, a `"`, or an escaped `\"` (a JSON text quoted
/// inside another string).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Quote {
    None,
    Plain,
    Escaped,
}

/// The quote that opens at `at`, and where the text after it starts.
#[inline]
fn open_quote(bytes: &[u8], at: usize) -> (Quote, usize) {
    match (bytes.get(at), bytes.get(at + 1)) {
        (Some(b'"'), _) => (Quote::Plain, at + 1),
        (Some(b'\\'), Some(b'"')) => (Quote::Escaped, at + 2),
        _ => (Quote::None, at),
    }
}

fn value_end(bytes: &[u8], at: usize, quote: Quote) -> usize {
    if quote != Quote::Escaped {
        return run_end(bytes, at);
    }
    let mut end = at;
    while let Some(&b) = bytes.get(end) {
        if b.is_ascii_whitespace() {
            break;
        }
        if b == b'"' {
            if end > at && bytes.get(end - 1) == Some(&b'\\') {
                end -= 1;
            }
            break;
        }
        end += 1;
    }
    end
}

/// Rule 1: matches a token prefix plus `[A-Za-z0-9_]+` at `at` and returns its end index.
fn token_at(bytes: &[u8], at: usize) -> Option<usize> {
    TOKEN_PREFIXES.iter().find_map(|prefix| {
        let tail = bytes.get(at..)?;
        if !tail.starts_with(prefix) {
            return None;
        }
        let start = at + prefix.len();
        let mut end = start;
        while bytes
            .get(end)
            .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_')
        {
            end += 1;
        }
        (end > start).then_some(end)
    })
}

/// Rule 2: matches `Authorization:` (including JSON-quoted forms) and an optional scheme word at `at`.
fn header_at(bytes: &[u8], at: usize) -> Option<(usize, usize)> {
    let name = door::AUTHORIZATION.as_bytes();
    if !starts_ci(bytes, at, name) {
        return None;
    }
    let (_, mut j) = open_quote(bytes, at + name.len());
    j = skip_blanks(bytes, j);
    if bytes.get(j) != Some(&b':') {
        return None;
    }
    let (mut quote, mut j) = open_quote(bytes, skip_blanks(bytes, j + 1));
    if let Some(scheme) = SCHEMES
        .iter()
        .find(|s| starts_ci(bytes, j, s) && blank_at(bytes, j + s.len()) > 0)
    {
        let (inner, after) = open_quote(bytes, skip_blanks(bytes, j + scheme.len()));
        if inner != Quote::None {
            quote = inner;
        }
        j = after;
    }
    let end = value_end(bytes, j, quote);
    (end > j).then_some((j, end))
}

/// Rule 3: matches `bearer <value>` (at least 8 bytes) at `at`.
fn bearer_at(bytes: &[u8], at: usize) -> Option<(usize, usize)> {
    let word = "bearer".len();
    if !starts_ci(bytes, at, b"bearer") || blank_at(bytes, at + word) == 0 {
        return None;
    }
    let (quote, start) = open_quote(bytes, skip_blanks(bytes, at + word));
    let end = value_end(bytes, start, quote);
    (end - start >= BEARER_MIN).then_some((start, end))
}

impl Redactor {
    /// Creates an empty redactor.
    #[inline]
    #[must_use]
    pub fn new() -> Redactor {
        Redactor::default()
    }

    /// Remembers `secret`'s exact bytes (values under 8 bytes or already learned are ignored).
    pub fn learn(&self, secret: &[u8]) {
        if secret.len() < LEARN_MIN {
            return;
        }
        let mut exact = self.exact.lock().unwrap_or_else(PoisonError::into_inner);
        if exact.iter().any(|known| known.expose() == secret) {
            return;
        }
        exact.push(Secret::new(secret));
    }

    /// Redacts every learned value (longest first), then applies [`Redactor::shapes`].
    #[inline]
    #[must_use]
    pub fn apply(&self, text: &str) -> String {
        Redactor::shapes(&self.learned(text))
    }

    /// Redacts learned values and context-free token prefixes (rule 1) without cross-field rules 2/3.
    #[inline]
    #[must_use]
    pub fn learned_and_tokens(&self, text: &str) -> String {
        scan(&self.learned(text), false)
    }

    fn learned(&self, text: &str) -> String {
        let exact = self.exact.lock().unwrap_or_else(PoisonError::into_inner);
        let mut known: Vec<&str> = exact
            .iter()
            .filter_map(|secret| std::str::from_utf8(secret.expose()).ok())
            .collect();
        if known.is_empty() {
            return text.to_string();
        }
        known.sort_by_key(|value| std::cmp::Reverse(value.len()));
        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        while !rest.is_empty() {
            let matched = known.iter().copied().find(|value| rest.starts_with(value));
            if rest.starts_with(REDACTED_TEXT)
                && matched.is_none_or(|value| value.len() <= REDACTED_TEXT.len())
            {
                out.push_str(REDACTED_TEXT);
                rest = &rest[REDACTED_TEXT.len()..];
            } else if let Some(matched) = matched {
                out.push_str(REDACTED_TEXT);
                rest = &rest[matched.len()..];
            } else {
                let ch = rest.chars().next().expect("rest is non-empty");
                out.push(ch);
                rest = &rest[ch.len_utf8()..];
            }
        }
        out
    }

    /// Redacts token shapes (`ghp_`/`gho_`/`ghu_`/`ghs_`/`ghr_`/`github_pat_`),
    /// `Authorization:` header values, and `bearer <8+ bytes>` runs into `[redacted]`.
    #[inline]
    #[must_use]
    pub fn shapes(text: &str) -> String {
        scan(text, true)
    }
}

fn scan(text: &str, context: bool) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while let Some(&b) = bytes.get(at) {
        if let Some(end) = token_at(bytes, at) {
            out.extend_from_slice(REDACTED);
            at = end;
        } else if let Some((keep, end)) = context
            .then(|| header_at(bytes, at).or_else(|| bearer_at(bytes, at)))
            .flatten()
        {
            out.extend_from_slice(&bytes[at..keep]);
            out.extend_from_slice(REDACTED);
            at = end;
        } else {
            out.push(b);
            at += 1;
        }
    }
    String::from_utf8(out).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
}

impl std::fmt::Debug for Redactor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let n = self
            .exact
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len();
        write!(f, "Redactor({n} learned)")
    }
}
