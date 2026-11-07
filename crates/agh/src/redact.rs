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
/// The shortest base64 run rule 4 redacts.
const BASIC_MIN: usize = 16;

/// One learned secret: its exact bytes, and the unpadded base64 of the Basic credential git
/// sends for it, when it could be encoded.
struct Learned {
    exact: Secret,
    basic: Option<Secret>,
}

/// Rewrites secrets out of text. Clones share one learned set.
#[derive(Clone, Default)]
pub struct Redactor {
    exact: Arc<Mutex<Vec<Learned>>>,
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

    /// Remembers two needles of `secret` (values under 8 bytes or already learned are
    /// ignored): its exact bytes, and the unpadded standard base64 of `x-access-token:`
    /// followed by it, the payload of the Basic credential git sends under agh's helper
    /// username. The unpadded needle also matches the padded text, of which it is a prefix. A
    /// secret too large to encode (over 64 MiB, which no token reaches) keeps the first needle
    /// alone.
    pub fn learn(&self, secret: &[u8]) {
        if secret.len() < LEARN_MIN {
            return;
        }
        let mut exact = self.exact.lock().unwrap_or_else(PoisonError::into_inner);
        if exact.iter().any(|known| known.exact.expose() == secret) {
            return;
        }
        // The username git sends a helper's token under, before the token, in a Basic
        // credential: the helper's own constant, so the needle follows what git sends. Both
        // buffers hold the token (the second as its base64), so each is scrubbed: the first
        // is sized once, so no growth leaves a copy behind, and the second goes whole,
        // spare capacity included, into `Secret::from_vec`.
        let user = crate::git::GIT_USERNAME.as_bytes();
        let mut credential = Vec::with_capacity(user.len() + 1 + secret.len());
        credential.extend_from_slice(user);
        credential.push(b':');
        credential.extend_from_slice(secret);
        let basic = aterm_codec::base64::encode(&credential)
            .ok()
            .map(|encoded| {
                let mut bytes = encoded.into_bytes();
                let unpadded = bytes
                    .iter()
                    .rposition(|byte| *byte != b'=')
                    .map_or(0, |last| last + 1);
                bytes.truncate(unpadded);
                Secret::from_vec(bytes)
            });
        aterm_uds::secret::scrub(&mut credential);
        exact.push(Learned {
            exact: Secret::new(secret),
            basic,
        });
    }

    /// Redacts every learned value (longest first), then applies [`Redactor::shapes`].
    #[inline]
    #[must_use]
    pub fn apply(&self, text: &str) -> String {
        self.apply_cow(text).into_owned()
    }

    /// Borrowing variant of [`Redactor::apply`]: returns [`std::borrow::Cow::Borrowed`] when no
    /// learned secret or credential shape matches in `text`.
    #[inline]
    #[must_use]
    pub fn apply_cow<'a>(&self, text: &'a str) -> std::borrow::Cow<'a, str> {
        scan(self.learned(text), true)
    }

    /// Redacts learned values and context-free token prefixes (rule 1) without the context rules
    /// (2-4), which read across fields: a receipt line's `repo=acme/torchbearer as=anonymous` is
    /// no `bearer <value>`. Borrows when nothing matches.
    #[inline]
    #[must_use]
    pub fn learned_and_tokens<'a>(&self, text: &'a str) -> std::borrow::Cow<'a, str> {
        scan(self.learned(text), false)
    }

    fn learned<'a>(&self, text: &'a str) -> std::borrow::Cow<'a, str> {
        if text.len() < LEARN_MIN {
            return std::borrow::Cow::Borrowed(text);
        }
        let exact = self.exact.lock().unwrap_or_else(PoisonError::into_inner);
        if exact.is_empty() {
            return std::borrow::Cow::Borrowed(text);
        }
        let mut known: Vec<&str> = exact
            .iter()
            .flat_map(|learned| std::iter::once(&learned.exact).chain(learned.basic.as_ref()))
            .filter_map(|secret| std::str::from_utf8(secret.expose()).ok())
            .filter(|value| text.contains(value))
            .collect();
        if known.is_empty() {
            return std::borrow::Cow::Borrowed(text);
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
        std::borrow::Cow::Owned(out)
    }

    /// Redacts into `[redacted]`: token shapes (`ghp_`/`gho_`/`ghu_`/`ghs_`/`ghr_`/
    /// `github_pat_`, rule 1), `Authorization:` header values (rule 2), `bearer <8+ bytes>`
    /// runs (rule 3), and Basic credentials (rule 4): `basic` (ASCII case-insensitive), a space
    /// or tab (raw or JSON-escaped), an optional `"` or `\"`, then a run of at least 16 bytes
    /// of `[A-Za-z0-9+/=]` holding one byte outside `[A-Za-z]`. Prose such as
    /// `the basic configuration` is left alone: a run of letters alone, or under 16 bytes.
    #[inline]
    #[must_use]
    pub fn shapes(text: &str) -> String {
        Self::shapes_cow(text).into_owned()
    }

    /// Borrowing variant of [`Redactor::shapes`]: returns [`std::borrow::Cow::Borrowed`] when no
    /// credential shape matches in `text`.
    #[inline]
    #[must_use]
    pub fn shapes_cow(text: &str) -> std::borrow::Cow<'_, str> {
        scan(std::borrow::Cow::Borrowed(text), true)
    }
}

/// Rule 4: matches `basic <base64 run>` at `at` and returns the run's start and end.
fn basic_at(bytes: &[u8], at: usize) -> Option<(usize, usize)> {
    let word = "basic".len();
    if !starts_ci(bytes, at, b"basic") || blank_at(bytes, at + word) == 0 {
        return None;
    }
    let (_, start) = open_quote(bytes, skip_blanks(bytes, at + word));
    let mut end = start;
    while bytes
        .get(end)
        .is_some_and(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'='))
    {
        end += 1;
    }
    let run = &bytes[start..end];
    (run.len() >= BASIC_MIN && run.iter().any(|b| !b.is_ascii_alphabetic())).then_some((start, end))
}

fn scan(text: std::borrow::Cow<'_, str>, context: bool) -> std::borrow::Cow<'_, str> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut flushed = 0;
    let mut at = 0;
    while let Some(&b) = bytes.get(at) {
        let matched = if b == b'g' {
            token_at(bytes, at).map(|end| (at, end))
        } else if context && matches!(b, b'a' | b'A') {
            header_at(bytes, at)
        } else if context && matches!(b, b'b' | b'B') {
            bearer_at(bytes, at).or_else(|| basic_at(bytes, at))
        } else {
            None
        };
        if let Some((keep, end)) = matched {
            if flushed == 0 {
                out.reserve(bytes.len());
            }
            out.extend_from_slice(&bytes[flushed..keep]);
            out.extend_from_slice(REDACTED);
            flushed = end;
            at = end;
        } else {
            at += 1;
        }
    }
    if flushed == 0 {
        return text;
    }
    out.extend_from_slice(&bytes[flushed..]);
    std::borrow::Cow::Owned(
        String::from_utf8(out)
            .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned()),
    )
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
