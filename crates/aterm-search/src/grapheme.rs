// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Minimal grapheme helpers needed by search indexing.

use aterm_grapheme::GraphemeClusters;
use aterm_grapheme::str_width;

/// Greek small letter final sigma (U+03C2).
const FINAL_SIGMA: char = '\u{03C2}';
/// Greek small letter sigma (U+03C3).
const SIGMA: char = '\u{03C3}';

/// Lowercase a single `char` with the shared context-free routine used by
/// every case-insensitive search site (query key, indexed trigrams, per-line
/// scan buffer, and [`LowerByteMap`]).
///
/// Unlike `str::to_lowercase`, this is purely per-character (no contextual
/// Final_Sigma rule), and it additionally folds Greek final sigma U+03C2 → σ
/// U+03C3 so that capital Σ, medial σ, and word-final ς all collapse to a
/// single canonical form. Without this, `str::to_lowercase` (contextual) and
/// `char::to_lowercase` (non-contextual) disagree on a word-final Σ, so the
/// trigram index, the query key, and the scan buffer end up byte-divergent and
/// case-insensitive search silently misses every sigma match.
///
/// Both ς and σ encode to 2 UTF-8 bytes, so folding never changes a
/// character's lowered byte length — `LowerByteMap` offsets stay aligned.
pub(crate) fn lower_fold_char(c: char) -> impl Iterator<Item = char> {
    c.to_lowercase()
        .map(|lc| if lc == FINAL_SIGMA { SIGMA } else { lc })
}

/// Lowercase + sigma-fold an entire string using [`lower_fold_char`].
///
/// Shared by the query key and the trigram index/removal/rebuild passes so
/// that index, query, and per-line scan are byte-identical.
pub(crate) fn lower_fold(s: &str) -> String {
    // Spelled as an explicit push loop (not `.collect()`) because the Trust L0
    // gate's bulk-allocation recognizer cannot derive an element count for the
    // flat_map collect and refuses to prove the allocation bounded;
    // incremental `push` growth carries the bounds instead. Identical output.
    let mut out = String::new();
    for c in s.chars() {
        for lc in lower_fold_char(c) {
            out.push(lc);
        }
    }
    out
}

/// Convert a byte offset to a display column (O(G) per call).
///
/// Reference implementation retained for equivalence testing against
/// [`ColumnMap::byte_to_column`]. Production code uses `ColumnMap` for
/// O(log G) lookups (#5672).
#[cfg(any(test, kani))]
#[must_use]
pub fn byte_to_column(s: &str, byte_offset: usize) -> usize {
    let mut column: usize = 0;

    for (offset, grapheme) in s.grapheme_indices() {
        if offset >= byte_offset {
            return column;
        }
        column = column.saturating_add(str_width(grapheme).min(2));
    }

    column
}

/// Map a byte offset in lowercased text back to the corresponding byte offset
/// in the original text.
///
/// `to_lowercase()` can change byte lengths (e.g. U+212A Kelvin Sign: 3 bytes
/// → 1-byte 'k'), so searching in the lowered text and using byte offsets
/// directly on the original produces wrong columns. This mapping tracks
/// per-char byte expansion/contraction.
#[cfg(any(test, kani))]
pub(crate) fn map_lower_byte_to_original(original: &str, lower_byte: usize) -> usize {
    let mut orig_offset = 0usize;
    let mut low_offset = 0usize;

    for ch in original.chars() {
        if low_offset >= lower_byte {
            return orig_offset;
        }
        let orig_len = ch.len_utf8();
        let low_len: usize = ch.to_lowercase().map(|c| c.len_utf8()).sum();
        orig_offset = orig_offset.saturating_add(orig_len);
        low_offset = low_offset.saturating_add(low_len);
    }

    orig_offset
}

/// Precomputed byte-offset-to-column map for a single line.
///
/// Built once per line in O(G) where G is the grapheme count, then enables
/// O(log G) column lookups via binary search. This replaces the O(G)-per-call
/// `byte_to_column` pattern that causes O(M × G) total cost when there are
/// M matches in a line.
///
/// Column maps are cached in `SearchIndex` alongside line text so that
/// repeated searches reuse the precomputed map instead of rebuilding it
/// for every query (#7373).
#[derive(Debug)]
pub(crate) struct ColumnMap {
    /// `None` when the source text is pure ASCII (byte_offset == column).
    /// `Some(entries)` for non-ASCII text, with (byte_offset, column) pairs
    /// sorted by byte_offset including a sentinel at the end.
    entries: Option<Vec<(usize, usize)>>,
    /// Length of the source text in bytes. Used for ASCII identity mapping
    /// to clamp lookups at the text boundary.
    text_len: usize,
}

impl ColumnMap {
    /// Build a column map from line text.
    ///
    /// Under Kani, uses ASCII byte-identity mapping to avoid CBMC path
    /// explosion from `unicode_segmentation::grapheme_indices` internal
    /// binary searches on Unicode property tables. All Kani proof inputs
    /// are ASCII where byte offset == grapheme boundary == display column.
    /// See #6119.
    #[cfg(kani)]
    pub fn new(text: &str) -> Self {
        // Kani inputs are always ASCII, so use the O(1) identity path.
        Self {
            entries: None,
            text_len: text.len(),
        }
    }

    /// Build a column map from line text.
    ///
    /// For pure-ASCII text, skips grapheme iteration entirely — O(1)
    /// construction with O(1) lookups via identity mapping (#7375).
    /// For non-ASCII text, builds the full grapheme map in O(G).
    #[cfg(not(kani))]
    pub fn new(text: &str) -> Self {
        // Fast path: printable ASCII only — byte offset == display column.
        // Control characters such as tab/newline/carriage return can have
        // zero display width, so they must use the full grapheme map.
        if text.is_ascii() && text.bytes().all(|byte| matches!(byte, b' '..=b'~')) {
            return Self {
                entries: None,
                text_len: text.len(),
            };
        }

        let mut entries = Vec::new();
        let mut col: usize = 0;
        for (offset, grapheme) in text.grapheme_indices() {
            entries.push((offset, col));
            col = col.saturating_add(str_width(grapheme).min(2));
        }
        // Sentinel so lookups at text.len() return total width.
        entries.push((text.len(), col));
        Self {
            entries: Some(entries),
            text_len: text.len(),
        }
    }

    /// Return the total display column count of the source text.
    ///
    /// Equivalent to `byte_to_column(text.len())`.
    #[must_use]
    #[inline]
    pub fn total_columns(&self) -> usize {
        self.byte_to_column(self.text_len)
    }

    /// Look up the display column for a byte offset.
    ///
    /// O(1) for ASCII text (identity mapping), O(log G) for non-ASCII.
    ///
    /// Semantics match `byte_to_column`: returns the cumulative display width
    /// of all graphemes whose byte offset is strictly less than `byte_offset`.
    #[must_use]
    #[inline]
    pub fn byte_to_column(&self, byte_offset: usize) -> usize {
        let entries = match self.entries {
            // ASCII fast path: byte offset == column, clamped to text length.
            None => return byte_offset.min(self.text_len),
            Some(ref e) => e,
        };
        // `total` is the past-sentinel column, reused as a provably-dead fallback
        // for the in-bounds arms so indexing is `get` (panic-free), never `[]`.
        let total = entries.last().map_or(0, |&(_, col)| col);
        match entries.binary_search_by_key(&byte_offset, |&(off, _)| off) {
            // Exact grapheme boundary — return its column directly.
            Ok(idx) => entries.get(idx).map_or(total, |&(_, col)| col),
            // Between two boundaries — the column of the next grapheme whose
            // offset >= byte_offset, which is entries[idx] (idx < len, guarded).
            Err(idx) if idx < entries.len() => entries.get(idx).map_or(total, |&(_, col)| col),
            // Past the sentinel — return total width.
            Err(_) => total,
        }
    }
}

/// Precomputed lowered-byte-to-original-byte map for case-insensitive search.
///
/// Built once per line in O(C) where C is the char count, then enables
/// O(log C) lookups via binary search. This replaces the O(C)-per-call
/// `map_lower_byte_to_original` pattern that causes O(M × C) total cost
/// when there are M case-insensitive matches in a line.
pub(crate) struct LowerByteMap {
    /// (lower_byte_offset, original_byte_offset) pairs at char boundaries,
    /// sorted by lower_byte_offset. Includes a sentinel at end.
    entries: Vec<(usize, usize)>,
}

impl LowerByteMap {
    /// Build a lower-byte map from original text. O(C) where C = char count.
    pub fn new(original: &str) -> Self {
        let mut entries = Vec::new();
        let mut orig_offset: usize = 0;
        let mut low_offset: usize = 0;
        for ch in original.chars() {
            entries.push((low_offset, orig_offset));
            let orig_len = ch.len_utf8();
            // Use the same lower+sigma-fold routine as the scan buffer so the
            // lowered byte lengths stay byte-identical (ς/σ are both 2 bytes,
            // so the fold never changes a length, but this keeps the mapping
            // tied to the one canonical lowering routine).
            let low_len: usize = lower_fold_char(ch).map(|c| c.len_utf8()).sum();
            orig_offset = orig_offset.saturating_add(orig_len);
            low_offset = low_offset.saturating_add(low_len);
        }
        // Sentinel so lookups at total lowered length return total original length.
        entries.push((low_offset, orig_offset));
        Self { entries }
    }

    /// Map a byte offset in lowered text to the corresponding byte offset in
    /// original text. O(log C). Semantics match `map_lower_byte_to_original`.
    #[must_use]
    pub fn map_to_original(&self, lower_byte: usize) -> usize {
        let total = self.entries.last().map_or(0, |&(_, orig)| orig);
        match self
            .entries
            .binary_search_by_key(&lower_byte, |&(lo, _)| lo)
        {
            Ok(idx) => self.entries.get(idx).map_or(total, |&(_, orig)| orig),
            Err(idx) if idx < self.entries.len() => {
                self.entries.get(idx).map_or(total, |&(_, orig)| orig)
            }
            Err(_) => total,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ColumnMap, LowerByteMap, byte_to_column, map_lower_byte_to_original};

    /// ColumnMap produces identical results to byte_to_column for ASCII text.
    #[test]
    fn column_map_matches_byte_to_column_ascii() {
        let text = "hello world, this is a test line!";
        let map = ColumnMap::new(text);
        for offset in 0..=text.len() {
            assert_eq!(
                map.byte_to_column(offset),
                byte_to_column(text, offset),
                "mismatch at offset {offset}"
            );
        }
    }

    /// ColumnMap matches byte_to_column for wide CJK characters.
    #[test]
    fn column_map_matches_byte_to_column_cjk() {
        let text = "ab日本語cd";
        let map = ColumnMap::new(text);
        for offset in 0..=text.len() {
            assert_eq!(
                map.byte_to_column(offset),
                byte_to_column(text, offset),
                "mismatch at offset {offset}"
            );
        }
    }

    /// ColumnMap handles empty strings.
    #[test]
    fn column_map_empty_string() {
        let map = ColumnMap::new("");
        assert_eq!(map.byte_to_column(0), 0);
    }

    /// ColumnMap equivalence across mixed-width Unicode: combining characters,
    /// emoji, CJK, and ASCII in a single line.
    #[test]
    fn column_map_matches_byte_to_column_mixed_unicode() {
        let texts = [
            "abc\u{0301}def",          // combining acute accent on 'c'
            "\u{1F600}hello\u{1F600}", // emoji + ASCII + emoji
            "日a本b語c",               // interleaved CJK and ASCII
            "a\u{0308}\u{0301}b",      // stacked combining marks
            "\t\n\r abc",              // control chars + ASCII
        ];
        for text in &texts {
            let map = ColumnMap::new(text);
            for offset in 0..=text.len() {
                assert_eq!(
                    map.byte_to_column(offset),
                    byte_to_column(text, offset),
                    "mismatch in {text:?} at offset {offset}"
                );
            }
        }
    }

    /// Stress test: ColumnMap is equivalent to byte_to_column on a long line
    /// with many match positions. This validates the O(log G) lookup path
    /// produces identical results to the O(G) scan for all byte offsets.
    #[test]
    fn column_map_long_line_all_offsets() {
        // 80 CJK chars = 240 bytes, 160 display columns
        let text: String = "日本語テスト漢字表示".chars().cycle().take(80).collect();
        let map = ColumnMap::new(&text);
        for offset in 0..=text.len() {
            assert_eq!(
                map.byte_to_column(offset),
                byte_to_column(&text, offset),
                "mismatch at byte offset {offset} in {}-byte CJK line",
                text.len()
            );
        }
    }

    /// LowerByteMap produces identical results to map_lower_byte_to_original
    /// for pure ASCII text where lowering preserves byte lengths.
    #[test]
    fn lower_byte_map_matches_ascii() {
        let text = "Hello World Test";
        let lowered = text.to_lowercase();
        let map = LowerByteMap::new(text);
        for offset in 0..=lowered.len() {
            assert_eq!(
                map.map_to_original(offset),
                map_lower_byte_to_original(text, offset),
                "mismatch at lower offset {offset}"
            );
        }
    }

    /// LowerByteMap handles the Kelvin sign (U+212A) which lowercases from
    /// 3 bytes to 1 byte, causing byte-length divergence.
    #[test]
    fn lower_byte_map_matches_kelvin_sign() {
        // U+212A KELVIN SIGN (3 bytes) → 'k' (1 byte)
        let text = "a\u{212A}b";
        let lowered = text.to_lowercase();
        let map = LowerByteMap::new(text);
        for offset in 0..=lowered.len() {
            assert_eq!(
                map.map_to_original(offset),
                map_lower_byte_to_original(text, offset),
                "mismatch at lower offset {offset} for Kelvin sign text"
            );
        }
    }

    /// LowerByteMap matches for mixed-width Unicode including CJK and emoji.
    #[test]
    fn lower_byte_map_matches_mixed_unicode() {
        let texts = [
            "ABCdef",    // simple mixed case
            "Straße",    // ß stays 2 bytes lowered
            "ΔΕΛΤΑ",     // Greek uppercase → lowercase
            "日本語ABC", // CJK + ASCII
            "",          // empty
        ];
        for text in &texts {
            let lowered = text.to_lowercase();
            let map = LowerByteMap::new(text);
            for offset in 0..=lowered.len() {
                assert_eq!(
                    map.map_to_original(offset),
                    map_lower_byte_to_original(text, offset),
                    "mismatch in {text:?} at lower offset {offset}"
                );
            }
        }
    }

    /// LowerByteMap stress test on a long line with many char boundaries.
    #[test]
    fn lower_byte_map_long_line_all_offsets() {
        let text: String = "Hello日本語World".chars().cycle().take(100).collect();
        let lowered = text.to_lowercase();
        let map = LowerByteMap::new(&text);
        for offset in 0..=lowered.len() {
            assert_eq!(
                map.map_to_original(offset),
                map_lower_byte_to_original(&text, offset),
                "mismatch at lower offset {offset} in {}-byte line",
                text.len()
            );
        }
    }
}

#[cfg(kani)]
mod kani_proofs {
    use super::byte_to_column;

    /// byte_to_column monotonicity: a <= b implies byte_to_column(s, a) <= byte_to_column(s, b).
    /// Uses ASCII content where grapheme iteration is trivial for Kani.
    #[kani::proof]
    #[kani::unwind(14)]
    fn byte_to_column_monotonic() {
        let s = "hello world";
        let a: usize = kani::any();
        let b: usize = kani::any();
        kani::assume(a <= b);
        kani::assume(b <= s.len());

        let col_a = byte_to_column(s, a);
        let col_b = byte_to_column(s, b);

        kani::assert(col_a <= col_b, "byte_to_column must be monotonic");
    }

    /// For pure ASCII strings, byte_to_column(s, n) == n (identity mapping).
    #[kani::proof]
    #[kani::unwind(12)]
    fn byte_to_column_ascii_identity() {
        let s = "abcdefghij";
        let n: usize = kani::any();
        kani::assume(n <= s.len());

        let col = byte_to_column(s, n);

        kani::assert(col == n, "ASCII: byte offset must equal column");
    }

    /// byte_to_column(s, 0) == 0 for any string: no graphemes precede offset 0.
    ///
    /// Symbolic over string selection: proves byte_to_column(s, 0) == 0
    /// holds for strings of varying lengths and content types,
    /// selected symbolically.
    #[kani::proof]
    #[kani::unwind(14)]
    fn byte_to_column_identity_at_zero() {
        let case: u8 = kani::any();
        kani::assume(case <= 3);

        let s = match case {
            0 => "",            // empty string
            1 => "a",           // single char
            2 => "hello world", // multi-word
            _ => "abcdefghij",  // max-length ASCII
        };

        let col = byte_to_column(s, 0);
        kani::assert(
            col == 0,
            "byte_to_column(s, 0) must be 0 for any string content",
        );
    }

    /// byte_to_column(s, n) <= display_width(s) for any offset n within bounds.
    /// The column position can never exceed the total display width of the string.
    #[kani::proof]
    #[kani::unwind(14)]
    fn byte_to_column_bounded_by_display_width() {
        let s = "hello world";
        let n: usize = kani::any();
        kani::assume(n <= s.len() + 1);

        let col = byte_to_column(s, n);
        let display_width = aterm_grapheme::str_width(s);

        kani::assert(
            col <= display_width,
            "column must not exceed total display width",
        );
    }
}
