// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! The Redactor (design §6.6): token shapes, and the exact bytes of every credential this
//! process loaded.

use std::sync::{Arc, Mutex, PoisonError};

use aterm_uds::secret::Secret;

use crate::wire::door;

/// What every match becomes.
const REDACTED_TEXT: &str = "[redacted]";
/// [`REDACTED_TEXT`]'s bytes, for the scanner.
const REDACTED: &[u8] = REDACTED_TEXT.as_bytes();
/// The token prefixes rule 1 looks for.
const TOKEN_PREFIXES: [&[u8]; 6] = [b"ghp_", b"gho_", b"ghu_", b"ghs_", b"ghr_", b"github_pat_"];
/// The scheme words rule 2 passes over.
const SCHEMES: [&[u8]; 3] = [b"bearer", b"token", b"basic"];
/// A value under this many bytes is not learned.
const LEARN_MIN: usize = 8;
/// Rule 3's least run after `bearer `.
const BEARER_MIN: usize = 8;

/// Rewrites secrets out of text. Clones share one learned set.
#[derive(Clone, Default)]
pub struct Redactor {
    exact: Arc<Mutex<Vec<Secret>>>,
}

/// Whether `bytes` at `at` start with `prefix`, ASCII case-insensitively.
fn starts_ci(bytes: &[u8], at: usize, prefix: &[u8]) -> bool {
    bytes
        .get(at..at.saturating_add(prefix.len()))
        .is_some_and(|window| window.eq_ignore_ascii_case(prefix))
}

/// The end of the run that starts at `at`: it stops at ASCII whitespace, or at a `"` that no
/// odd run of `\` escapes. So a quoted value's closing quote survives (`why="… bearer x…"`),
/// while an escaped quote inside a value (`abc\"def`) does not cut the match short.
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

/// The JSON escapes of a tab or a space, which a JSON text carries in place of the byte.
const ESCAPED_BLANKS: [&[u8]; 3] = [b"\\t", b"\\u0009", b"\\u0020"];

/// The length of the space or tab at `at`, raw (one byte) or JSON-escaped (`\t`, `\u0009`,
/// `\u0020`); 0 when there is none.
fn blank_at(bytes: &[u8], at: usize) -> usize {
    match bytes.get(at) {
        Some(b' ' | b'\t') => 1,
        _ => ESCAPED_BLANKS
            .iter()
            .find(|escape| bytes.get(at..).is_some_and(|tail| tail.starts_with(escape)))
            .map_or(0, |escape| escape.len()),
    }
}

/// `at` moved past any spaces and tabs, raw or JSON-escaped.
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
fn open_quote(bytes: &[u8], at: usize) -> (Quote, usize) {
    match (bytes.get(at), bytes.get(at + 1)) {
        (Some(b'"'), _) => (Quote::Plain, at + 1),
        (Some(b'\\'), Some(b'"')) => (Quote::Escaped, at + 2),
        _ => (Quote::None, at),
    }
}

/// The end of the value run at `at` that `quote` opened. After an escaped `\"` the run ends
/// at ASCII whitespace or at the `\"` that closes it; otherwise as `run_end`.
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

/// Rule 1: a token prefix and at least one of `[A-Za-z0-9_]` at `at`; the token's end.
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

/// Rule 2: the credential header's name, `:`, an optional scheme word, at `at`; where the
/// value starts and ends. The JSON form (`"name":"scheme value"`) is read too, and so is
/// that form quoted inside another string (`\"name\":\"scheme value\"`): a `"` or `\"` may
/// follow the name and open the value, and one more may open the value after the scheme
/// word (`Bearer "value"`). The run ends at the quote that opened it.
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

/// Rule 3: `bearer`, spaces or tabs, and a run of at least 8 bytes at `at`, which a `"` or
/// `\"` may open; where the run starts and ends (inside the quotes).
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
    /// A redactor that has learned nothing.
    #[must_use]
    pub fn new() -> Redactor {
        Redactor::default()
    }

    /// Remember `secret`'s exact bytes. A value under 8 bytes is ignored; a value already
    /// learned is not stored twice.
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

    /// Every learned value (longest first, where its bytes are UTF-8), then `shapes`.
    #[must_use]
    pub fn apply(&self, text: &str) -> String {
        Redactor::shapes(&self.learned(text))
    }

    /// Every learned value, then rule 1 of `shapes` alone: the matches that need no context.
    /// For a line joined from values that each passed `apply` already (a receipt, a
    /// heartbeat): rules 2 and 3 read the text after a word, so over a joined line they would
    /// match across a field boundary (`repo=o/torchbearer as=anonymous`) and eat the next
    /// field. Neither a learned value nor a token can span a ` key=` boundary.
    #[must_use]
    pub fn learned_and_tokens(&self, text: &str) -> String {
        scan(&self.learned(text), false)
    }

    /// `text` with every learned value (longest first, where its bytes are UTF-8) replaced.
    fn learned(&self, text: &str) -> String {
        let mut out = text.to_string();
        let exact = self.exact.lock().unwrap_or_else(PoisonError::into_inner);
        let mut known: Vec<&str> = exact
            .iter()
            .filter_map(|secret| std::str::from_utf8(secret.expose()).ok())
            .collect();
        known.sort_by_key(|value| std::cmp::Reverse(value.len()));
        for value in known {
            if out.contains(value) {
                out = out.replace(value, REDACTED_TEXT);
            }
        }
        out
    }

    /// The shapes alone, in this order:
    /// 1. `ghp_`, `gho_`, `ghu_`, `ghs_`, `ghr_` or `github_pat_`, followed by at least one
    ///    of `[A-Za-z0-9_]`: the whole token;
    /// 2. the header name (`door::AUTHORIZATION`, ASCII case-insensitive), an optional `"` or
    ///    `\"`, optional spaces and tabs, `:`, optional spaces and tabs, an optional `"` or
    ///    `\"`, then an optional scheme word (`bearer`, `token` or `basic`, any case), at least
    ///    one space or tab and an optional `"` or `\"`: the run after them;
    /// 3. `bearer` (any case), at least one space or tab, an optional `"` or `\"`, then a run
    ///    of at least 8 bytes: that run.
    ///
    /// A space or tab in rules 2 and 3 may also be JSON-escaped (`\t`, `\u0009`, `\u0020`), as
    /// it is in a JSON text that quotes the header or the word.
    ///
    /// Each match becomes `[redacted]`. A byte scanner; no regex. Prose such as `token file`
    /// or `the bearer of` is left alone. A run ends at ASCII whitespace, or at a `"` that no
    /// odd run of `\` escapes (after an opening `\"`, at the closing `\"`), so a quoted value
    /// keeps its closing quote and the scan of an already redacted text changes nothing.
    #[must_use]
    pub fn shapes(text: &str) -> String {
        scan(text, true)
    }
}

/// The byte scanner: rule 1 of [`Redactor::shapes`] always, rules 2 and 3 (which read the
/// text after a word) when `context`.
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
