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

/// A parsed `@<selector>`: the body after the `@`.
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

/// A request line read by the grammar: what [`head`] answers.
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
    /// Read `line` by the grammar (see the module doc).
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

/// Read a request line: its selector, verb and rest (see the module doc).
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
mod tests {
    use super::*;

    fn read(line: &str) -> (Option<Selector<'_>>, &str, &str) {
        let Head {
            selector,
            verb,
            rest,
        } = head(line);
        (selector, verb, rest)
    }

    /// A sid is `s-` and lower-case hex, the form ids are minted in; anything
    /// else (an upper-case digit included) is no session's id.
    #[test]
    fn a_sid_is_s_dash_and_hex() {
        for good in ["s-0", "s-0f1e", "s-abcdef0123456789abcd"] {
            assert_eq!(Sid::parse(good).map(|s| s.as_str()), Some(good), "{good}");
        }
        for bad in [
            "",
            "s-",
            "S-1",
            "s1",
            "s-g",
            "s-1 ",
            "s-../x",
            "./s-1",
            "s-1/..",
            "s-nope",
            "self",
            "s-ABCDEF0123456789ABCD",
            "s-0F1e",
        ] {
            assert_eq!(Sid::parse(bad), None, "{bad:?}");
        }
    }

    /// The selector body: `.` is self, digits a local number (past `u64` it
    /// names nothing, never a wrapped number or an id), a sid an id, and every
    /// other spelling, the empty body included, is invalid.
    #[test]
    fn selector_parses_the_wire_forms() {
        assert_eq!(Selector::parse(""), Selector::Invalid(""));
        assert_eq!(Selector::parse("."), Selector::SelfTok);
        assert_eq!(Selector::parse("12"), Selector::Local(12));
        assert_eq!(Selector::parse("007"), Selector::Local(7));
        assert_eq!(
            Selector::parse("s-0f1e"),
            Selector::Sid(Sid::parse("s-0f1e").expect("a sid"))
        );
        let huge = "99999999999999999999";
        assert_eq!(Selector::parse(huge), Selector::Invalid(huge));
        for bad in [
            "+5", "-1", "S-1", "s-0F1E", "./s-1", "../x", "self", "*", "s-a,s-b",
        ] {
            assert_eq!(Selector::parse(bad), Selector::Invalid(bad), "{bad:?}");
        }
        assert!(Selector::SelfTok.is_self() && !Selector::Local(0).is_self());
    }

    /// One line, one reading: leading separators and a run after the selector
    /// are skipped, either separator ends a word, and `rest` starts after
    /// exactly one separator, so a payload keeps its spacing.
    #[test]
    fn head_reads_separators_once() {
        let sid = Selector::Sid(Sid::parse("s-1a").expect("a sid"));
        assert_eq!(read("text"), (None, "text", ""));
        assert_eq!(read("text --json"), (None, "text", "--json"));
        assert_eq!(read("send  hello"), (None, "send", " hello"));
        assert_eq!(read("send hello \r"), (None, "send", "hello "));
        assert_eq!(read("  feed-bin 5"), (None, "feed-bin", "5"));
        assert_eq!(read("\tfeed-bin\t5"), (None, "feed-bin", "5"));
        assert_eq!(read("@s-1a\tfeed-bin 5"), (Some(sid), "feed-bin", "5"));
        assert_eq!(read("@s-1a \t screen x"), (Some(sid), "screen", "x"));
        assert_eq!(read("@. text"), (Some(Selector::SelfTok), "text", ""));
        assert_eq!(read("@ text"), (None, "@", "text"));
        assert_eq!(read("@"), (None, "@", ""));
        assert_eq!(read("@7"), (Some(Selector::Local(7)), "", ""));
        assert_eq!(read(""), (None, "", ""));
        assert_eq!(read("   "), (None, "", ""));
        assert_eq!(read("text\u{a0}x"), (None, "text\u{a0}x", ""));
        assert_eq!(read("feed-bin\x0b4"), (None, "feed-bin\x0b4", ""));
        let line = "  @s-1a\tsend  hi\r";
        let spans = Spans::of(line);
        assert_eq!(spans.head(line), head(line));
        assert_eq!(&line[spans.verb.clone()], "send");
        assert_eq!(&line[spans.rest.clone()], " hi");
    }

    /// A verb's words split at SP and HT only, never at another space
    /// character, and no word is empty.
    #[test]
    fn words_split_where_the_head_does() {
        let split = |text| words(text).collect::<Vec<_>>();
        assert_eq!(split("@. screen since=4"), ["@.", "screen", "since=4"]);
        assert_eq!(split(" \t@.\t\tscreen  "), ["@.", "screen"]);
        assert_eq!(split("screen\u{a0}x\x0bcells"), ["screen\u{a0}x\x0bcells"]);
        assert!(split("").is_empty() && split(" \t ").is_empty());
    }

    /// A `subscribe` names one remote session only by a single `@<sid>` as its
    /// first word; the text after it is kept as written.
    #[test]
    fn subscribe_names_one_session_only_by_a_sid() {
        let (sid, tail) = subscribe_target("@s-1a screen trim").expect("one sid");
        assert_eq!((sid.as_str(), tail), ("s-1a", " screen trim"));
        let (sid, tail) = subscribe_target("\t@s-1a\tcells").expect("one sid");
        assert_eq!((sid.as_str(), tail), ("s-1a", "\tcells"));
        for local in [
            "@* screen",
            "@s-1a,s-2b screen",
            "@. screen",
            "@ screen",
            "@7 screen",
            "@self screen",
            "@S-1A screen",
            "@s-1A screen",
            "screen",
            "",
        ] {
            assert_eq!(subscribe_target(local), None, "{local:?}");
        }
    }
}
