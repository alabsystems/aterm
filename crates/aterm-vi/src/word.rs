// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Semantic and whitespace word motions (w/b/e/ge, W/B/E/gE).
//!
//! Semantic motions use a configurable separator string. Whitespace
//! (WORD) motions use a fixed set: space, tab, null.

use aterm_types::BufferAccess;

use super::navigation::{LineCache, point_backward, point_forward};
use super::types::ViPoint;

// ---------------------------------------------------------------------------
// Character classification
// ---------------------------------------------------------------------------

/// Whether `ch` is a semantic word separator.
fn is_separator(ch: char, separators: &str) -> bool {
    ch == '\0' || separators.contains(ch)
}

/// Whether `ch` is whitespace for WORD motions.
fn is_whitespace(ch: char) -> bool {
    ch == ' ' || ch == '\t' || ch == '\0'
}

// ---------------------------------------------------------------------------
// Semantic word motions (w / b / e / ge)
// ---------------------------------------------------------------------------

/// Move to the start of the next semantic word (vim `w`).
pub fn semantic_word_right(grid: &dyn BufferAccess, point: ViPoint, separators: &str) -> ViPoint {
    // One materialization per scrollback line scanned (not per cell).
    let mut cache = LineCache::new();
    let mut cur = point;

    // Skip current word characters (non-separators).
    while !is_separator(cache.word_char_at(grid, cur), separators) {
        match point_forward(grid, cur) {
            Some(p) => cur = p,
            None => return cur,
        }
    }

    // Skip separators / spaces.
    while is_separator(cache.word_char_at(grid, cur), separators) {
        match point_forward(grid, cur) {
            Some(p) => cur = p,
            None => return cur,
        }
    }

    cur
}

/// Move to the start of the previous semantic word (vim `b`).
pub fn semantic_word_left(grid: &dyn BufferAccess, point: ViPoint, separators: &str) -> ViPoint {
    // One materialization per scrollback line scanned (not per cell).
    let mut cache = LineCache::new();
    let Some(mut cur) = point_backward(grid, point) else {
        return point;
    };

    // Skip separators backward.
    while is_separator(cache.word_char_at(grid, cur), separators) {
        match point_backward(grid, cur) {
            Some(p) => cur = p,
            None => return cur,
        }
    }

    // Skip word characters backward until a separator is found.
    while !is_separator(cache.word_char_at(grid, cur), separators) {
        match point_backward(grid, cur) {
            Some(p) => {
                if is_separator(cache.word_char_at(grid, p), separators) {
                    return cur;
                }
                cur = p;
            }
            None => return cur,
        }
    }

    // Step forward past the separator we landed on.
    point_forward(grid, cur).unwrap_or(cur)
}

/// Move to the end of the current/next semantic word (vim `e`).
pub fn semantic_word_right_end(
    grid: &dyn BufferAccess,
    point: ViPoint,
    separators: &str,
) -> ViPoint {
    // One materialization per scrollback line scanned (not per cell).
    let mut cache = LineCache::new();
    let Some(mut cur) = point_forward(grid, point) else {
        return point;
    };

    // Skip separators.
    while is_separator(cache.word_char_at(grid, cur), separators) {
        match point_forward(grid, cur) {
            Some(p) => cur = p,
            None => return cur,
        }
    }

    // Advance through word characters, stopping at the last one.
    loop {
        match point_forward(grid, cur) {
            Some(p) => {
                if is_separator(cache.word_char_at(grid, p), separators) {
                    return cur;
                }
                cur = p;
            }
            None => return cur,
        }
    }
}

/// Move to the end of the previous semantic word (vim `ge`).
pub fn semantic_word_left_end(
    grid: &dyn BufferAccess,
    point: ViPoint,
    separators: &str,
) -> ViPoint {
    // One materialization per scrollback line scanned (not per cell).
    let mut cache = LineCache::new();
    let Some(mut cur) = point_backward(grid, point) else {
        return point;
    };

    // Skip current word characters backward.
    while !is_separator(cache.word_char_at(grid, cur), separators) {
        match point_backward(grid, cur) {
            Some(p) => cur = p,
            None => return cur,
        }
    }

    // Skip separators backward.
    while is_separator(cache.word_char_at(grid, cur), separators) {
        match point_backward(grid, cur) {
            Some(p) => cur = p,
            None => return cur,
        }
    }

    cur
}

// ---------------------------------------------------------------------------
// Whitespace word motions (W / B / E / gE)
// ---------------------------------------------------------------------------

/// Move to the start of the next WORD (vim `W`).
pub fn whitespace_word_right(grid: &dyn BufferAccess, point: ViPoint) -> ViPoint {
    // One materialization per scrollback line scanned (not per cell).
    let mut cache = LineCache::new();
    let mut cur = point;

    // Skip non-whitespace.
    while !is_whitespace(cache.word_char_at(grid, cur)) {
        match point_forward(grid, cur) {
            Some(p) => cur = p,
            None => return cur,
        }
    }

    // Skip whitespace.
    while is_whitespace(cache.word_char_at(grid, cur)) {
        match point_forward(grid, cur) {
            Some(p) => cur = p,
            None => return cur,
        }
    }

    cur
}

/// Move to the start of the previous WORD (vim `B`).
pub fn whitespace_word_left(grid: &dyn BufferAccess, point: ViPoint) -> ViPoint {
    // One materialization per scrollback line scanned (not per cell).
    let mut cache = LineCache::new();
    let Some(mut cur) = point_backward(grid, point) else {
        return point;
    };

    // Skip whitespace backward.
    while is_whitespace(cache.word_char_at(grid, cur)) {
        match point_backward(grid, cur) {
            Some(p) => cur = p,
            None => return cur,
        }
    }

    // Skip non-whitespace backward until whitespace is found.
    while !is_whitespace(cache.word_char_at(grid, cur)) {
        match point_backward(grid, cur) {
            Some(p) => {
                if is_whitespace(cache.word_char_at(grid, p)) {
                    return cur;
                }
                cur = p;
            }
            None => return cur,
        }
    }

    point_forward(grid, cur).unwrap_or(cur)
}

/// Move to the end of the current/next WORD (vim `E`).
pub fn whitespace_word_right_end(grid: &dyn BufferAccess, point: ViPoint) -> ViPoint {
    // One materialization per scrollback line scanned (not per cell).
    let mut cache = LineCache::new();
    let Some(mut cur) = point_forward(grid, point) else {
        return point;
    };

    // Skip whitespace.
    while is_whitespace(cache.word_char_at(grid, cur)) {
        match point_forward(grid, cur) {
            Some(p) => cur = p,
            None => return cur,
        }
    }

    // Advance through non-whitespace, stopping at the last one.
    loop {
        match point_forward(grid, cur) {
            Some(p) => {
                if is_whitespace(cache.word_char_at(grid, p)) {
                    return cur;
                }
                cur = p;
            }
            None => return cur,
        }
    }
}

/// Move to the end of the previous WORD (vim `gE`).
pub fn whitespace_word_left_end(grid: &dyn BufferAccess, point: ViPoint) -> ViPoint {
    // One materialization per scrollback line scanned (not per cell).
    let mut cache = LineCache::new();
    let Some(mut cur) = point_backward(grid, point) else {
        return point;
    };

    // Skip non-whitespace backward.
    while !is_whitespace(cache.word_char_at(grid, cur)) {
        match point_backward(grid, cur) {
            Some(p) => cur = p,
            None => return cur,
        }
    }

    // Skip whitespace backward.
    while is_whitespace(cache.word_char_at(grid, cur)) {
        match point_backward(grid, cur) {
            Some(p) => cur = p,
            None => return cur,
        }
    }

    cur
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::navigation::DEFAULT_SEPARATORS;
    use crate::test_utils::MockGrid;

    const SEP: &str = DEFAULT_SEPARATORS;

    // ---- Semantic word motions (w / b / e / ge) ----

    /// `(what, motion, line, start col, landing col)` on a one-row grid —
    /// the semantic motions (w / b / e / ge) stop at separators, the
    /// whitespace ones (W / B / E / gE) only at blanks.
    #[test]
    fn word_motion_landings() {
        type Motion = fn(&MockGrid, ViPoint) -> ViPoint;
        let w: Motion = |g, p| semantic_word_right(g, p, SEP);
        let b: Motion = |g, p| semantic_word_left(g, p, SEP);
        let e: Motion = |g, p| semantic_word_right_end(g, p, SEP);
        let ge: Motion = |g, p| semantic_word_left_end(g, p, SEP);
        let big_w: Motion = |g, p| whitespace_word_right(g, p);
        let big_b: Motion = |g, p| whitespace_word_left(g, p);
        let big_e: Motion = |g, p| whitespace_word_right_end(g, p);
        let big_ge: Motion = |g, p| whitespace_word_left_end(g, p);
        for (what, motion, line, cols, start, col) in [
            (
                "w skips the word, then the spaces",
                w,
                "hello world",
                20,
                0,
                6,
            ),
            (
                "b finds the start of the current word",
                b,
                "hello world",
                20,
                8,
                6,
            ),
            ("b jumps to the previous word", b, "hello world", 20, 6, 0),
            (
                "e finds the end of the next word",
                e,
                "hello world",
                20,
                0,
                4,
            ),
            (
                "ge finds the end of the previous word",
                ge,
                "hello world",
                20,
                8,
                4,
            ),
            ("W skips over punctuation", big_w, "foo.bar baz", 30, 0, 8),
            ("B finds the start", big_b, "foo.bar baz", 30, 9, 8),
            ("E finds the end", big_e, "foo.bar baz", 30, 0, 6),
            (
                "gE finds the end of the previous",
                big_ge,
                "foo.bar baz",
                30,
                9,
                6,
            ),
        ] {
            let grid = MockGrid::new(1, cols).with_line(0, line);
            assert_eq!(motion(&grid, ViPoint::new(0, start)).col, col, "{what}");
        }
    }

    #[test]
    fn word_right_stops_at_end() {
        let grid = MockGrid::new(1, 10).with_line(0, "abcdef");
        let result = semantic_word_right(&grid, ViPoint::new(0, 0), SEP);
        assert_eq!(result.line, 0);
    }

    // ---- Whitespace word motions (W / B / E / gE) ----

    // ---- Scrollback content traversal (one-motion line cache) ----

    /// `b` from inside the visible region must walk back across the line
    /// boundary into scrollback to the start of an unbroken word, reading
    /// the scrollback row through the one-motion line cache (one
    /// materialization for the whole `-1` row, not one per cell).
    #[test]
    fn word_left_traverses_scrollback_content() {
        // A single unbroken word spans the newest scrollback line (-1,
        // "aaaaa") and the first visible line (0, "bbbbb").
        let grid = MockGrid::new(2, 5)
            .with_scrollback_lines(&["aaaaa"]) // line -1
            .with_line(0, "bbbbb"); // line 0
        let result = semantic_word_left(&grid, ViPoint::new(0, 2), SEP);
        assert_eq!(
            result,
            ViPoint::new(-1, 0),
            "b should walk back to the word start in scrollback"
        );
    }

    /// CJK RUNS ARE WORDS (2026-07-24). A double-width character stores a
    /// literal `' '` in its continuation cell, and `' '` is both a
    /// `DEFAULT_SEPARATORS` member and whitespace for WORD motions — so before
    /// this fix `日本語` read as `日 · 本 · 語` and every motion stepped ONE
    /// GLYPH. Alacritty, which these motions are ported from, skips the spacer
    /// explicitly; aterm did not.
    #[test]
    fn cjk_runs_are_crossed_as_one_word() {
        // Columns:  0=日 1=cont 2=本 3=cont 4=語 5=cont 6=' ' 7=n 8=e 9=x 10=t
        let grid = MockGrid::new(4, 24).with_wide_text(0, "日本語 next", "日本語");

        // `w` from inside the run must reach the NEXT word, not the next glyph.
        let after = semantic_word_right(&grid, ViPoint::new(0, 0), DEFAULT_SEPARATORS);
        assert_eq!(
            after,
            ViPoint::new(0, 7),
            "w stepped inside the CJK run instead of crossing it"
        );

        // `W` (whitespace-delimited) must agree — same root cause, same fix.
        let after_w = whitespace_word_right(&grid, ViPoint::new(0, 0));
        assert_eq!(
            after_w,
            ViPoint::new(0, 7),
            "W stepped inside the CJK run instead of crossing it"
        );

        // …and coming back crosses the whole run to its first cell.
        let back = semantic_word_left(&grid, ViPoint::new(0, 7), DEFAULT_SEPARATORS);
        assert_eq!(
            back,
            ViPoint::new(0, 0),
            "b stopped inside the CJK run instead of at its start"
        );
    }

    /// The fix must not merge across a REAL space. A space that is genuinely a
    /// space — not a continuation spacer — still separates.
    #[test]
    fn a_real_space_between_cjk_runs_still_separates() {
        // 日本 | space | 語 — the middle space is real, not a continuation.
        let grid = MockGrid::new(4, 24).with_wide_text(0, "日本 語", "日本語");
        let after = semantic_word_right(&grid, ViPoint::new(0, 0), DEFAULT_SEPARATORS);
        assert_eq!(
            after,
            ViPoint::new(0, 5),
            "a real space between CJK runs must still break the word"
        );
    }
}
