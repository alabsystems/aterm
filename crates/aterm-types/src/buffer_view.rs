// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Unified read-only buffer access over scrollback + visible grid.
//!
//! The [`BufferView`] trait provides a single linear address space over all
//! terminal content: scrollback history (hot/warm/cold tiers) followed by the
//! visible grid. Line 0 is the oldest scrollback line; the highest line number
//! is the last visible row.

use std::borrow::Cow;

/// A match found during buffer search.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BufferMatch {
    /// Line number where the match was found.
    pub line: u64,
    /// Byte offset of match start within the line.
    pub start: usize,
    /// Byte offset of match end within the line (exclusive).
    pub end: usize,
}

/// Read-only access to terminal buffer content.
///
/// Provides a unified linear address space over scrollback + visible grid.
/// Line 0 is the oldest scrollback line. Implementations handle decompression
/// and tier traversal transparently.
pub trait BufferView {
    /// Total number of lines (scrollback + visible).
    fn line_count(&self) -> u64;

    /// Text content of a single line.
    ///
    /// Returns `None` if `line` is out of range.
    /// Uses `Cow` to avoid allocation when the line is in the hot tier.
    fn line_text(&self, line: u64) -> Option<Cow<'_, str>>;

    /// Text content for a range of lines, joined with newlines.
    // Skip: a trait DEFAULT method calling caller-implemented `Self::line_text`
    // / `Self::line_count` — `BufferView` is public and implemented downstream,
    // so the callee is unknowable here (the irreducible open-world dispatch
    // class). The default bodies are simple string/range walks, unit-tested.
    #[cfg_attr(trust_verify, trust::skip)]
    fn text_range(&self, start: u64, end: u64) -> String {
        let mut result = String::new();
        for line in start..end {
            if line > start {
                result.push('\n');
            }
            if let Some(text) = self.line_text(line) {
                result.push_str(text.trim_end());
            }
        }
        result
    }

    /// Search forward from a given line for a pattern (substring match).
    ///
    /// Returns the first match at or after `from`.
    // Skip: a trait DEFAULT method calling caller-implemented `Self::line_text`
    // / `Self::line_count` — `BufferView` is public and implemented downstream,
    // so the callee is unknowable here (the irreducible open-world dispatch
    // class). The default bodies are simple string/range walks, unit-tested.
    #[cfg_attr(trust_verify, trust::skip)]
    fn search_forward(&self, from: u64, pattern: &str) -> Option<BufferMatch> {
        for line in from..self.line_count() {
            if let Some(text) = self.line_text(line)
                && let Some(start) = text.find(pattern)
            {
                // Saturating is exact here: `start <= text.len()` and a match
                // requires `start + pattern.len() <= text.len() <= isize::MAX`,
                // so the sum cannot overflow on any reachable input (Trust L0:
                // the verifier cannot see the `find` postcondition).
                return Some(BufferMatch {
                    line,
                    start,
                    end: start.saturating_add(pattern.len()),
                });
            }
        }
        None
    }

    /// Search backward from a given line for a pattern (substring match).
    ///
    /// Returns the last match at or before `from`.
    // Skip: a trait DEFAULT method calling caller-implemented `Self::line_text`
    // / `Self::line_count` — `BufferView` is public and implemented downstream,
    // so the callee is unknowable here (the irreducible open-world dispatch
    // class). The default bodies are simple string/range walks, unit-tested.
    #[cfg_attr(trust_verify, trust::skip)]
    fn search_backward(&self, from: u64, pattern: &str) -> Option<BufferMatch> {
        let start = from.min(self.line_count().saturating_sub(1));
        for line in (0..=start).rev() {
            if let Some(text) = self.line_text(line)
                && let Some(pos) = text.rfind(pattern)
            {
                // Saturating is exact here; see `search_forward`.
                return Some(BufferMatch {
                    line,
                    start: pos,
                    end: pos.saturating_add(pattern.len()),
                });
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Simple in-memory buffer for testing.
    struct TestBuffer {
        lines: Vec<String>,
    }

    impl BufferView for TestBuffer {
        fn line_count(&self) -> u64 {
            self.lines.len() as u64
        }

        fn line_text(&self, line: u64) -> Option<Cow<'_, str>> {
            self.lines
                .get(line as usize)
                .map(|s| Cow::Borrowed(s.as_str()))
        }
    }

    fn make_buffer(lines: &[&str]) -> TestBuffer {
        TestBuffer {
            lines: lines.iter().map(|s| (*s).to_string()).collect(),
        }
    }

    #[test]
    fn line_count_empty() {
        let buf = make_buffer(&[]);
        assert_eq!(buf.line_count(), 0);
    }

    #[test]
    fn line_count_nonempty() {
        let buf = make_buffer(&["a", "b", "c"]);
        assert_eq!(buf.line_count(), 3);
    }

    #[test]
    fn line_text_valid() {
        let buf = make_buffer(&["hello", "world"]);
        assert_eq!(buf.line_text(0).unwrap().as_ref(), "hello");
        assert_eq!(buf.line_text(1).unwrap().as_ref(), "world");
    }

    #[test]
    fn line_text_out_of_range() {
        let buf = make_buffer(&["hello"]);
        assert!(buf.line_text(1).is_none());
    }

    #[test]
    fn text_range_joins_with_newlines() {
        let buf = make_buffer(&["line1", "line2", "line3"]);
        assert_eq!(buf.text_range(0, 3), "line1\nline2\nline3");
    }

    #[test]
    fn text_range_partial() {
        let buf = make_buffer(&["a", "b", "c", "d"]);
        assert_eq!(buf.text_range(1, 3), "b\nc");
    }

    #[test]
    fn search_forward_finds_match() {
        let buf = make_buffer(&["foo bar", "baz qux", "foo again"]);
        let m = buf.search_forward(0, "foo").unwrap();
        assert_eq!(m.line, 0);
        assert_eq!(m.start, 0);
        assert_eq!(m.end, 3);
    }

    #[test]
    fn search_forward_from_offset() {
        let buf = make_buffer(&["foo bar", "baz qux", "foo again"]);
        let m = buf.search_forward(1, "foo").unwrap();
        assert_eq!(m.line, 2);
    }

    #[test]
    fn search_forward_no_match() {
        let buf = make_buffer(&["foo", "bar"]);
        assert!(buf.search_forward(0, "xyz").is_none());
    }

    #[test]
    fn search_backward_finds_match() {
        let buf = make_buffer(&["foo bar", "baz qux", "foo again"]);
        let m = buf.search_backward(2, "foo").unwrap();
        assert_eq!(m.line, 2);
    }

    #[test]
    fn search_backward_skips_later() {
        let buf = make_buffer(&["foo bar", "baz qux", "foo again"]);
        let m = buf.search_backward(1, "foo").unwrap();
        assert_eq!(m.line, 0);
    }

    #[test]
    fn search_backward_no_match() {
        let buf = make_buffer(&["foo", "bar"]);
        assert!(buf.search_backward(1, "xyz").is_none());
    }
}
