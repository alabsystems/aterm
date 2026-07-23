// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::super::types::{FilterMode, StreamingMatch};
use super::StreamingSearch;
#[cfg(kani)]
use crate::grapheme::map_lower_byte_to_original;
use crate::grapheme::{ColumnMap, LowerByteMap};
use std::borrow::Cow;

/// Resolve column positions for a match using precomputed maps.
/// O(log G) + O(log C) per call instead of O(G) + O(C) (#5672).
fn resolve_columns(
    col_map: &ColumnMap,
    lower_map: Option<&LowerByteMap>,
    abs_pos: usize,
    match_len: usize,
) -> (usize, usize) {
    // Saturating: callers only pass in-bounds match offsets (abs_pos +
    // match_len <= text length), so this is identical to `+` on every
    // reachable path while carrying the no-overflow proof for the L0 gate.
    let match_end = abs_pos.saturating_add(match_len);
    let (start_byte, end_byte) = match lower_map {
        Some(lm) => (lm.map_to_original(abs_pos), lm.map_to_original(match_end)),
        None => (abs_pos, match_end),
    };
    (
        col_map.byte_to_column(start_byte),
        col_map.byte_to_column(end_byte),
    )
}

/// Production literal search: find all substring matches via `str::find`.
/// Used by both Literal mode and the non-regex fallback path.
fn literal_find_matches(
    search_text: &str,
    search_pattern: &str,
    row: usize,
    col_map: &ColumnMap,
    lower_map: Option<&LowerByteMap>,
) -> Vec<StreamingMatch> {
    let mut matches = Vec::new();
    let match_len = search_pattern.len();
    // `get` + checked offsets instead of `search_text[start..]` arithmetic
    // indexing: find() only returns in-bounds, char-boundary offsets, so the
    // `else break`s are dead on real inputs — the shape carries the
    // bounds/no-overflow proofs for the Trust L0 gate. Identical matches.
    let mut start = 0;
    while let Some(tail) = search_text.get(start..) {
        let Some(pos) = tail.find(search_pattern) else {
            break;
        };
        let Some(abs_pos) = start.checked_add(pos) else {
            break;
        };
        let (start_col, end_col) = resolve_columns(col_map, lower_map, abs_pos, match_len);
        let m = StreamingMatch::new(row, start_col, end_col);
        // Filter zero-display-width matches (combining marks that are
        // non-empty in bytes but map to the same column). See INV-SEARCH-2c.
        if m.match_len > 0 {
            matches.push(m);
        }
        // Advance by one character to stay on a valid char boundary.
        let step = search_text
            .get(abs_pos..)
            .and_then(|s| s.chars().next())
            .map_or(1, char::len_utf8);
        let Some(next_start) = abs_pos.checked_add(step) else {
            break;
        };
        start = next_start;
    }
    matches
}

impl StreamingSearch {
    fn prepare_case_folded_inputs<'a>(&'a self, text: &'a str) -> (Cow<'a, str>, Cow<'a, str>) {
        if self.config.case_sensitive {
            (Cow::Borrowed(text), Cow::Borrowed(&self.pattern))
        } else {
            (
                Cow::Owned(text.to_lowercase()),
                Cow::Owned(self.pattern.to_lowercase()),
            )
        }
    }

    fn literal_matches_in_row(
        &self,
        row: usize,
        text: &str,
        search_text: &str,
        search_pattern: &str,
        col_map: &ColumnMap,
    ) -> Vec<StreamingMatch> {
        let lower_map = (!self.config.case_sensitive).then(|| LowerByteMap::new(text));

        #[cfg(kani)]
        {
            let mut matches = Vec::new();
            for abs_pos in find_overlapping_substring_positions(search_text, search_pattern) {
                let (start_col, end_col) =
                    resolve_columns(col_map, lower_map.as_ref(), abs_pos, search_pattern.len());
                let m = StreamingMatch::new(row, start_col, end_col);
                // Filter zero-display-width matches (INV-SEARCH-2c).
                if m.match_len > 0 {
                    matches.push(m);
                }
            }
            matches
        }

        #[cfg(not(kani))]
        literal_find_matches(
            search_text,
            search_pattern,
            row,
            col_map,
            lower_map.as_ref(),
        )
    }

    fn fuzzy_matches_in_row(
        row: usize,
        text: &str,
        search_text: &str,
        search_pattern: &str,
        col_map: &ColumnMap,
    ) -> Vec<StreamingMatch> {
        // push instead of `vec![..]`: the macro's boxed-slice expansion
        // (Box::new_uninit) trips the L0 gate's hardened-unsafe boundary
        // checks; incremental push takes the plain Vec growth path.
        // Identical single-element (or empty) result.
        let mut matches = Vec::new();
        if Self::fuzzy_match(search_text, search_pattern) {
            let end_col = col_map.byte_to_column(text.len());
            matches.push(StreamingMatch::new(row, 0, end_col));
        }
        matches
    }

    /// Find matches in a single row.
    ///
    /// Literal mode has two code paths (#2688): `#[cfg(kani)]` uses an explicit
    /// byte-level scanner that Kani can unroll; production uses `str::find()`.
    /// Both produce identical results for valid UTF-8.
    pub(crate) fn find_matches_in_row(&self, row: usize, text: &str) -> Vec<StreamingMatch> {
        if self.pattern.is_empty() {
            return Vec::new();
        }

        // Build the grapheme→column map once per line so every match resolves
        // columns in O(log G) instead of rescanning graphemes (#5672).
        let col_map = ColumnMap::new(text);

        match self.filter_mode {
            FilterMode::Literal => {
                let (search_text, search_pattern) = self.prepare_case_folded_inputs(text);
                self.literal_matches_in_row(
                    row,
                    text,
                    search_text.as_ref(),
                    search_pattern.as_ref(),
                    &col_map,
                )
            }
            FilterMode::Regex => {
                #[cfg(feature = "regex")]
                if let Some(ref re) = self.compiled_regex {
                    re.find_iter(text)
                        .filter(|cap| cap.start() != cap.end())
                        .map(|cap| {
                            let start_col = col_map.byte_to_column(cap.start());
                            let end_col = col_map.byte_to_column(cap.end());
                            StreamingMatch::new(row, start_col, end_col)
                        })
                        // Filter zero-display-width matches (e.g., combining marks
                        // that are non-empty in bytes but map to the same column).
                        .filter(|m| m.match_len > 0)
                        .collect()
                } else {
                    Vec::new()
                }

                #[cfg(not(feature = "regex"))]
                {
                    let (search_text, search_pattern) = self.prepare_case_folded_inputs(text);
                    self.literal_matches_in_row(
                        row,
                        text,
                        search_text.as_ref(),
                        search_pattern.as_ref(),
                        &col_map,
                    )
                }
            }
            FilterMode::Fuzzy => {
                let (search_text, search_pattern) = self.prepare_case_folded_inputs(text);
                Self::fuzzy_matches_in_row(row, text, &search_text, &search_pattern, &col_map)
            }
        }
    }

    /// Simple fuzzy match: check if all pattern characters appear in text in order.
    fn fuzzy_match(text: &str, pattern: &str) -> bool {
        let mut text_chars = text.chars();
        for p in pattern.chars() {
            loop {
                match text_chars.next() {
                    Some(t) if t == p => break,
                    Some(_) => {}
                    None => return false,
                }
            }
        }
        true
    }
}

// ========================================================================
// Gap coverage: map_lower_byte_to_original monotonicity/identity
// Part of #2875
// ========================================================================

#[cfg(kani)]
mod kani_proofs {
    use super::map_lower_byte_to_original;

    /// map_lower_byte_to_original monotonicity: a <= b implies
    /// map(s, a) <= map(s, b) for ASCII input where lowering preserves
    /// byte lengths.
    #[kani::proof]
    #[kani::unwind(14)]
    fn map_lower_byte_monotonic() {
        let original = "Hello World";
        let lowered = original.to_lowercase();
        let a: usize = kani::any();
        let b: usize = kani::any();
        kani::assume(a <= b);
        kani::assume(b <= lowered.len());

        let mapped_a = map_lower_byte_to_original(original, a);
        let mapped_b = map_lower_byte_to_original(original, b);

        kani::assert(
            mapped_a <= mapped_b,
            "map_lower_byte_to_original must be monotonic",
        );
    }

    /// For all-lowercase ASCII input, map_lower_byte_to_original(s, n) == n.
    #[kani::proof]
    #[kani::unwind(12)]
    fn map_lower_byte_ascii_identity() {
        let original = "abcdefghij";
        let n: usize = kani::any();
        kani::assume(n <= original.len());

        let mapped = map_lower_byte_to_original(original, n);

        kani::assert(
            mapped == n,
            "lowercase ASCII identity: mapped offset must equal input",
        );
    }

    /// map_lower_byte_to_original(s, n) <= s.len() for any offset n.
    /// The mapped offset must always be a valid position within (or at the
    /// end of) the original string.
    #[kani::proof]
    #[kani::unwind(14)]
    fn map_lower_byte_bounded_by_original_len() {
        let original = "Hello World";
        let n: usize = kani::any();
        // "Hello World" lowercased = "hello world", same byte length (11)
        kani::assume(n <= 12);

        let mapped = map_lower_byte_to_original(original, n);

        kani::assert(
            mapped <= original.len(),
            "mapped offset must not exceed original string length",
        );
    }

    /// fuzzy_match: positive subsequence cases.
    /// If pattern characters appear in text in order, fuzzy_match returns true.
    ///
    /// Symbolic over test case selection: proves all positive subsequence
    /// relationships hold by exploring each case through symbolic branching.
    /// Also proves prefix truncation of a matching pattern still matches.
    #[kani::proof]
    #[kani::unwind(14)]
    fn fuzzy_match_positive_subsequences() {
        use super::StreamingSearch;

        let case: u8 = kani::any();
        kani::assume(case <= 4);

        match case {
            // "hlo" is a subsequence of "hello" (h..l..o)
            0 => kani::assert(
                StreamingSearch::fuzzy_match("hello", "hlo"),
                "hlo is a subsequence of hello",
            ),
            // Full string matches itself
            1 => kani::assert(
                StreamingSearch::fuzzy_match("abc", "abc"),
                "exact match is a valid subsequence",
            ),
            // Empty pattern matches everything
            2 => kani::assert(
                StreamingSearch::fuzzy_match("hello", ""),
                "empty pattern matches any text",
            ),
            3 => kani::assert(
                StreamingSearch::fuzzy_match("", ""),
                "empty pattern matches empty text",
            ),
            // Prefix closure: any prefix of "hlo" also matches "hello"
            _ => {
                let prefix_len: usize = kani::any();
                kani::assume(prefix_len <= 3);
                let prefix = &"hlo"[..prefix_len];
                kani::assert(
                    StreamingSearch::fuzzy_match("hello", prefix),
                    "any prefix of a matching subsequence must also match",
                );
            }
        }
    }

    /// fuzzy_match: negative cases.
    /// If pattern characters do NOT appear in text in order, returns false.
    ///
    /// Symbolic over test case selection: proves all negative subsequence
    /// relationships hold by exploring each case through symbolic branching.
    #[kani::proof]
    #[kani::unwind(14)]
    fn fuzzy_match_negative_cases() {
        use super::StreamingSearch;

        let case: u8 = kani::any();
        kani::assume(case <= 3);

        match case {
            // Reversed order: not a subsequence
            0 => kani::assert(
                !StreamingSearch::fuzzy_match("hello", "olleh"),
                "reversed pattern is not a subsequence",
            ),
            // Character not present
            1 => kani::assert(
                !StreamingSearch::fuzzy_match("hello", "xyz"),
                "absent characters are not a subsequence",
            ),
            // Pattern longer than text: any suffix of "abc" beyond length of "ab" fails
            2 => {
                let pat_len: usize = kani::any();
                kani::assume(pat_len >= 3 && pat_len <= 3);
                let pattern = &"abc"[..pat_len];
                kani::assert(
                    !StreamingSearch::fuzzy_match("ab", pattern),
                    "pattern longer than text cannot be a subsequence",
                );
            }
            // Non-empty pattern on empty text: any non-empty prefix of "abc" fails
            _ => {
                let pat_len: usize = kani::any();
                kani::assume(pat_len >= 1 && pat_len <= 3);
                let pattern = &"abc"[..pat_len];
                kani::assert(
                    !StreamingSearch::fuzzy_match("", pattern),
                    "non-empty pattern does not match empty text",
                );
            }
        }
    }

    /// fuzzy_match structural property: if fuzzy_match(text, pattern) is true,
    /// then fuzzy_match(text, prefix_of_pattern) is also true (prefix closure).
    ///
    /// Symbolic over prefix length: for known-matching patterns "ace" and "bde",
    /// proves that any prefix (length 0 to full) also matches. This is the
    /// core prefix closure property of subsequence matching.
    #[kani::proof]
    #[kani::unwind(14)]
    fn fuzzy_match_prefix_closure() {
        use super::StreamingSearch;

        let text = "abcde";

        // Choose which matching pattern to test prefix closure on
        let use_second: bool = kani::any();
        let full_pattern = if use_second { "bde" } else { "ace" };
        let full_len = full_pattern.len(); // 3 for both

        // Verify the full pattern matches first
        kani::assert(
            StreamingSearch::fuzzy_match(text, full_pattern),
            "full pattern must be a subsequence of text",
        );

        // Symbolic prefix length: any prefix of the matching pattern must also match
        let prefix_len: usize = kani::any();
        kani::assume(prefix_len <= full_len);
        let prefix = &full_pattern[..prefix_len];

        kani::assert(
            StreamingSearch::fuzzy_match(text, prefix),
            "any prefix of a matching subsequence must also match (prefix closure)",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::types::{FilterMode, SearchState, StreamingSearchConfig};
    use super::super::StreamingSearch;

    /// Helper: create engine with literal mode and given case sensitivity.
    fn engine_literal(case_sensitive: bool) -> StreamingSearch {
        let config = StreamingSearchConfig {
            case_sensitive,
            ..StreamingSearchConfig::default()
        };
        StreamingSearch::with_config(config)
    }

    /// Helper: start literal search, scan a single row, return matches.
    fn find_in_row(pattern: &str, row_text: &str, case_sensitive: bool) -> Vec<(usize, usize)> {
        let mut engine = engine_literal(case_sensitive);
        engine.start_search(pattern, FilterMode::Literal).unwrap();
        engine.scan_row(0, row_text, 1);
        engine
            .results()
            .iter()
            .map(|m| (m.start_col, m.end_col))
            .collect()
    }

    // ====================================================================
    // Literal string matching
    // ====================================================================

    #[test]
    fn test_literal_match_at_start() {
        let matches = find_in_row("hello", "hello world", true);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0], (0, 5));
    }

    #[test]
    fn test_literal_match_at_middle() {
        let matches = find_in_row("llo", "hello world", true);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0], (2, 5));
    }

    #[test]
    fn test_literal_match_at_end() {
        let matches = find_in_row("world", "hello world", true);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0], (6, 11));
    }

    #[test]
    fn test_literal_multiple_matches_per_line() {
        let matches = find_in_row("ab", "ab cd ab ef ab", true);
        assert_eq!(matches.len(), 3);
        assert_eq!(matches[0], (0, 2));
        assert_eq!(matches[1], (6, 8));
        assert_eq!(matches[2], (12, 14));
    }

    #[test]
    fn test_literal_no_match() {
        let matches = find_in_row("xyz", "hello world", true);
        assert!(matches.is_empty());
    }

    #[test]
    fn test_literal_pattern_equals_text() {
        let matches = find_in_row("exact", "exact", true);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0], (0, 5));
    }

    // ====================================================================
    // Case-insensitive matching
    // ====================================================================

    #[test]
    fn test_case_insensitive_finds_uppercase() {
        let matches = find_in_row("hello", "HELLO WORLD", false);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0], (0, 5));
    }

    #[test]
    fn test_case_insensitive_finds_mixed_case() {
        let matches = find_in_row("hello", "HeLLo World", false);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0], (0, 5));
    }

    #[test]
    fn test_case_sensitive_does_not_match_different_case() {
        let matches = find_in_row("hello", "HELLO WORLD", true);
        assert!(matches.is_empty());
    }

    #[test]
    fn test_case_insensitive_multiple_matches() {
        let matches = find_in_row("ab", "Ab aB AB ab", false);
        assert_eq!(matches.len(), 4);
    }

    // ====================================================================
    // Empty pattern / empty line
    // ====================================================================

    #[test]
    fn test_empty_pattern_returns_no_matches() {
        let mut engine = StreamingSearch::new();
        // start_search rejects empty patterns
        let result = engine.start_search("", FilterMode::Literal);
        assert!(result.is_err());
    }

    #[test]
    fn test_empty_line_returns_no_matches() {
        let matches = find_in_row("test", "", true);
        assert!(matches.is_empty());
    }

    #[test]
    fn test_pattern_longer_than_text_no_match() {
        let matches = find_in_row("longpattern", "short", true);
        assert!(matches.is_empty());
    }

    // ====================================================================
    // Unicode matching
    // ====================================================================

    #[test]
    fn test_unicode_cjk_match() {
        let matches = find_in_row("日本", "日本語テスト", true);
        assert_eq!(matches.len(), 1);
        // CJK chars are 2 columns wide: "日" = cols 0-1, "本" = cols 2-3
        assert_eq!(matches[0], (0, 4));
    }

    #[test]
    fn test_unicode_emoji_match() {
        let matches = find_in_row("🎉", "hello 🎉 world", true);
        assert_eq!(matches.len(), 1);
    }

    #[test]
    fn test_unicode_mixed_width() {
        // "aあb" — 'a' is 1 col, 'あ' is 2 cols, 'b' is 1 col
        let matches = find_in_row("あ", "aあb", true);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0], (1, 3)); // columns 1-2 (2 wide)
    }

    #[test]
    fn test_unicode_repeated_cjk_all_found() {
        let matches = find_in_row("日", "日日日", true);
        assert_eq!(matches.len(), 3);
        assert_eq!(matches[0], (0, 2));
        assert_eq!(matches[1], (2, 4));
        assert_eq!(matches[2], (4, 6));
    }

    // ====================================================================
    // Fuzzy matching
    // ====================================================================

    #[test]
    fn test_fuzzy_match_subsequence() {
        let mut engine = StreamingSearch::new();
        engine.start_search("hlo", FilterMode::Fuzzy).unwrap();
        engine.scan_row(0, "hello world", 1);
        assert_eq!(engine.result_count(), 1);
        // Fuzzy match returns the full line
        let m = &engine.results()[0];
        assert_eq!(m.start_col, 0);
    }

    #[test]
    fn test_fuzzy_no_match() {
        let mut engine = StreamingSearch::new();
        engine.start_search("xyz", FilterMode::Fuzzy).unwrap();
        engine.scan_row(0, "hello world", 1);
        assert_eq!(engine.result_count(), 0);
    }

    #[test]
    fn test_fuzzy_match_exact_string() {
        assert!(StreamingSearch::fuzzy_match("abc", "abc"));
    }

    #[test]
    fn test_fuzzy_match_empty_pattern() {
        assert!(StreamingSearch::fuzzy_match("anything", ""));
    }

    #[test]
    fn test_fuzzy_match_empty_both() {
        assert!(StreamingSearch::fuzzy_match("", ""));
    }

    #[test]
    fn test_fuzzy_no_match_reversed() {
        assert!(!StreamingSearch::fuzzy_match("hello", "olleh"));
    }

    #[test]
    fn test_fuzzy_no_match_absent_chars() {
        assert!(!StreamingSearch::fuzzy_match("hello", "xyz"));
    }

    #[test]
    fn test_fuzzy_no_match_nonempty_on_empty() {
        assert!(!StreamingSearch::fuzzy_match("", "a"));
    }

    // ====================================================================
    // find_matches_in_row with empty pattern
    // ====================================================================

    #[test]
    fn test_find_matches_empty_pattern_returns_empty() {
        let engine = StreamingSearch::new(); // pattern is empty
        let matches = engine.find_matches_in_row(0, "some text");
        assert!(matches.is_empty());
    }

    // ====================================================================
    // Case-insensitive with scan_all
    // ====================================================================

    #[test]
    fn test_case_insensitive_scan_all() {
        use super::super::super::content::SearchContent;

        struct SimpleContent(Vec<String>);
        impl SearchContent for SimpleContent {
            fn row_count(&self) -> usize {
                self.0.len()
            }
            fn get_row_text(&mut self, row: usize) -> Option<String> {
                self.0.get(row).cloned()
            }
        }

        let mut engine = engine_literal(false);
        engine.start_search("hello", FilterMode::Literal).unwrap();
        let mut content = SimpleContent(vec![
            "Hello World".to_string(),
            "HELLO".to_string(),
            "no match".to_string(),
            "hello".to_string(),
        ]);
        engine.scan_all(&mut content);
        assert_eq!(engine.state(), SearchState::HasResults);
        assert_eq!(engine.result_count(), 3);
    }
}

/// Kani-only byte-level substring scanner. Equivalent to the production
/// `str::find()` loop for valid UTF-8 inputs but expressed in terms Kani
/// can unroll and verify. See #2688 for the dual-path rationale.
#[cfg(kani)]
fn find_overlapping_substring_positions(haystack: &str, needle: &str) -> Vec<usize> {
    let mut positions = Vec::new();
    if needle.is_empty() {
        return positions;
    }

    let haystack_bytes = haystack.as_bytes();
    let needle_bytes = needle.as_bytes();

    if needle_bytes.len() > haystack_bytes.len() {
        return positions;
    }

    let mut i = 0usize;
    while i + needle_bytes.len() <= haystack_bytes.len() {
        let mut j = 0usize;
        while j < needle_bytes.len() && haystack_bytes[i + j] == needle_bytes[j] {
            j += 1;
        }
        if j == needle_bytes.len() {
            positions.push(i);
        }
        i += 1;
    }

    positions
}
