// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Control wire request line parser for optional `@<selector>`, verb, and payload.

use core::ops::Range;

/// A stable session id as the wire spells it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Sid<'a>(&'a str);

impl<'a> Sid<'a> {
    /// `s` as a session id, or `None` when it is not one.
    #[inline]
    #[must_use]
    pub fn parse(s: &'a str) -> Option<Self> {
        let hex = s.strip_prefix("s-")?;
        let digit = |b: u8| b.is_ascii_digit() || (b'a'..=b'f').contains(&b);
        (!hex.is_empty() && hex.bytes().all(digit)).then_some(Self(s))
    }

    /// The id as the wire and the rendezvous directory spell it.
    #[inline]
    #[must_use]
    pub const fn as_str(&self) -> &'a str {
        self.0
    }
}

/// A parsed `@<selector>`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Selector<'a> {
    /// `@.`: the connection's own session.
    SelfTok,
    /// `@<digits>`: a process-local session number.
    Local(u64),
    /// `@s-<hex>`: a stable session id.
    Sid(Sid<'a>),
    /// Any other body (`@S-1`, `@./s-1`, `@self`, a number past `u64`, an empty one).
    Invalid(&'a str),
}

impl<'a> Selector<'a> {
    /// Classify the body after the leading `@`.
    #[inline]
    #[must_use]
    pub fn parse(body: &'a str) -> Self {
        if body == "." {
            Self::SelfTok
        } else if !body.is_empty() && body.bytes().all(|b| b.is_ascii_digit()) {
            body.parse().map_or(Self::Invalid(body), Self::Local)
        } else {
            Sid::parse(body).map_or(Self::Invalid(body), Self::Sid)
        }
    }

    /// Whether it names the connection's own session.
    #[inline]
    #[must_use]
    pub const fn is_self(&self) -> bool {
        matches!(self, Self::SelfTok)
    }
}

/// A request line read by the grammar ([`head`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Head<'a> {
    /// The leading `@<selector>`, if the line has one.
    pub selector: Option<Selector<'a>>,
    /// The verb word (empty for a blank line or a lone selector).
    pub verb: &'a str,
    /// Everything after the one separator that ends the verb.
    pub rest: &'a str,
}

/// Where a line's [`Head`] lies in it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Spans {
    /// The selector's body (after the `@`).
    pub selector: Option<Range<usize>>,
    /// The verb word.
    pub verb: Range<usize>,
    /// The rest.
    pub rest: Range<usize>,
}

/// Whether `b` separates words.
#[inline]
const fn is_separator(b: u8) -> bool {
    b == b' ' || b == b'\t'
}

/// The first index from `at` (below `end`) that is not a separator, or `end`.
#[inline]
const fn skip_separators(bytes: &[u8], mut at: usize, end: usize) -> usize {
    while at < end && is_separator(bytes[at]) {
        at += 1;
    }
    at
}

/// The first index from `at` (below `end`) that is a separator, or `end`.
#[inline]
const fn word_end(bytes: &[u8], mut at: usize, end: usize) -> usize {
    while at < end && !is_separator(bytes[at]) {
        at += 1;
    }
    at
}

impl Spans {
    /// Read `line` by the grammar: words separated by SP or HT, separators skipped before the
    /// first word and after the selector, a trailing `\r` not part of the line, `rest` all after
    /// the one separator that ends the verb, and a lone `@` read as the verb.
    #[inline]
    #[must_use]
    pub fn of(line: &str) -> Self {
        let bytes = line.as_bytes();
        let end = bytes.len() - usize::from(bytes.last() == Some(&b'\r'));
        let mut at = skip_separators(bytes, 0, end);
        let selector =
            (at + 1 < end && bytes[at] == b'@' && !is_separator(bytes[at + 1])).then(|| {
                let stop = word_end(bytes, at + 1, end);
                let body = at + 1..stop;
                at = skip_separators(bytes, stop, end);
                body
            });
        let verb_end = word_end(bytes, at, end);
        let rest_at = if verb_end < end { verb_end + 1 } else { end };
        Self {
            selector,
            verb: at..verb_end,
            rest: rest_at..end,
        }
    }

    /// Borrows the [`Head`] slices marked by these spans in `line`.
    #[inline]
    #[must_use]
    pub fn head<'a>(&self, line: &'a str) -> Head<'a> {
        Head {
            selector: self
                .selector
                .clone()
                .map(|body| Selector::parse(&line[body])),
            verb: &line[self.verb.clone()],
            rest: &line[self.rest.clone()],
        }
    }
}

/// `send`'s text as the PTY receives it, one rule for an inline argument and a `send -` payload:
/// a trailing line end, a trailing backslash-n, or a backslash-n before a line end (`echo 'ls\\n'`)
/// is written as one Enter (CR); every other byte is kept, an inner newline or backslash-n included.
#[must_use]
pub fn send_payload(text: &[u8]) -> Vec<u8> {
    let line_end = text
        .strip_suffix(b"\n")
        .map(|head| head.strip_suffix(b"\r").unwrap_or(head));
    let head = line_end.unwrap_or(text);
    let (head, enter) = match head.strip_suffix(b"\\n") {
        Some(before) => (before, true),
        None => (head, line_end.is_some()),
    };
    let mut bytes = Vec::with_capacity(head.len() + 1);
    bytes.extend_from_slice(head);
    if enter {
        bytes.push(0x0d);
    }
    bytes
}

/// Read a request line.
#[inline]
#[must_use]
pub fn head(line: &str) -> Head<'_> {
    Spans::of(line).head(line)
}

/// The words of `text` (a verb's rest) by the same separators, SP and HT, with none empty.
#[inline]
pub fn words(text: &str) -> impl Iterator<Item = &str> {
    text.split([' ', '\t']).filter(|word| !word.is_empty())
}

/// Extracts the single target session named in a `subscribe` request's `rest`.
#[inline]
#[must_use]
pub fn subscribe_target(rest: &str) -> Option<(Sid<'_>, &str)> {
    let bytes = rest.as_bytes();
    let at = skip_separators(bytes, 0, bytes.len());
    let stop = word_end(bytes, at, bytes.len());
    let sid = Sid::parse(rest[at..stop].strip_prefix('@')?)?;
    Some((sid, &rest[stop..]))
}

#[cfg(test)]
#[path = "control_request_tests.rs"]
mod tests;
