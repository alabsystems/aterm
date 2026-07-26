// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Vi mode navigation: bracket matching, paragraph motions, inline
//! search, and shared traversal helpers.
//!
//! Word motions (w/b/e/ge, W/B/E/gE) are in [`super::word`].
//! All functions operate through [`BufferAccess`]
//! and [`ViPoint`].

use aterm_types::BufferAccess;

use super::cell_char;
use super::types::ViPoint;

/// Default semantic separator characters (matches Alacritty default).
pub const DEFAULT_SEPARATORS: &str = ",│`|:\"' ()[]{}<>\t";

// ---------------------------------------------------------------------------
// Point traversal (pub(super) for use by word.rs)
// ---------------------------------------------------------------------------

/// Advance one cell forward (right then down), returning `None` at
/// the bottom-right corner of the grid.
pub(super) fn point_forward(grid: &dyn BufferAccess, p: ViPoint) -> Option<ViPoint> {
    let cols = grid.cols();
    // saturating_add: identical for every real point (p.col < cols); a
    // saturated u16::MAX fails `next_col < cols` for any cols <= u16::MAX.
    let next_col = p.col.saturating_add(1);
    if next_col < cols {
        Some(ViPoint::new(p.line, next_col))
    } else {
        // saturating ops are exact here: `i32::from(u16)` is >= 0, and
        // `p.line < bottom` implies `p.line < i32::MAX`.
        let bottom = i32::from(grid.visible_rows()).saturating_sub(1);
        if p.line < bottom {
            Some(ViPoint::new(p.line.saturating_add(1), 0))
        } else {
            None
        }
    }
}

/// Retreat one cell backward (left then up), returning `None` at
/// the top-left corner.
pub(super) fn point_backward(grid: &dyn BufferAccess, p: ViPoint) -> Option<ViPoint> {
    if p.col > 0 {
        Some(ViPoint::new(p.line, p.col - 1))
    } else {
        // saturating_neg: identical for every real grid (total_lines >= 0); removes
        // the provable i32::MIN negation-overflow Level-0 obligation.
        let top = grid.total_lines().saturating_neg();
        if p.line > top {
            Some(ViPoint::new(p.line - 1, grid.cols().saturating_sub(1)))
        } else {
            None
        }
    }
}

// ---------------------------------------------------------------------------
// One-motion scrollback line cache
// ---------------------------------------------------------------------------

/// One-motion line cache for cell-by-cell scrollback scanners.
///
/// Reading a scrollback cell through [`BufferAccess::char_at`]
/// re-materializes the WHOLE row on every call (a fresh row
/// reconstruction plus heap allocations). The vi scanners that walk
/// across scrollback one cell at a time (`b`/`B`/`ge`/`gE`, the forward
/// word motions, and backward `%`) would therefore pay that full-row cost
/// once per cell, making an unbroken single-line scan quadratic in the row
/// width. This cache holds the most-recently materialized scrollback line
/// — resolved to one char per cell, exactly as `char_at` yields — so
/// consecutive reads on the same line reuse a single materialization.
///
/// Visible rows (`line >= 0`) are read directly: their `char_at` is a
/// cheap cell lookup with no allocation, so caching them would only add
/// overhead. The cache lives for a single motion call, so there is no
/// cross-mutation invalidation concern.
pub(super) struct LineCache {
    /// `(line, resolved-per-cell chars)` for the cached scrollback line.
    ///
    /// Stored as `Vec<char>` rather than the materialized `String` so a
    /// per-cell read indexes in O(1); a `String` would force an O(col)
    /// `chars().nth(col)` per read, re-introducing the quadratic this
    /// cache exists to remove.
    row: Option<(i32, Vec<char>)>,
    /// Wide-CONTINUATION flags for the same cached line, from ONE
    /// [`BufferAccess::line_wide_continuations`] call. Kept beside the chars
    /// (not derived per read) for exactly the reason the chars are: a per-cell
    /// probe would re-materialize the scrollback row once per column.
    conts: Option<(i32, Vec<bool>)>,
}

impl LineCache {
    /// Create an empty cache.
    pub(super) fn new() -> Self {
        Self {
            row: None,
            conts: None,
        }
    }

    /// Read the character at `point`, reusing a materialized scrollback
    /// row across consecutive reads on the same line.
    ///
    /// Behaviour-identical to [`cell_char`]: out-of-bounds cells read as
    /// space. `line_text` resolves each scrollback cell with the same
    /// complex-char first-char mapping as `char_at`, so the cached chars
    /// match `char_at` column-for-column.
    /// [`Self::char_at`] for WORD MOTIONS: a double-width character's
    /// continuation cell resolves to the character itself, not to the literal
    /// `' '` the grid stores there.
    ///
    /// Without this, `日本語` reads as `日 · 本 · 語` — the classifier sees a
    /// space between every glyph (`' '` is in [`DEFAULT_SEPARATORS`] and is
    /// whitespace for WORD motions), so `w`/`b`/`e`/`W`/`B`/`E` step ONE glyph
    /// at a time through CJK instead of crossing the run. Alacritty, which
    /// these motions are ported from, skips the spacer explicitly.
    ///
    /// Deliberately SEPARATE from [`Self::char_at`]: that function's documented
    /// contract is to be behaviour-identical to `cell_char` and to agree with
    /// `line_text` column-for-column, which bracket matching and the paragraph
    /// motions rely on. Only the word classifier wants the merged reading.
    pub(super) fn word_char_at(&mut self, grid: &dyn BufferAccess, point: ViPoint) -> char {
        let ch = self.char_at(grid, point);
        if point.col == 0 || !self.is_wide_continuation(grid, point) {
            return ch;
        }
        // Resolve to the lead cell. A continuation is always exactly one cell
        // past its lead, so this is a single step, not a walk.
        self.char_at(grid, ViPoint::new(point.line, point.col - 1))
    }

    /// Whether `point` is the SECOND half of a double-width character, from the
    /// batched per-line flags (one grid call per line, cached beside the text).
    fn is_wide_continuation(&mut self, grid: &dyn BufferAccess, point: ViPoint) -> bool {
        let needs_refresh = match &self.conts {
            Some((line, _)) => *line != point.line,
            None => true,
        };
        if needs_refresh {
            let flags = grid.line_wide_continuations(point.line).unwrap_or_default();
            self.conts = Some((point.line, flags));
        }
        self.conts
            .as_ref()
            .and_then(|(_, f)| f.get(point.col as usize).copied())
            .unwrap_or(false)
    }

    pub(super) fn char_at(&mut self, grid: &dyn BufferAccess, point: ViPoint) -> char {
        if point.line >= 0 {
            // Visible rows: a direct lookup is already cheap and avoids
            // allocating a String.
            return cell_char(grid, point);
        }
        // Scrollback: materialize the row once and reuse it for later
        // reads on the same line.
        let needs_refresh = match &self.row {
            Some((line, _)) => *line != point.line,
            None => true,
        };
        if needs_refresh {
            // Explicit match (not `unwrap_or_default`) so the hardened
            // boundary sees the None -> empty-row mapping as deliberate:
            // out-of-history lines read as all-spaces, same as `char_at`.
            let chars: Vec<char> = match grid.line_text(point.line) {
                Some(t) => t.chars().collect(),
                None => Vec::new(),
            };
            self.row = Some((point.line, chars));
        }
        self.row
            .as_ref()
            .and_then(|(_, chars)| chars.get(point.col as usize).copied())
            .unwrap_or(' ')
    }
}

// ---------------------------------------------------------------------------
// Bracket matching (%)
// ---------------------------------------------------------------------------

/// Bracket pairs for matching.
const BRACKET_PAIRS: &[(char, char)] = &[('(', ')'), ('[', ']'), ('{', '}'), ('<', '>')];

/// Find the matching bracket for the character at `point` (vim `%`).
///
/// Returns `None` if the character is not a bracket or no match exists.
pub fn bracket_match(grid: &dyn BufferAccess, point: ViPoint) -> Option<ViPoint> {
    // One materialization per scrollback line scanned (not per cell).
    let mut cache = LineCache::new();
    let ch = cache.char_at(grid, point);

    // Determine the paired bracket and scan direction.
    let (pair, forward) = BRACKET_PAIRS.iter().find_map(|&(open, close)| {
        if ch == open {
            Some((close, true))
        } else if ch == close {
            Some((open, false))
        } else {
            None
        }
    })?;

    let mut depth: u32 = 1;
    let mut cur = point;

    loop {
        cur = if forward {
            point_forward(grid, cur)?
        } else {
            point_backward(grid, cur)?
        };

        let c = cache.char_at(grid, cur);
        if c == ch {
            // saturating_add: identical for every real grid — depth is
            // bounded by the number of bracket cells scanned, and no
            // materializable grid holds u32::MAX bracket cells.
            depth = depth.saturating_add(1);
        } else if c == pair {
            // saturating_sub is exact: depth == 0 returns immediately
            // below, so depth >= 1 on every path reaching this decrement.
            depth = depth.saturating_sub(1);
            if depth == 0 {
                return Some(cur);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Paragraph motions ({ / })
// ---------------------------------------------------------------------------

/// Move up to the previous empty line (vim `{`).
pub fn paragraph_up(grid: &dyn BufferAccess, point: ViPoint) -> ViPoint {
    // saturating_neg: identical for every real grid (total_lines >= 0); removes
    // the provable i32::MIN negation-overflow Level-0 obligation.
    let top = grid.total_lines().saturating_neg();
    let mut line = point.line;

    // Move up at least one line.
    if line > top {
        line -= 1;
    } else {
        return ViPoint::new(top, 0);
    }

    while line > top {
        if is_line_empty(grid, line) {
            return ViPoint::new(line, 0);
        }
        line -= 1;
    }

    ViPoint::new(top, 0)
}

/// Move down to the next empty line (vim `}`).
pub fn paragraph_down(grid: &dyn BufferAccess, point: ViPoint) -> ViPoint {
    // saturating ops are exact here: `i32::from(u16)` is >= 0, and both
    // increments are guarded by `line < bottom` (so `line < i32::MAX`).
    let bottom = i32::from(grid.visible_rows()).saturating_sub(1);
    let mut line = point.line;

    // Move down at least one line.
    if line < bottom {
        line = line.saturating_add(1);
    } else {
        return ViPoint::new(bottom, 0);
    }

    while line < bottom {
        if is_line_empty(grid, line) {
            return ViPoint::new(line, 0);
        }
        line = line.saturating_add(1);
    }

    ViPoint::new(bottom, 0)
}

/// Check if every cell on `line` is whitespace or null.
fn is_line_empty(grid: &dyn BufferAccess, line: i32) -> bool {
    // Scrollback rows (line < 0) are expensive to read cell-by-cell: each
    // `char_at` re-materializes the entire row. Fetch the row once via
    // `line_text` (same per-cell resolution as `char_at`) and test
    // emptiness in a single pass. `None` (out of retained history) reads
    // as empty, matching the old per-cell `char_at(..).unwrap_or(' ')`.
    if line < 0 {
        return match grid.line_text(line) {
            None => true,
            Some(t) => t.chars().all(|c| matches!(c, ' ' | '\t' | '\0')),
        };
    }
    // Visible rows: a per-cell `char_at` is a cheap lookup that avoids
    // allocating a String.
    let cols = grid.cols();
    for col in 0..cols {
        let ch = cell_char(grid, ViPoint::new(line, col));
        if ch != ' ' && ch != '\t' && ch != '\0' {
            return false;
        }
    }
    true
}

// ---------------------------------------------------------------------------
// Inline search (f / F / t / T)
// ---------------------------------------------------------------------------

/// Perform an inline character search from `point` to the right.
///
/// Returns the position of the first matching character, or `None`.
pub fn inline_search_right(
    grid: &dyn BufferAccess,
    point: ViPoint,
    needle: char,
) -> Option<ViPoint> {
    let cols = grid.cols();
    // saturating_add: identical for every real cursor (point.col < cols),
    // and a saturated u16::MAX fails `col < cols` immediately; the loop
    // step cannot saturate at all because `col < cols <= u16::MAX`.
    let mut col = point.col.saturating_add(1);
    while col < cols {
        if cell_char(grid, ViPoint::new(point.line, col)) == needle {
            return Some(ViPoint::new(point.line, col));
        }
        col = col.saturating_add(1);
    }
    None
}

/// Perform an inline character search from `point` to the left.
///
/// Returns the position of the first matching character, or `None`.
pub fn inline_search_left(
    grid: &dyn BufferAccess,
    point: ViPoint,
    needle: char,
) -> Option<ViPoint> {
    let mut col = point.col;
    while col > 0 {
        col -= 1;
        if cell_char(grid, ViPoint::new(point.line, col)) == needle {
            return Some(ViPoint::new(point.line, col));
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::MockGrid;

    // ---- Bracket matching ----

    #[test]
    fn bracket_match_forward() {
        let grid = MockGrid::new(1, 20).with_line(0, "(hello)");
        assert_eq!(
            bracket_match(&grid, ViPoint::new(0, 0)),
            Some(ViPoint::new(0, 6))
        );
    }

    #[test]
    fn bracket_match_backward() {
        let grid = MockGrid::new(1, 20).with_line(0, "(hello)");
        assert_eq!(
            bracket_match(&grid, ViPoint::new(0, 6)),
            Some(ViPoint::new(0, 0))
        );
    }

    #[test]
    fn bracket_match_nested() {
        let grid = MockGrid::new(1, 20).with_line(0, "((a)(b))");
        assert_eq!(
            bracket_match(&grid, ViPoint::new(0, 0)),
            Some(ViPoint::new(0, 7))
        );
    }

    #[test]
    fn bracket_match_none_for_non_bracket() {
        let grid = MockGrid::new(1, 10).with_line(0, "hello");
        assert_eq!(bracket_match(&grid, ViPoint::new(0, 0)), None);
    }

    #[test]
    fn bracket_match_curly() {
        let grid = MockGrid::new(1, 10).with_line(0, "{x}");
        assert_eq!(
            bracket_match(&grid, ViPoint::new(0, 0)),
            Some(ViPoint::new(0, 2))
        );
    }

    #[test]
    fn bracket_match_angle() {
        let grid = MockGrid::new(1, 20).with_line(0, "<html>");
        assert_eq!(
            bracket_match(&grid, ViPoint::new(0, 0)),
            Some(ViPoint::new(0, 5))
        );
    }

    #[test]
    fn bracket_match_angle_backward() {
        let grid = MockGrid::new(1, 20).with_line(0, "<x>");
        assert_eq!(
            bracket_match(&grid, ViPoint::new(0, 2)),
            Some(ViPoint::new(0, 0))
        );
    }

    #[test]
    fn bracket_match_empty_pair() {
        let grid = MockGrid::new(1, 10).with_line(0, "()");
        assert_eq!(
            bracket_match(&grid, ViPoint::new(0, 0)),
            Some(ViPoint::new(0, 1))
        );
    }

    #[test]
    fn bracket_match_deeply_nested() {
        let grid = MockGrid::new(1, 20).with_line(0, "(((())))");
        assert_eq!(
            bracket_match(&grid, ViPoint::new(0, 0)),
            Some(ViPoint::new(0, 7))
        );
    }

    #[test]
    fn bracket_match_mixed_nesting() {
        let grid = MockGrid::new(1, 20).with_line(0, "([{<>}])");
        assert_eq!(
            bracket_match(&grid, ViPoint::new(0, 0)),
            Some(ViPoint::new(0, 7))
        );
    }

    #[test]
    fn bracket_match_unmatched_open() {
        let grid = MockGrid::new(1, 10).with_line(0, "(abc");
        assert_eq!(bracket_match(&grid, ViPoint::new(0, 0)), None);
    }

    #[test]
    fn bracket_match_unmatched_close() {
        let grid = MockGrid::new(1, 10).with_line(0, "abc)");
        assert_eq!(bracket_match(&grid, ViPoint::new(0, 3)), None);
    }

    #[test]
    fn bracket_match_multirow() {
        let grid = MockGrid::new(3, 10)
            .with_line(0, "(         ")
            .with_line(1, "  hello   ")
            .with_line(2, "         )");
        assert_eq!(
            bracket_match(&grid, ViPoint::new(0, 0)),
            Some(ViPoint::new(2, 9))
        );
    }

    #[test]
    fn bracket_match_all_spaces_grid() {
        // Grid of all spaces — bracket at (0,0) won't match
        let grid = MockGrid::new(1, 10);
        assert_eq!(bracket_match(&grid, ViPoint::new(0, 0)), None);
    }

    // ---- Paragraph motions ----

    #[test]
    fn paragraph_up_finds_empty_line() {
        let grid = MockGrid::new(5, 10)
            .with_line(0, "aaa")
            .with_line(2, "bbb")
            .with_line(3, "ccc")
            .with_line(4, "ddd");
        assert_eq!(paragraph_up(&grid, ViPoint::new(3, 0)).line, 1);
    }

    #[test]
    fn paragraph_down_finds_empty_line() {
        let grid = MockGrid::new(5, 10)
            .with_line(0, "aaa")
            .with_line(1, "bbb")
            .with_line(3, "ccc")
            .with_line(4, "ddd");
        assert_eq!(paragraph_down(&grid, ViPoint::new(0, 0)).line, 2);
    }

    #[test]
    fn paragraph_up_clamps_to_top() {
        let grid = MockGrid::new(3, 10)
            .with_line(0, "aaa")
            .with_line(1, "bbb")
            .with_line(2, "ccc");
        assert_eq!(paragraph_up(&grid, ViPoint::new(2, 0)).line, 0);
    }

    #[test]
    fn paragraph_down_clamps_to_bottom() {
        let grid = MockGrid::new(3, 10)
            .with_line(0, "aaa")
            .with_line(1, "bbb")
            .with_line(2, "ccc");
        assert_eq!(paragraph_down(&grid, ViPoint::new(0, 0)).line, 2);
    }

    // ---- Inline search ----

    #[test]
    fn inline_search_right_finds_char() {
        let grid = MockGrid::new(1, 20).with_line(0, "hello world");
        assert_eq!(
            inline_search_right(&grid, ViPoint::new(0, 0), 'o'),
            Some(ViPoint::new(0, 4))
        );
    }

    #[test]
    fn inline_search_left_finds_char() {
        let grid = MockGrid::new(1, 20).with_line(0, "hello world");
        assert_eq!(
            inline_search_left(&grid, ViPoint::new(0, 10), 'o'),
            Some(ViPoint::new(0, 7))
        );
    }

    #[test]
    fn inline_search_right_not_found() {
        let grid = MockGrid::new(1, 20).with_line(0, "hello");
        assert_eq!(inline_search_right(&grid, ViPoint::new(0, 0), 'z'), None);
    }

    #[test]
    fn inline_search_left_not_found() {
        let grid = MockGrid::new(1, 20).with_line(0, "hello");
        assert_eq!(inline_search_left(&grid, ViPoint::new(0, 4), 'z'), None);
    }

    // ---- Regression: scrollback boundary (#5612) ----

    /// Regression for #5612: point_backward must stop at -total_lines,
    /// not -(display_offset + total_lines). The display_offset controls
    /// which part of the buffer is *visible*, not the navigable extent.
    #[test]
    fn test_point_backward_scrollback_boundary_ignores_display_offset() {
        // 3 visible rows, 5 scrollback lines, display_offset=3
        let grid = MockGrid::new(3, 10)
            .with_scrollback(5)
            .with_display_offset(3);
        // Top should be -5 (total_lines), not -8 (display_offset + total_lines).
        // Navigate backward from the top-left corner of visible area.
        let result = point_backward(&grid, ViPoint::new(-5, 0));
        assert_eq!(result, None, "should stop at -total_lines, not go further");
    }

    /// Regression for #5612: paragraph_up uses the same formula as
    /// point_backward and must also stop at -total_lines.
    ///
    /// Starting from line -5 (exactly at the boundary) ensures the
    /// boundary clamp is exercised, not just empty-line detection.
    /// With the old formula `top = -(offset + total) = -8`, this would
    /// return ViPoint(-6, 0) — below the real scrollback boundary.
    #[test]
    fn test_paragraph_up_scrollback_boundary_ignores_display_offset() {
        let grid = MockGrid::new(3, 10)
            .with_scrollback(5)
            .with_display_offset(3);
        let result = paragraph_up(&grid, ViPoint::new(-5, 0));
        assert_eq!(
            result,
            ViPoint::new(-5, 0),
            "paragraph_up at boundary should clamp to -total_lines, not go to -8"
        );
    }

    /// Regression for #5612: point_backward should allow navigation up to
    /// -total_lines but not beyond, regardless of display_offset.
    #[test]
    fn test_point_backward_allows_navigation_to_scrollback() {
        let grid = MockGrid::new(3, 10)
            .with_scrollback(5)
            .with_display_offset(3);
        // Should be able to navigate backward from line -4 to -5.
        let result = point_backward(&grid, ViPoint::new(-4, 0));
        assert_eq!(
            result,
            Some(ViPoint::new(-5, 9)),
            "should navigate to last col of previous scrollback line"
        );
    }

    /// Regression for #5612: bracket_match into scrollback should use
    /// the correct boundary so it can find brackets in scrollback content.
    #[test]
    fn test_bracket_match_respects_scrollback_boundary() {
        // Use non-zero display_offset (#5618): with display_offset=0, the old
        // buggy boundary -(offset + total) equals the correct -(total), hiding
        // regressions. display_offset=3 separates the two formulas.
        let grid = MockGrid::new(2, 10)
            .with_scrollback(3)
            .with_display_offset(3)
            .with_line(0, "(hello    ")
            .with_line(1, "    world)");
        // Forward search: ( -> )
        let result = bracket_match(&grid, ViPoint::new(0, 0));
        assert_eq!(
            result,
            Some(ViPoint::new(1, 9)),
            "bracket match should find closing paren on visible row"
        );
        // Backward search: ) -> ( — exercises point_backward boundary
        let result = bracket_match(&grid, ViPoint::new(1, 9));
        assert_eq!(
            result,
            Some(ViPoint::new(0, 0)),
            "backward bracket match should find opening paren"
        );
    }

    // ---- Scrollback content traversal (one-shot / cached reads) ----

    /// `{` walking up from the visible region into scrollback must read
    /// each scrollback row once via `line_text` (is_line_empty fast path)
    /// and stop at the empty paragraph break that lives in scrollback.
    #[test]
    fn paragraph_up_finds_empty_line_in_scrollback() {
        // Scrollback laid out oldest→newest: "aaa", "", "bbb"
        //   line -3 = "aaa", line -2 = "" (empty), line -1 = "bbb".
        // Visible: line 0 = "ccc", line 1 = "ddd".
        let grid = MockGrid::new(2, 10)
            .with_scrollback_lines(&["aaa", "", "bbb"])
            .with_line(0, "ccc")
            .with_line(1, "ddd");
        let result = paragraph_up(&grid, ViPoint::new(0, 0));
        assert_eq!(
            result,
            ViPoint::new(-2, 0),
            "{{ should stop at the empty scrollback line"
        );
    }

    /// `%` matching a bracket whose partner lives in scrollback must scan
    /// the scrollback rows cell-by-cell through the one-motion line cache
    /// and resolve to the correct partner cell.
    #[test]
    fn bracket_match_scans_into_scrollback_content() {
        // Scrollback line -1 holds the opening bracket and its run;
        // visible line 0 holds the closing bracket.
        let grid = MockGrid::new(1, 10)
            .with_scrollback_lines(&["({abc}    "]) // line -1
            .with_line(0, ")         "); // line 0
        // Forward from the scrollback '(' at (-1, 0) to the visible ')'.
        let result = bracket_match(&grid, ViPoint::new(-1, 0));
        assert_eq!(
            result,
            Some(ViPoint::new(0, 0)),
            "forward bracket match should cross from scrollback into visible"
        );
        // Backward from the visible ')' at (0, 0) back to the scrollback '('.
        let result = bracket_match(&grid, ViPoint::new(0, 0));
        assert_eq!(
            result,
            Some(ViPoint::new(-1, 0)),
            "backward bracket match should find the opening paren in scrollback"
        );
    }
}
