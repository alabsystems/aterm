// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `line:h6` anchors (design §7b-§7c, decision D-11; slice S6).

use std::fmt;
use std::str::FromStr;

/// The UTF-8 byte-order mark a file may open with.
pub(crate) const BOM: &str = "\u{feff}";

/// Bits an h6 keeps of the 64-bit hash: the top 24.
const H6_SHIFT: u32 = 40;

/// The largest h6: 24 bits.
const H6_MAX: u32 = 0x00ff_ffff;

/// The first 24 bits of FNV-1a/64 over a line's bytes, without its
/// end-of-line.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Hash6(u32);

impl Hash6 {
    /// The h6 of one line's bytes (without its end-of-line).
    #[must_use]
    pub fn of(line: &str) -> Self {
        let top = h6_of_hash(aterm_hash::fnv1a_64(line.as_bytes()));
        Self(u32::try_from(top).unwrap_or(H6_MAX))
    }

    /// An h6 from its 24-bit value; `None` past 24 bits.
    #[must_use]
    pub fn from_value(value: u32) -> Option<Self> {
        fits_h6(value).then_some(Self(value))
    }

    /// The 24-bit value.
    #[must_use]
    pub const fn value(self) -> u32 {
        self.0
    }
}

impl fmt::Display for Hash6 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:06x}", self.0)
    }
}

impl FromStr for Hash6 {
    type Err = AnchorError;

    /// Exactly 6 hex digits; upper case reads as the same value.
    fn from_str(text: &str) -> Result<Self, AnchorError> {
        if text.len() != 6 || !text.bytes().all(is_hex_digit) {
            return Err(AnchorError::Hash(text.to_string()));
        }
        u32::from_str_radix(text, 16)
            .ok()
            .and_then(Self::from_value)
            .ok_or_else(|| AnchorError::Hash(text.to_string()))
    }
}

/// One `<n>:<h6>` token: a 1-based line number and the h6 the line had when
/// it was read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Anchor {
    /// The line, 1-based.
    pub line: usize,
    /// The line's h6 when it was read.
    pub h6: Hash6,
}

impl Anchor {
    /// The anchor of line `line` (1-based) of `text`, or `None` past its last
    /// line.
    #[must_use]
    pub fn of_line(text: &str, line: usize) -> Option<Self> {
        let lines = Lines::of(text);
        lines.content(line).map(|content| Self {
            line,
            h6: Hash6::of(content),
        })
    }
}

impl fmt::Display for Anchor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.line, self.h6)
    }
}

impl FromStr for Anchor {
    type Err = AnchorError;

    /// `<n>:<h6>`: a decimal line number from 1, a colon, 6 hex digits.
    fn from_str(token: &str) -> Result<Self, AnchorError> {
        let (line, h6) = token
            .split_once(':')
            .ok_or_else(|| AnchorError::Shape(token.to_string()))?;
        let line = parse_line_number(line).ok_or_else(|| AnchorError::Line(token.to_string()))?;
        Ok(Self {
            line,
            h6: h6.parse()?,
        })
    }
}

/// Why a token is not an anchor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AnchorError {
    /// No `:` between a line number and a hash.
    Shape(String),
    /// The line number is not a decimal from 1.
    Line(String),
    /// The hash is not 6 hex digits.
    Hash(String),
}

impl fmt::Display for AnchorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Shape(token) => write!(
                f,
                "`{token}` is not an anchor (want <line>:<h6>, as a read prints it)"
            ),
            Self::Line(token) => write!(f, "`{token}` has no line number (want a decimal from 1)"),
            Self::Hash(token) => write!(f, "`{token}` is not 6 hex digits"),
        }
    }
}

impl std::error::Error for AnchorError {}

/// A 1-based line number: decimal digits only, not 0, no sign.
pub(crate) fn parse_line_number(text: &str) -> Option<usize> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse::<usize>().ok().filter(|line| *line > 0)
}

/// The h6 of a 64-bit hash: its top 24 bits, the first 6 of its 16 hex
/// digits.
const fn h6_of_hash(hash: u64) -> u64 {
    hash >> H6_SHIFT
}

trust_contract_fn! {
    /// Whether `value` fits an h6: 24 bits, at most `0xff_ffff`.
    #[must_use]
    fn fits_h6(value: u32) -> bool
    clauses [ensures result == (value <= 16777215) by fits_h6_is_24_bits]
    {
        value <= H6_MAX
    }
}

verify_island! {
    clean {
        theorem fits_h6_is_24_bits (value : UInt32) :
            trust_import_amacs__protocol__anchor__fits_h6 value = decide (value <= 16777215) := by
          simp only [trust_import_amacs__protocol__anchor__fits_h6]
    }
}

trust_contract_fn! {
    /// Whether `byte` is an ASCII hex digit, either case: what an h6 is
    /// written in.
    #[must_use]
    #[expect(
        clippy::manual_range_contains,
        reason = "the contract's import reads comparisons, not a call to RangeInclusive::contains"
    )]
    fn is_hex_digit(byte: u8) -> bool
    clauses [ensures result == ((byte >= 48 && byte <= 57) || ((byte >= 97 && byte <= 102) || (byte >= 65 && byte <= 70))) by is_hex_digit_is_the_class]
    {
        (byte >= b'0' && byte <= b'9') || ((byte >= b'a' && byte <= b'f') || (byte >= b'A' && byte <= b'F'))
    }
}

verify_island! {
    clean {
        theorem is_hex_digit_is_the_class (byte : UInt8) :
            trust_import_amacs__protocol__anchor__is_hex_digit byte =
              ((decide (48 <= byte) && decide (byte <= 57))
                || ((decide (97 <= byte) && decide (byte <= 102))
                  || (decide (65 <= byte) && decide (byte <= 70)))) := by
          simp only [trust_import_amacs__protocol__anchor__is_hex_digit]
    }
}

/// One line's byte offsets in its text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LineSpan {
    /// The line's first byte.
    pub(crate) start: usize,
    /// One past its last byte before the end-of-line.
    pub(crate) content_end: usize,
    /// One past its end-of-line (`content_end` for a final line without one).
    pub(crate) end: usize,
}

/// A text's lines, by the rule in the module docs.
#[derive(Clone, Debug)]
pub(crate) struct Lines<'t> {
    text: &'t str,
    spans: Vec<LineSpan>,
    /// Where line 1 starts: past a byte-order mark, or 0.
    origin: usize,
}

impl<'t> Lines<'t> {
    pub(crate) fn of(text: &'t str) -> Self {
        let origin = if text.starts_with(BOM) { BOM.len() } else { 0 };
        let mut spans = Vec::new();
        let mut start = origin;
        let body = text.get(origin..).unwrap_or("");
        for (offset, byte) in body.bytes().enumerate() {
            if byte != b'\n' {
                continue;
            }
            let newline = origin.saturating_add(offset);
            let content_end = if newline > start
                && text.as_bytes().get(newline.saturating_sub(1)) == Some(&b'\r')
            {
                newline.saturating_sub(1)
            } else {
                newline
            };
            let end = newline.saturating_add(1);
            spans.push(LineSpan {
                start,
                content_end,
                end,
            });
            start = end;
        }
        if start < text.len() {
            spans.push(LineSpan {
                start,
                content_end: text.len(),
                end: text.len(),
            });
        }
        Self {
            text,
            spans,
            origin,
        }
    }

    /// The text the table was built over.
    pub(crate) const fn text(&self) -> &'t str {
        self.text
    }

    /// How many lines the text has.
    pub(crate) const fn count(&self) -> usize {
        self.spans.len()
    }

    /// Where line 1 starts (past a byte-order mark).
    pub(crate) const fn origin(&self) -> usize {
        self.origin
    }

    /// Line `line`'s offsets (1-based), or `None` past the last line.
    pub(crate) fn span(&self, line: usize) -> Option<LineSpan> {
        line.checked_sub(1)
            .and_then(|index| self.spans.get(index))
            .copied()
    }

    /// Line `line`'s bytes without its end-of-line.
    pub(crate) fn content(&self, line: usize) -> Option<&'t str> {
        self.span(line)
            .and_then(|span| self.text.get(span.start..span.content_end))
    }

    /// Line `line`'s h6, or `None` past the last line.
    pub(crate) fn h6(&self, line: usize) -> Option<Hash6> {
        self.content(line).map(Hash6::of)
    }

    /// Whether the text's last line has no end-of-line (and there is one).
    pub(crate) fn open_ended(&self) -> bool {
        self.spans
            .last()
            .is_some_and(|span| span.content_end == span.end)
    }

    /// The 1-based line holding byte `offset`: the line whose bytes, with its
    /// end-of-line, contain it.
    pub(crate) fn line_at(&self, offset: usize) -> Option<usize> {
        let index = self.spans.partition_point(|span| span.end <= offset);
        let span = self.spans.get(index)?;
        (span.start <= offset).then(|| index.saturating_add(1))
    }

    /// The end-of-line new lines get: the text's first one, or `\n`.
    pub(crate) fn eol(&self) -> &'static str {
        match self.spans.first() {
            Some(span) if span.end.saturating_sub(span.content_end) == 2 => "\r\n",
            _ => "\n",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn h6_matches_fnv1a_64_prefix_and_is_stable() {
        for line in ["", "fn main() {", "  let x = 1;", "é漢🦀", "tab\there"] {
            let hash = aterm_hash::fnv1a_64(line.as_bytes());
            let hex16 = aterm_hash::fnv1a_64_to_hex16(hash);
            let prefix = std::str::from_utf8(&hex16[..6]).unwrap();
            assert_eq!(Hash6::of(line).to_string(), prefix, "{line:?}");
            assert_eq!(u64::from(Hash6::of(line).value()), hash >> 40);
        }
        assert_eq!(Hash6::of("").to_string(), "cbf29c");
        assert_eq!(Hash6::of("a").to_string(), "af63dc");
        assert_eq!(Hash6::of("hello").to_string(), "a430d8");

        let lf = Lines::of("one\ntwo\n");
        let crlf = Lines::of("one\r\ntwo\r\n");
        let bom = Lines::of("\u{feff}one\ntwo");
        for line in 1..=2 {
            assert_eq!(lf.h6(line), crlf.h6(line));
            assert_eq!(lf.h6(line), bom.h6(line));
        }
        assert_eq!(lf.h6(1), Some(Hash6::of("one")));
        assert_eq!(lf.h6(3), None);
        assert_eq!(crlf.eol(), "\r\n");
        assert_eq!(lf.eol(), "\n");
        assert_eq!(bom.origin(), 3);
        assert!(bom.open_ended());
        assert!(!lf.open_ended());
    }

    #[test]
    fn anchors_round_trip_and_refuse_what_a_read_never_prints() {
        let anchor: Anchor = "12:a3f9c1".parse().unwrap();
        assert_eq!(anchor.line, 12);
        assert_eq!(anchor.h6.value(), 0x00a3_f9c1);
        assert_eq!(anchor.to_string(), "12:a3f9c1");
        assert_eq!("12:A3F9C1".parse::<Anchor>().unwrap(), anchor);
        assert_eq!(
            Anchor::of_line("x\nhello\n", 2).unwrap().to_string(),
            "2:a430d8"
        );
        assert_eq!(Anchor::of_line("x\n", 2), None);
        for bad in [
            "12",
            "0:a3f9c1",
            "-1:a3f9c1",
            "+1:a3f9c1",
            ":a3f9c1",
            "12:a3f9c",
            "12:a3f9c10",
            "12:a3f9cg",
            "x:a3f9c1",
            "12:a3f9c1|text",
        ] {
            assert!(bad.parse::<Anchor>().is_err(), "{bad}");
        }
        assert_eq!(Hash6::from_value(0x0100_0000), None);
        assert_eq!(Hash6::from_value(0xff_ffff).unwrap().to_string(), "ffffff");
    }

    #[test]
    fn lines_split_at_newline_and_keep_a_final_open_line() {
        let empty = Lines::of("");
        assert_eq!(empty.count(), 0);
        let one = Lines::of("\n");
        assert_eq!(one.count(), 1);
        assert_eq!(one.content(1), Some(""));
        let text = "a\r\nb\nc";
        let lines = Lines::of(text);
        assert_eq!(lines.count(), 3);
        assert_eq!(lines.content(1), Some("a"));
        assert_eq!(lines.content(2), Some("b"));
        assert_eq!(lines.content(3), Some("c"));
        assert_eq!(lines.line_at(0), Some(1));
        assert_eq!(lines.line_at(2), Some(1), "the LF of a CRLF is line 1's");
        assert_eq!(lines.line_at(3), Some(2));
        assert_eq!(lines.line_at(text.len() - 1), Some(3));
        assert_eq!(lines.line_at(text.len()), None);
        let cr = Lines::of("a\rb\n");
        assert_eq!(cr.count(), 1);
        assert_eq!(cr.content(1), Some("a\rb"));
        assert_eq!(Lines::of("\u{feff}").count(), 0);
        assert_eq!(Lines::of("\u{feff}x").line_at(0), None);
    }
}
