// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::super::types::{FilterMode, StreamingMatch};
use super::StreamingSearch;
use crate::grapheme::{
    ColumnMap, LowerByteMap, LowerNeed, display_columns, lower_fold, lower_need,
};
use crate::index::scan_line;
use std::borrow::Cow;

/// Per-row coordinate scratch, populated only after the first substring hit.
/// Most rows in a history search miss; they need neither grapheme traversal
/// nor an allocated offset map. ASCII lowercasing preserves every byte offset,
/// including control characters (whose display width still uses `ColumnMap`).
struct MatchColumns<'a> {
    text: &'a str,
    lowered: bool,
    maps: Option<(ColumnMap, Option<LowerByteMap>)>,
}

impl<'a> MatchColumns<'a> {
    fn new(text: &'a str, lowered: bool) -> Self {
        Self {
            text,
            lowered,
            maps: None,
        }
    }

    fn resolve(&mut self, abs_pos: usize, match_len: usize) -> (usize, usize) {
        let (col_map, lower_map) = self.maps.get_or_insert_with(|| {
            (
                // The streaming engine is fed by no terminal, so it keeps the
                // default policy.
                ColumnMap::new(self.text, aterm_types::text_shaping::AmbiguousWidth::Single),
                (self.lowered && !self.text.is_ascii()).then(|| LowerByteMap::new(self.text)),
            )
        });
        resolve_columns(col_map, lower_map.as_ref(), abs_pos, match_len)
    }
}

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
    col_map.span(start_byte, end_byte)
}

/// Production literal search: find all substring matches via `str::find`.
/// Used by both Literal mode and the non-regex fallback path.
fn literal_find_matches(
    search_text: &str,
    search_pattern: &str,
    row: usize,
    columns: &mut MatchColumns<'_>,
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
        let (start_col, end_col) = columns.resolve(abs_pos, match_len);
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
            (case_fold(text), case_fold(&self.pattern))
        }
    }

    fn literal_matches_in_row(
        &self,
        row: usize,
        text: &str,
        search_text: &str,
        search_pattern: &str,
    ) -> Vec<StreamingMatch> {
        let mut columns = MatchColumns::new(text, !self.config.case_sensitive);

        literal_find_matches(search_text, search_pattern, row, &mut columns)
    }

    fn fuzzy_matches_in_row(
        row: usize,
        text: &str,
        search_text: &str,
        search_pattern: &str,
    ) -> Vec<StreamingMatch> {
        // push instead of `vec![..]`: the macro's boxed-slice expansion
        // (Box::new_uninit) trips the L0 gate's hardened-unsafe boundary
        // checks; incremental push takes the plain Vec growth path.
        // Identical single-element (or empty) result.
        let mut matches = Vec::new();
        if Self::fuzzy_match(search_text, search_pattern) {
            let end_col = display_columns(text);
            if end_col > 0 {
                matches.push(StreamingMatch::new(row, 0, end_col));
            }
        }
        matches
    }

    /// Find matches in a single row.
    ///
    /// Literal mode scans with `str::find()`, advancing one character past
    /// each hit so overlapping matches are found (#2688).
    pub(crate) fn find_matches_in_row(&self, row: usize, text: &str) -> Vec<StreamingMatch> {
        if self.pattern.is_empty() {
            return Vec::new();
        }

        match self.filter_mode {
            FilterMode::Literal => {
                let (search_text, search_pattern) = self.prepare_case_folded_inputs(text);
                self.literal_matches_in_row(
                    row,
                    text,
                    search_text.as_ref(),
                    search_pattern.as_ref(),
                )
            }
            FilterMode::Regex => {
                if let Some(ref re) = self.compiled_regex {
                    let mut columns = MatchColumns::new(text, false);
                    scan_line(re, text)
                        .filter(|cap| cap.start() != cap.end())
                        .map(|cap| {
                            let (start_col, end_col) =
                                columns.resolve(cap.start(), cap.end() - cap.start());
                            StreamingMatch::new(row, start_col, end_col)
                        })
                        // Filter zero-display-width matches (e.g., combining marks
                        // that are non-empty in bytes but map to the same column).
                        .filter(|m| m.match_len > 0)
                        .collect()
                } else {
                    Vec::new()
                }
            }
            FilterMode::Fuzzy => {
                let (search_text, search_pattern) = self.prepare_case_folded_inputs(text);
                Self::fuzzy_matches_in_row(row, text, &search_text, &search_pattern)
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

/// Keep identity ASCII folds borrowed and use the index's canonical Unicode
/// fold. Contextual `str::to_lowercase` turns word-final Σ into ς, which made
/// the streaming UI disagree with indexed searches for σ.
fn case_fold(text: &str) -> Cow<'_, str> {
    match lower_need(text) {
        LowerNeed::None => Cow::Borrowed(text),
        LowerNeed::Ascii => Cow::Owned(text.to_ascii_lowercase()),
        LowerNeed::Unicode => Cow::Owned(lower_fold(text)),
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::types::{FilterMode, SearchState, StreamingSearchConfig};
    use super::super::StreamingSearch;
    use super::{MatchColumns, literal_find_matches};

    /// Helper: create engine with literal mode and given case sensitivity.
    fn engine_literal(case_sensitive: bool) -> StreamingSearch {
        let config = StreamingSearchConfig {
            case_sensitive,
            ..StreamingSearchConfig::default()
        };
        StreamingSearch::with_config(config)
    }

    #[test]
    fn fuzzy_search_ignores_rows_without_display_columns() {
        for text in ["\t", "\r", "\u{0301}"] {
            let mut engine = engine_literal(true);
            engine.start_search(text, FilterMode::Fuzzy).unwrap();
            engine.scan_row(0, text, 2);
            assert!(engine.results().is_empty(), "{text:?}");
            assert!(engine.verify_result_positions_valid());

            engine.scan_row(1, &format!("{text}x"), 2);
            assert_eq!(engine.results().len(), 1);
            assert!(engine.results()[0].match_len > 0);
            assert!(engine.verify_result_positions_valid());
        }
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

    #[test]
    fn unmatched_rows_do_not_build_coordinate_maps() {
        let text = "日本語 Kelvin e\u{0301} 😀";
        let mut columns = MatchColumns::new(text, true);
        assert!(literal_find_matches(&text.to_lowercase(), "absent", 0, &mut columns).is_empty());
        assert!(columns.maps.is_none());

        assert_eq!(
            literal_find_matches(&text.to_lowercase(), "kelvin", 0, &mut columns).len(),
            1
        );
        assert!(columns.maps.is_some());
    }

    #[test]
    fn ascii_lowercase_offsets_need_no_map_but_controls_keep_display_columns() {
        let text = "\tHeLLo\r hello";
        let mut columns = MatchColumns::new(text, true);
        let matches = literal_find_matches(&text.to_lowercase(), "hello", 0, &mut columns);
        assert_eq!(matches.len(), 2);
        let (_, lower_map) = columns.maps.as_ref().expect("matching row has columns");
        assert!(lower_map.is_none());
        for (m, offset) in matches.iter().zip([1, 8]) {
            assert_eq!(m.start_col, crate::grapheme::byte_to_column(text, offset));
            assert_eq!(m.end_col, crate::grapheme::byte_to_column(text, offset + 5));
        }
    }

    #[test]
    fn lazy_coordinate_maps_match_reference_for_overlaps_and_unicode() {
        use crate::grapheme::{byte_to_column, lower_fold, map_lower_byte_to_original};

        for text in [
            "aAaAa\tAA",
            "日本語 Kelvin İSTANBUL e\u{0301} 😀😀",
            "ΟΣ Σσς éÉ",
            "",
        ] {
            for pattern in ["aa", "AA", "日", "k", "i", "\u{0301}", "😀", "σ", "absent"] {
                for case_sensitive in [true, false] {
                    let mut engine = engine_literal(case_sensitive);
                    engine.start_search(pattern, FilterMode::Literal).unwrap();
                    let haystack = if case_sensitive {
                        text.to_owned()
                    } else {
                        lower_fold(text)
                    };
                    let needle = if case_sensitive {
                        pattern.to_owned()
                    } else {
                        lower_fold(pattern)
                    };
                    let mut expected = Vec::new();
                    for (offset, _) in haystack.char_indices() {
                        if !haystack[offset..].starts_with(&needle) {
                            continue;
                        }
                        let original = |offset| {
                            if case_sensitive {
                                offset
                            } else {
                                map_lower_byte_to_original(text, offset)
                            }
                        };
                        let start = byte_to_column(text, original(offset));
                        let end = byte_to_column(text, original(offset + needle.len()));
                        if end > start {
                            expected.push((start, end));
                        }
                    }
                    let actual: Vec<_> = engine
                        .find_matches_in_row(0, text)
                        .iter()
                        .map(|m| (m.start_col, m.end_col))
                        .collect();
                    assert_eq!(
                        actual, expected,
                        "text={text:?}, pattern={pattern:?}, sensitive={case_sensitive}"
                    );
                }
            }
        }
    }

    #[test]
    fn streaming_sigma_matches_all_forms_at_original_display_columns() {
        // The CJK prefix makes byte offsets different from display columns.
        for pattern in ["Σ", "σ", "ς"] {
            assert_eq!(
                find_in_row(pattern, "日ΟΣ Σσς", false),
                vec![(3, 4), (5, 6), (6, 7), (7, 8)],
                "pattern={pattern}"
            );
        }
        assert_eq!(find_in_row("σ", "日ΟΣ Σσς", true), vec![(6, 7)]);
    }

    #[test]
    fn width_only_search_paths_preserve_unicode_match_columns() {
        use super::super::super::test_content::WrappedTestContent;

        let mut fuzzy = engine_literal(true);
        fuzzy.start_search("日e", FilterMode::Fuzzy).unwrap();
        fuzzy.scan_row(0, "\t日1\u{fe0f}\u{20e3}e\u{0301}", 1);
        let m = &fuzzy.results()[0];
        assert_eq!((m.row, m.start_col, m.end_col), (0, 0, 5));

        let mut content = WrappedTestContent::new(
            vec!["日e\u{0301}", "1\u{fe0f}\u{20e3}find", " tail"],
            vec![false, true, true],
        );
        let mut literal = engine_literal(true);
        literal.start_search("find", FilterMode::Literal).unwrap();
        literal.scan_all(&mut content);
        let m = &literal.results()[0];
        assert_eq!((m.row, m.start_col, m.end_col), (1, 2, 6));
        assert!(fuzzy.verify_all_invariants());
        assert!(literal.verify_all_invariants());
    }

    // ====================================================================
    // Literal, case and Unicode matching: one row per former test
    // ====================================================================

    #[test]
    fn literal_rows_report_display_column_spans() {
        // (label, pattern, row text, case sensitive, expected (start, end) spans)
        #[allow(clippy::type_complexity, reason = "one labelled row per case")]
        let rows: &[(&str, &str, &str, bool, &[(usize, usize)])] = &[
            ("match at start", "hello", "hello world", true, &[(0, 5)]),
            ("match in middle", "llo", "hello world", true, &[(2, 5)]),
            ("match at end", "world", "hello world", true, &[(6, 11)]),
            (
                "several per line",
                "ab",
                "ab cd ab ef ab",
                true,
                &[(0, 2), (6, 8), (12, 14)],
            ),
            ("no match", "xyz", "hello world", true, &[]),
            ("pattern equals text", "exact", "exact", true, &[(0, 5)]),
            (
                "insensitive finds uppercase",
                "hello",
                "HELLO WORLD",
                false,
                &[(0, 5)],
            ),
            (
                "insensitive finds mixed case",
                "hello",
                "HeLLo World",
                false,
                &[(0, 5)],
            ),
            (
                "sensitive rejects other case",
                "hello",
                "HELLO WORLD",
                true,
                &[],
            ),
            (
                "insensitive, several per line",
                "ab",
                "Ab aB AB ab",
                false,
                &[(0, 2), (3, 5), (6, 8), (9, 11)],
            ),
            ("empty line", "test", "", true, &[]),
            (
                "pattern longer than text",
                "longpattern",
                "short",
                true,
                &[],
            ),
            // CJK is two columns wide: 日 = 0-1, 本 = 2-3.
            ("CJK", "日本", "日本語テスト", true, &[(0, 4)]),
            ("emoji", "🎉", "hello 🎉 world", true, &[(6, 8)]),
            // a = 1 column, あ = 2 columns, b = 1 column.
            ("mixed width", "あ", "aあb", true, &[(1, 3)]),
            (
                "repeated CJK, every one found",
                "日",
                "日日日",
                true,
                &[(0, 2), (2, 4), (4, 6)],
            ),
        ];
        for &(label, pattern, text, case_sensitive, expected) in rows {
            assert_eq!(
                find_in_row(pattern, text, case_sensitive),
                expected,
                "{label}"
            );
        }
    }

    // ====================================================================
    // Fuzzy matching
    // ====================================================================

    #[test]
    fn fuzzy_match_rows() {
        // (label, text, pattern, is a subsequence match)
        let rows: &[(&str, &str, &str, bool)] = &[
            ("exact string", "abc", "abc", true),
            ("empty pattern", "anything", "", true),
            ("empty text and pattern", "", "", true),
            ("reversed order", "hello", "olleh", false),
            ("absent chars", "hello", "xyz", false),
            ("non-empty pattern on empty text", "", "a", false),
        ];
        for &(label, text, pattern, expected) in rows {
            assert_eq!(
                StreamingSearch::fuzzy_match(text, pattern),
                expected,
                "{label}"
            );
        }
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
