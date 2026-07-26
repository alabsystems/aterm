// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Cmd-F FIND BAR: a single pinned chrome row painted over the TOP of the
//! terminal grid (directly below the tab strip) while find mode is active
//! (`WindowState.search`). Before this the find state was surfaced ONLY in the window
//! title + the current-match selection highlight (FIND-1 in
//! `docs/V1_READINESS_FINDINGS.md`), so pressing ⌘F looked like it did nothing — the
//! search engine was wired, but INVISIBLE. This makes it visible on glass: the live
//! query, a caret, the match position (`2/3`), the case/regex toggle state (`Aa` /
//! `.*`, brightened when active — and CLICKABLE, via the [`indicator_cols`] geometry
//! the splice records for the mouse path), and the key hints for the EMACS-ISEARCH
//! keymap (⌘S/^S and ⌘R/^R next/prev · ⌥⌘C case · ⌥⌘R regex · ⏎ accept · ⎋
//! cancel). A no-match
//! reads honestly — `no matches (partial history)` when scrollback ran deeper than the
//! search index cap, `bad regex` on a malformed pattern — never a bare, misleading
//! "no matches".
//!
//! Pure + themed like the config-notice bands (it reuses
//! [`crate::settings::blank_row`]/[`crate::settings::write_str`] +
//! [`crate::chrome_band::band_colors`]), so the row builder unit-tests with no window and
//! is drawn by `App::splice_find_bar` (app_render.rs), which OVERWRITES one grid row in
//! place — the top row normally, or the BOTTOM row when the current match would
//! otherwise sit under the top bar (adaptive placement, so the match is never hidden).

use std::ops::Range;

use aterm_core::terminal::RenderCell;
use aterm_render::Theme;

use crate::chrome_band::{self, BandColors};
use crate::settings::{blank_row, write_str};

/// The caret drawn immediately after the query — a thin vertical bar, so the bar
/// reads as an editable text field even though the terminal cursor stays on the shell.
const CARET: char = '▏';

/// Left indent of the `Find:` prompt (one blank cell, like the notice bands).
const LEFT_PAD: usize = 1;

/// The `Find: ` prompt drawn ahead of the live query. Its width fixes where the query
/// (and the caret after it) begins.
const PROMPT: &str = "Find: ";

/// The find state the bar paints — a value copy the splice takes under its disjoint
/// borrow, so the pure builder needs no `App`/window handle.
pub(crate) struct FindBarView {
    pub query: String,
    /// 1-based position of the current match (meaningful only when `total > 0`).
    pub idx: usize,
    pub total: usize,
    pub case_sensitive: bool,
    pub is_regex: bool,
    /// The query was an invalid regex (regex mode only) — shown as `bad regex`.
    pub regex_error: bool,
    /// Scrollback ran deeper than the search index cap (older history unsearched), so a
    /// zero-match reads `no matches (partial history)` rather than a definitive miss.
    pub truncated: bool,
}

/// One coloured run on the bar's right side.
struct Seg {
    text: String,
    fg: [u8; 3],
    bold: bool,
}

fn seg(text: impl Into<String>, fg: [u8; 3], bold: bool) -> Seg {
    Seg {
        text: text.into(),
        fg,
        bold,
    }
}

fn segs_width(segs: &[Seg]) -> usize {
    segs.iter().map(|s| s.text.chars().count()).sum()
}

/// Write `segs` left-to-right starting at column `col`.
fn write_segs(row: &mut [RenderCell], cols: usize, mut col: usize, segs: &[Seg], bg: [u8; 3]) {
    for s in segs {
        write_str(row, cols, col, &s.text, s.fg, bg, s.bold);
        col += s.text.chars().count();
    }
}

/// The status run: bad-regex / match position / (truncation-honest) no-match, or
/// `None` when the query is empty (bar just opened — nothing to report yet).
fn status_seg(v: &FindBarView, c: &BandColors) -> Option<Seg> {
    if v.query.is_empty() {
        return None;
    }
    if v.regex_error {
        return Some(seg("bad regex", c.warn, true));
    }
    if v.total == 0 {
        let text = if v.truncated {
            "no matches (partial history)"
        } else {
            "no matches"
        };
        return Some(seg(text, c.warn, false));
    }
    let suffix = if v.truncated { "+" } else { "" };
    Some(seg(
        format!("{}/{}{}", v.idx, v.total, suffix),
        c.value,
        true,
    ))
}

/// The right-side segment list: the `Aa` / `.*` toggle indicators (brightened when
/// active), the status, and — when `include_hint` — the key-hint tail (which teaches
/// the full emacs-isearch keymap while the query is still empty). The indicators
/// ALWAYS show, so the user can see the feature exists and its current state.
fn right_segs(v: &FindBarView, c: &BandColors, include_hint: bool) -> Vec<Seg> {
    let mut segs = vec![
        seg(
            "Aa",
            if v.case_sensitive { c.value } else { c.label },
            v.case_sensitive,
        ),
        seg(" ", c.label, false),
        seg(".*", if v.is_regex { c.value } else { c.label }, v.is_regex),
    ];
    if let Some(status) = status_seg(v, c) {
        segs.push(seg("   ", c.label, false));
        segs.push(status);
    }
    if include_hint {
        segs.push(seg("   ", c.label, false));
        // Teach the full keymap while the query is empty (there is room — no count);
        // once typing starts, drop to the compact nav hint (the indicators still show).
        let hint = if v.query.is_empty() {
            "⌘S/^S next  ⌘R/^R prev  ⌥⌘C case  ⌥⌘R regex  ⏎ accept  ⎋ cancel"
        } else {
            "⌘S/^S next  ⌘R/^R prev  ⏎ accept  ⎋ cancel"
        };
        segs.push(seg(hint, c.label, false));
    }
    segs
}

/// Build the ONE-row find bar, exactly `cols` cells wide (so the splice overwrites one
/// grid row in place). Left: `Find: {query}▏`. Right (right-aligned): the toggle
/// indicators + status + hints, tried at full width then compact (indicators+status),
/// and dropped entirely rather than colliding with the query/caret on a narrow window.
/// Pure + width-clamped: an over-long query or status is truncated by [`write_str`],
/// never panics.
///
/// `seam_at_top` places the thin separator rule on the edge that faces the terminal
/// content: an UNDERLINE (false) when the bar sits at the TOP (content below — the
/// normal placement), or an OVERLINE (true) when adaptive placement floats it to the
/// BOTTOM row (content above — see `App::splice_find_bar`, which flips placement so
/// the current match is never hidden). The rule rides the blank cells only, exactly
/// like the notice bands.
#[cfg(test)]
pub(crate) fn find_bar_row(
    v: &FindBarView,
    cols: usize,
    theme: Theme,
    seam_at_top: bool,
) -> Vec<RenderCell> {
    find_bar_row_with_indicators(v, cols, theme, seam_at_top).0
}

/// Build the row and return the two clickable indicator spans from the SAME
/// right-side placement pass. The compositor consumes this combined form so it
/// never allocates/walks the adaptive segment list twice per presented frame.
pub(crate) fn find_bar_row_with_indicators(
    v: &FindBarView,
    cols: usize,
    theme: Theme,
    seam_at_top: bool,
) -> (Vec<RenderCell>, Option<Range<usize>>, Option<Range<usize>>) {
    let c = chrome_band::band_colors(theme);
    // The seam marks the bar's content-facing edge: an overline (bar at the bottom) or,
    // when floated to the top, an underline on the bottom edge instead.
    let mut row = blank_row(cols, c.label, c.bar_bg, seam_at_top);
    if !seam_at_top {
        for cell in &mut row {
            cell.underline = aterm_core::terminal::UnderlineStyle::Single;
            cell.underline_color = Some(c.label);
        }
    }
    if cols == 0 {
        return (row, None, None);
    }
    // Left: `Find: ` prompt (dim label) + the live query (bright value) + caret.
    write_str(&mut row, cols, LEFT_PAD, PROMPT, c.label, c.bar_bg, true);
    let query_col = LEFT_PAD + PROMPT.chars().count();
    write_str(
        &mut row, cols, query_col, &v.query, c.value, c.bar_bg, false,
    );
    write_str(
        &mut row,
        cols,
        caret_col(v),
        CARET.encode_utf8(&mut [0u8; 4]),
        c.value,
        c.bar_bg,
        true,
    );

    // Right side (full-with-hints, else compact, else dropped) at its shared placement.
    let (case_cols, regex_cols) = if let Some((start, segs)) = right_side_placement(v, &c, cols) {
        write_segs(&mut row, cols, start, &segs, c.bar_bg);
        (Some(start..start + 2), Some(start + 3..start + 5))
    } else {
        (None, None)
    };
    (row, case_cols, regex_cols)
}

/// Column where the caret sits — after the `Find: ` prompt and the live query. Also the
/// clearance floor the right side must beat before it is drawn.
fn caret_col(v: &FindBarView) -> usize {
    LEFT_PAD + PROMPT.chars().count() + v.query.chars().count()
}

/// The right side actually drawn: `(start_col, segs)` for the widest variant (full with
/// hints, then compact indicators+status) that clears the caret+gap, or `None` when even
/// the compact set won't fit (narrow window). The SINGLE source of the right-side
/// geometry — both [`find_bar_row`] and [`indicator_cols`] call it, so the click
/// hit-test can never drift from the paint. Segment WIDTHS depend only on `v`, not the
/// theme colours, so the placement is colour-independent.
fn right_side_placement(v: &FindBarView, c: &BandColors, cols: usize) -> Option<(usize, Vec<Seg>)> {
    let caret = caret_col(v);
    for segs in [right_segs(v, c, true), right_segs(v, c, false)] {
        let w = segs_width(&segs);
        let start = cols.saturating_sub(w + 1);
        if w + 1 < cols && start > caret + 1 {
            return Some((start, segs));
        }
    }
    None
}

/// Cell-column spans of the clickable `Aa` (case) and `.*` (regex) indicators as
/// [`find_bar_row`] draws them for the same `v`/`cols`, or `(None, None)` when the right
/// side is dropped (window too narrow). [`right_segs`] always leads with
/// `[Aa, sep, .*]` (widths 2 / 1 / 2), so from the shared `start` the case span is
/// `start..start+2` and the regex span is `start+3..start+5`. Half-open, in cell columns;
/// `App::find_bar_hit` turns a click in these spans into a toggle without re-deriving the
/// layout. The theme is irrelevant to widths, so any is fine here.
#[cfg(test)]
pub(crate) fn indicator_cols(
    v: &FindBarView,
    cols: usize,
) -> (Option<Range<usize>>, Option<Range<usize>>) {
    match right_side_placement(v, &chrome_band::band_colors(Theme::default()), cols) {
        Some((start, _)) => (Some(start..start + 2), Some(start + 3..start + 5)),
        None => (None, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(query: &str) -> FindBarView {
        FindBarView {
            query: query.to_string(),
            idx: 1,
            total: 0,
            case_sensitive: false,
            is_regex: false,
            regex_error: false,
            truncated: false,
        }
    }

    fn text(row: &[RenderCell]) -> String {
        row.iter().map(|cell| cell.ch).collect()
    }

    /// Every row is exactly `cols` wide and carries the prompt + query + status across
    /// matched / no-match / empty.
    #[test]
    fn row_shape_and_content() {
        let cols = 90;
        let s = text(&find_bar_row(
            &FindBarView {
                idx: 2,
                total: 3,
                ..view("foo")
            },
            cols,
            Theme::default(),
            true,
        ));
        assert!(s.contains("Find: foo"), "{s}");
        assert!(s.contains("2/3"), "{s}");

        let s = text(&find_bar_row(
            &FindBarView {
                idx: 100_000,
                total: 100_000,
                truncated: true,
                ..view("e")
            },
            cols,
            Theme::default(),
            true,
        ));
        assert!(s.contains("100000/100000+"), "{s}");

        let s = text(&find_bar_row(&view("zzz"), cols, Theme::default(), true));
        assert!(s.contains("no matches"), "{s}");

        let s = text(&find_bar_row(&view(""), cols, Theme::default(), true));
        assert!(s.contains("Find: "), "{s}");
        assert!(s.contains("cancel"), "{s}");
    }

    /// The clickable indicator spans (`indicator_cols`) line up EXACTLY with the painted
    /// `Aa` / `.*` cells, so the mouse hit-test can never drift from the paint; a window
    /// too narrow to draw the right side yields no spans (nothing to click).
    #[test]
    fn indicator_cols_match_painted_cells() {
        let cols = 90;
        let v = view("q");
        let row = find_bar_row(&v, cols, Theme::default(), true);
        let (case, regex) = indicator_cols(&v, cols);
        let case = case.expect("Aa drawn at this width");
        let regex = regex.expect(".* drawn at this width");
        let at: String = row[case].iter().map(|c| c.ch).collect();
        assert_eq!(at, "Aa", "case span covers the painted Aa");
        let at: String = row[regex].iter().map(|c| c.ch).collect();
        assert_eq!(at, ".*", "regex span covers the painted .*");
        // A window too narrow for the right side has no clickable indicators.
        let (c2, r2) = indicator_cols(&view("q"), 10);
        assert!(c2.is_none() && r2.is_none(), "too narrow ⇒ no indicators");
    }

    /// The seam rides the content-facing edge: an OVERLINE for a bottom bar (content
    /// above), an UNDERLINE for a top-floated bar (content below), on the blank cells.
    #[test]
    fn seam_faces_the_content() {
        use aterm_core::terminal::UnderlineStyle;
        let cols = 90;
        let bottom = find_bar_row(&view("q"), cols, Theme::default(), true);
        assert!(
            bottom.iter().any(|c| c.overline),
            "bottom bar → overline seam"
        );
        assert!(
            bottom.iter().all(|c| c.underline == UnderlineStyle::None),
            "bottom bar → no underline"
        );
        let top = find_bar_row(&view("q"), cols, Theme::default(), false);
        assert!(
            top.iter().any(|c| c.underline == UnderlineStyle::Single),
            "top-floated bar → underline seam"
        );
        assert!(
            top.iter().all(|c| !c.overline),
            "top-floated bar → no overline"
        );
    }

    /// The toggle indicators `Aa` / `.*` always render; the empty-query bar teaches the
    /// full emacs-isearch keymap (nav chords + the ⌥⌘ toggles), and the typing bar keeps
    /// the compact nav tail.
    #[test]
    fn toggles_and_chord_hints() {
        let cols = 90;
        let s = text(&find_bar_row(&view("q"), cols, Theme::default(), true));
        assert!(s.contains("Aa"), "{s}");
        assert!(s.contains(".*"), "{s}");
        assert!(s.contains("⌘S/^S next"), "{s}");
        assert!(s.contains("⏎ accept"), "{s}");
        let s = text(&find_bar_row(&view(""), cols, Theme::default(), true));
        assert!(s.contains("⌘S/^S next"), "{s}");
        assert!(s.contains("⌘R/^R prev"), "{s}");
        assert!(s.contains("⌥⌘C case"), "{s}");
        assert!(s.contains("⌥⌘R regex"), "{s}");
        assert!(s.contains("⎋ cancel"), "{s}");
    }

    /// The active toggle brightens (uses the `value` tone), the inactive one is dim
    /// (`label`), so the state is visible, not just implied.
    #[test]
    fn active_toggle_uses_bright_tone() {
        let cols = 90;
        let c = chrome_band::band_colors(Theme::default());
        // The `A` cell of the `Aa` indicator — searched by cell (not byte offset: the
        // multi-byte caret before it would desync a String::find byte index).
        let fg_at = |row: &[RenderCell], ch: char| -> [u8; 3] {
            row[row.iter().position(|x| x.ch == ch).unwrap()].fg
        };
        let on = find_bar_row(
            &FindBarView {
                case_sensitive: true,
                ..view("q")
            },
            cols,
            Theme::default(),
            true,
        );
        assert_eq!(fg_at(&on, 'A'), c.value, "active case = bright value tone");
        let off = find_bar_row(&view("q"), cols, Theme::default(), true);
        assert_eq!(fg_at(&off, 'A'), c.label, "inactive case = dim label tone");
    }

    /// A truncated scan reports "no matches (partial history)"; a bad regex reports
    /// "bad regex".
    #[test]
    fn honest_no_match_states() {
        let cols = 100;
        let s = text(&find_bar_row(
            &FindBarView {
                truncated: true,
                ..view("needle")
            },
            cols,
            Theme::default(),
            true,
        ));
        assert!(s.contains("no matches (partial history)"), "{s}");

        let s = text(&find_bar_row(
            &FindBarView {
                is_regex: true,
                regex_error: true,
                ..view("(")
            },
            cols,
            Theme::default(),
            true,
        ));
        assert!(s.contains("bad regex"), "{s}");
    }

    /// A query far wider than the row truncates (no panic, exact width), and a very
    /// narrow row drops the right side rather than overwriting the query.
    #[test]
    fn narrow_and_overlong_never_panic() {
        for cols in [0usize, 1, 4, 8, 12, 20, 40] {
            let row = find_bar_row(
                &FindBarView {
                    idx: 5,
                    total: 9,
                    ..view(&"x".repeat(200))
                },
                cols,
                Theme::default(),
                true,
            );
            assert_eq!(row.len(), cols, "cols={cols}");
        }
        let cols = 14;
        let row = find_bar_row(
            &FindBarView {
                total: 2,
                ..view("abcd")
            },
            cols,
            Theme::default(),
            true,
        );
        assert_eq!(row.len(), cols);
        assert!(text(&row).contains("Find:"));
    }
}
