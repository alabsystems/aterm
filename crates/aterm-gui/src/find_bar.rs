// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Cmd-F FIND PANEL: a pinned chrome BAND painted over the terminal grid
//! (directly below the tab strip) while find mode is active (`WindowState.search`).
//! Before this the find state was surfaced ONLY in the window title + the
//! current-match selection highlight (the FIND-1 readiness finding), so
//! pressing ⌘F looked like it did nothing — the search engine was wired, but
//! INVISIBLE. Then it was one cramped row, which read as terminal output rather than
//! as UI. It is now a [`FIND_BAR_ROWS`]-row panel:
//!
//! ```text
//!                                                                  (pad)
//!   Find: needle                          2/7      [Aa] [.*]       (field)
//!   ⌘S next  ⌘R prev  ⌥⌘C case  ⌥⌘R regex  ⏎ accept  esc cancel     (hints)
//! ```
//!
//! The middle row carries a real INPUT WELL — a recessed run of cells in the
//! terminal's own background with the live query, a REVERSE-VIDEO block caret at the
//! edit position (the terminal's own cursor idiom, so the caret costs no cell and the
//! text never shifts as it moves), and a dim placeholder while empty. It reads — and
//! behaves — like a text field: never narrower than [`MIN_FIELD`] cells, never wider
//! than [`MAX_FIELD`], horizontally scrolled (with `‹`/`›` edge markers) to keep the
//! caret in view, and — the invariant that matters while typing — of a width that
//! depends on the WINDOW only, never on how wide the status text happens to be.
//!
//! The key hints move to their own row, so the field row is never squeezed by them.
//! The bracketed `[Aa]` / `[.*]` toggles reverse when active (visible across the
//! window, same width in both states) and are CLICKABLE via the [`FindBarPaint`]
//! geometry the splice records for the mouse path. The answer — `2/7`, `no matches`,
//! `bad regex` — outranks them: on a narrow window the toggles are dropped so the
//! readout survives, and a failed search additionally tints the QUERY ITSELF, because
//! that is where the eye already is. Truncation stays honest at every width, as the
//! full `no matches (partial history)` or the same `+` marker the count form uses.
//!
//! Pure + themed like the config-notice bands (it reuses
//! [`crate::settings::blank_row`]/[`crate::settings::write_str`] +
//! [`crate::chrome_band::band_colors`]), so the band builder unit-tests with no window
//! and is drawn by `App::splice_find_bar` (app_render.rs), which OVERWRITES its rows
//! in place — the top of the grid normally, or the BOTTOM when the current match
//! would otherwise sit under the panel (adaptive placement, so the match is never
//! hidden).

use std::ops::Range;

use aterm_core::terminal::RenderCell;
use aterm_render::Theme;

use crate::chrome_band::{self, BandColors};
use crate::settings::{blank_row, write_str};

/// Rows the find panel occupies: a blank pad, the field row, the hints row. The
/// splice shrinks the band on a terminal too short to hold it (see
/// [`find_bar_paint`]).
pub(crate) const FIND_BAR_ROWS: usize = 3;

/// Left indent of the `Find:` prompt (like the notice bands, but roomier — this is a
/// panel, not a status line).
const LEFT_PAD: usize = 2;

/// Right margin kept clear of the window edge.
const RIGHT_PAD: usize = 2;

/// Blank cells between the input well and the right-side run.
const GAP: usize = 3;

/// The `Find: ` prompt drawn ahead of the well. Its width fixes where the well begins.
const PROMPT: &str = "Find: ";

/// The input well never shrinks below this many cells: the right side is dropped
/// (indicators first, then the status) before the field is squeezed. Only a window
/// narrower than `LEFT_PAD + PROMPT + MIN_FIELD + RIGHT_PAD` gets a shorter field.
const MIN_FIELD: usize = 28;

/// …and never grows past this: a 100-cell trough of terminal-coloured void reads as a
/// hole, not a field. The surplus stays band, so the layout still breathes on a wide
/// window.
const MAX_FIELD: usize = 48;

/// `[Aa] [.*]` — both toggles, bracketed so they read as controls at a fixed width in
/// BOTH states (the ON state reverses the same 4 cells rather than resizing anything).
const IND_W: usize = 9;

/// Cells reserved for the status readout, as a function of the WINDOW width only —
/// never of the status text. That independence is the point: the well's geometry must
/// not shift under the caret when the count changes from `1/3` to `no matches` while
/// you are typing into it.
fn status_zone(cols: usize) -> usize {
    (cols / 6).clamp(6, 28)
}

/// Shown dimmed in an empty well — the same affordance a native search field has.
const PLACEHOLDER: &str = "search the screen + scrollback";

/// Drawn in the well's edge cell when the query is scrolled past it, so a clipped
/// query never masquerades as the whole query.
const SCROLL_LEFT: char = '‹';
const SCROLL_RIGHT: char = '›';

/// The find state the panel paints — a value copy the splice takes under its disjoint
/// borrow, so the pure builder needs no `App`/window handle.
pub(crate) struct FindBarView {
    pub query: String,
    /// Edit position as a BYTE offset into `query` (the field's caret), matching
    /// [`crate::app_search::SearchState::cursor`]. Clamped + floored to a char
    /// boundary here, so a desynced value can never panic the painter.
    pub cursor: usize,
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

impl FindBarView {
    /// The caret's position as a CHARACTER index into the query (0..=len). Clamped to
    /// the query and floored onto a char boundary: the painter must never panic on a
    /// cursor that raced ahead of the text it was captured with.
    fn cursor_chars(&self) -> usize {
        let mut byte = self.cursor.min(self.query.len());
        while byte > 0 && !self.query.is_char_boundary(byte) {
            byte -= 1;
        }
        self.query[..byte].chars().count()
    }
}

/// Where the painted panel put the things the mouse + the splice need to find again:
/// the rows themselves, which one is the editable field, the clickable toggle spans,
/// and the well geometry a click maps back into a caret position.
pub(crate) struct FindBarPaint {
    pub rows: Vec<Vec<RenderCell>>,
    /// Index within [`Self::rows`] of the row carrying the well + indicators.
    pub field_row: usize,
    /// Cell-column spans of the clickable `Aa` (case) / `.*` (regex) indicators, or
    /// `None` when the window was too narrow to draw them.
    pub case_cols: Option<Range<usize>>,
    pub regex_cols: Option<Range<usize>>,
    /// Columns of the input well on the field row.
    pub field_cols: Range<usize>,
    /// Character index of the query shown in the well's FIRST cell — the horizontal
    /// scroll offset. `field_cols.start + (i − field_scroll)` is where character `i`
    /// landed, so a click maps back to an edit position with no re-layout.
    pub field_scroll: usize,
}

/// One coloured run on the panel's right side. `bg` overrides the band background
/// (the reversed ON state of a toggle).
struct Seg {
    text: String,
    fg: [u8; 3],
    bg: Option<[u8; 3]>,
    bold: bool,
}

fn seg(text: impl Into<String>, fg: [u8; 3], bold: bool) -> Seg {
    Seg {
        text: text.into(),
        fg,
        bg: None,
        bold,
    }
}

/// Write `segs` left-to-right starting at column `col`.
fn write_segs(row: &mut [RenderCell], cols: usize, mut col: usize, segs: &[Seg], bg: [u8; 3]) {
    for s in segs {
        write_str(row, cols, col, &s.text, s.fg, s.bg.unwrap_or(bg), s.bold);
        col += s.text.chars().count();
    }
}

/// The status readout, widest form that fits `zone` cells first: bad-regex, match
/// position, or a (truncation-honest) no-match. `None` when the query is empty (panel
/// just opened — nothing to report yet) or when even the shortest form won't fit.
///
/// The ANSWER outranks the ornament: the caller drops the `Aa`/`.*` indicators before
/// it drops this, because a search that found nothing and one that found seven must
/// never look identical.
fn status_seg(v: &FindBarView, c: &BandColors, zone: usize) -> Option<Seg> {
    if v.query.is_empty() {
        return None;
    }
    let plus = if v.truncated { "+" } else { "" };
    let (candidates, fg, bold) = if v.regex_error {
        (
            vec!["bad regex".to_string(), "re!".to_string()],
            c.warn,
            true,
        )
    } else if v.total == 0 {
        // `+` is the same "…and history was deeper than the index" marker the count
        // form uses, so the honest qualifier survives at every width.
        (
            vec![
                format!(
                    "no matches{}",
                    if v.truncated {
                        " (partial history)"
                    } else {
                        ""
                    }
                ),
                format!("no matches{plus}"),
                format!("0 hits{plus}"),
            ],
            c.warn,
            false,
        )
    } else {
        (
            vec![
                format!("{}/{}{}", v.idx, v.total, plus),
                format!("{}/{}", v.idx, v.total),
                format!("{}{plus}", v.total),
            ],
            c.value,
            true,
        )
    };
    candidates
        .into_iter()
        .find(|text| text.chars().count() <= zone)
        .map(|text| seg(text, fg, bold))
}

/// The `[Aa]` / `[.*]` toggle indicators. Bracketed so they read as CONTROLS, and
/// REVERSED (ink and ground swapped) when active — a state you can see across the
/// window, at a width that never changes as it flips, so nothing reflows on a toggle.
fn indicator_segs(v: &FindBarView, c: &BandColors) -> Vec<Seg> {
    let toggle = |text: &str, on: bool| {
        let mut s = seg(text, if on { c.bar_bg } else { c.label }, on);
        s.bg = on.then_some(c.value);
        s
    };
    vec![
        toggle("[Aa]", v.case_sensitive),
        seg(" ", c.label, false),
        toggle("[.*]", v.is_regex),
    ]
}

/// The keymap taught on the hints row, widest variant that fits first.
///
/// `esc` is spelled out: U+238B (⎋) is a font-fallback lottery — it renders as blank in
/// one monospace face and as a reload-looking circular arrow in another, which is worse
/// than useless for the one key that gets you out.
///
/// PER PLATFORM, because a hint that names keys the keyboard does not have is
/// worse than no hint — aterm's own ratified rule (`palette.rs` blanks its
/// accelerator column off macOS because a ⌘ glyph "would MISLEAD", and `cli.rs`
/// splits its KEYS help the same way). Off macOS the same commands answer to
/// Ctrl+S / Ctrl+R (the emacs arms in `on_key_search_mode`, which always worked
/// but were never taught) and the toggles to Alt+C / Alt+R (the VS Code find
/// widget's chords; the seeded ⌥⌘ spelling is Win+Alt on Windows, where
/// Win+Alt+R belongs to the Xbox Game Bar).
fn hint_text(cols: usize) -> &'static str {
    #[cfg(target_os = "macos")]
    const FULL: &str = "⌘S next  ⌘R prev  ⌥⌘C case  ⌥⌘R regex  ⏎ accept  esc cancel";
    #[cfg(target_os = "macos")]
    const SHORT: &str = "⌘S/⌘R next/prev  ⏎ accept  esc cancel";
    #[cfg(not(target_os = "macos"))]
    const FULL: &str = "Ctrl+S next  Ctrl+R prev  Alt+C case  Alt+R regex  ⏎ accept  esc cancel";
    #[cfg(not(target_os = "macos"))]
    const SHORT: &str = "Ctrl+S/R next/prev  ⏎ accept  esc cancel";
    const TINY: &str = "⏎ accept  esc cancel";
    let room = cols.saturating_sub(LEFT_PAD + RIGHT_PAD);
    for hint in [FULL, SHORT] {
        if hint.chars().count() <= room {
            return hint;
        }
    }
    TINY
}

/// Build the find panel: `height` rows, each exactly `cols` cells wide (so the splice
/// overwrites whole grid rows in place). `height` is normally [`FIND_BAR_ROWS`]; a
/// terminal too short degrades gracefully — 2 rows drop the pad, 1 row keeps only the
/// field. Pure + width-clamped: an over-long query, status or hint is truncated, never
/// panics, and every returned row is exactly `cols` long.
///
/// `seam_at_top` places the thin separator rule on the edge that faces the terminal
/// content: an UNDERLINE under the LAST row (false) when the panel sits at the TOP
/// (content below — the normal placement), or an OVERLINE on the FIRST row (true) when
/// adaptive placement floats it to the BOTTOM (content above — see
/// `App::splice_find_bar`, which flips placement so the current match is never hidden).
/// The rule is applied AFTER the text, so it runs edge to edge instead of breaking into
/// stubs wherever a glyph landed.
pub(crate) fn find_bar_paint(
    v: &FindBarView,
    cols: usize,
    height: usize,
    theme: Theme,
    seam_at_top: bool,
) -> FindBarPaint {
    let c = chrome_band::band_colors(theme);
    let height = height.max(1);
    // 3 rows: pad / field / hints. 2: field / hints. 1: field only — the pad is the
    // first thing to go, the hints the second, the field never.
    let field_row = usize::from(height >= FIND_BAR_ROWS);
    let hint_row = (height >= 2).then_some(height - 1);
    let mut rows: Vec<Vec<RenderCell>> = (0..height)
        .map(|_| blank_row(cols, c.label, c.bar_bg, false))
        .collect();
    if cols == 0 {
        return FindBarPaint {
            rows,
            field_row,
            case_cols: None,
            regex_cols: None,
            field_cols: 0..0,
            field_scroll: 0,
        };
    }

    // ---- field row: `Find: ` + the well, then the right-aligned status/indicators.
    let well_start = (LEFT_PAD + PROMPT.chars().count()).min(cols);
    let layout = field_layout(v, &c, cols, well_start);
    let field_cols = layout.well.clone();
    let field_scroll = {
        let row = &mut rows[field_row];
        write_str(row, cols, LEFT_PAD, PROMPT, c.value, c.bar_bg, true);
        let scroll = paint_well(row, v, &c, field_cols.clone());
        for (start, segs) in [layout.indicators.as_ref(), layout.status.as_ref()]
            .into_iter()
            .flatten()
        {
            write_segs(row, cols, *start, segs, c.bar_bg);
        }
        scroll
    };
    // `[Aa]` and `[.*]` are 4 cells each, separated by one — the whole bracketed group
    // is the click target, so a pointer aimed at the bracket is not a dead pixel.
    let (case_cols, regex_cols) = match layout.indicators.as_ref() {
        Some((start, _)) => (Some(*start..start + 4), Some(start + 5..start + 9)),
        None => (None, None),
    };

    // ---- hints row: the keymap, dim, on its own line.
    if let Some(hint_row) = hint_row {
        let hint = hint_text(cols);
        write_str(
            &mut rows[hint_row],
            cols,
            LEFT_PAD,
            hint,
            c.label,
            c.bar_bg,
            false,
        );
    }

    // ---- seam LAST: one unbroken rule on the content-facing edge, over text and all.
    let seam_row = if seam_at_top { 0 } else { height - 1 };
    for cell in &mut rows[seam_row] {
        if seam_at_top {
            cell.overline = true;
        } else {
            cell.underline = aterm_core::terminal::UnderlineStyle::Single;
            cell.underline_color = Some(c.label);
        }
    }

    FindBarPaint {
        rows,
        field_row,
        case_cols,
        regex_cols,
        field_cols,
        field_scroll,
    }
}

/// Paint the input WELL: a recessed run of cells (the terminal's own background)
/// holding the visible slice of the query, with a REVERSE-VIDEO block caret on the
/// character at the edit position — the terminal's own cursor idiom, so the caret costs
/// no cell and the text never shifts when it moves. Returns the horizontal scroll (the
/// character index shown in the well's first cell).
///
/// The view scrolls the minimum needed to keep the caret inside, preferring to show as
/// much trailing text as fits — so typing past the right edge scrolls the text while
/// the caret stays put, and ^A jumps the view back to the head, exactly like a native
/// single-line field. `‹`/`›` mark an edge the query continues past.
///
/// A search that FOUND NOTHING tints the query itself with the warn tone: the answer
/// belongs where the eye already is, not only in a readout at the far side of the row.
fn paint_well(
    row: &mut [RenderCell],
    v: &FindBarView,
    c: &BandColors,
    field: Range<usize>,
) -> usize {
    let width = field.len();
    if width == 0 {
        return 0;
    }
    for cell in &mut row[field.clone()] {
        *cell = chrome_band::cell(' ', c.value, c.field_bg, false, false);
    }
    let chars: Vec<char> = v.query.chars().collect();
    let cursor = v.cursor_chars().min(chars.len());
    let ink = if !chars.is_empty() && (v.regex_error || v.total == 0) {
        c.warn
    } else {
        c.value
    };
    // The caret can sit one past the last character, so the scrollable extent is
    // `len + 1` even though only `len` cells carry a glyph.
    let max_start = (chars.len() + 1).saturating_sub(width);
    let scroll = cursor.saturating_sub(width - 1).min(max_start);
    for i in 0..width {
        let index = scroll + i;
        let col = field.start + i;
        let ch = chars.get(index).copied().unwrap_or(' ');
        row[col] = if index == cursor {
            // Reverse video: the well's background becomes the ink.
            chrome_band::cell(ch, c.field_bg, c.caret, false, false)
        } else {
            chrome_band::cell(ch, ink, c.field_bg, false, false)
        };
        if index > chars.len() {
            break;
        }
    }
    // An empty field says what it searches, dimmed, after the caret — the native
    // search-field affordance, and the honest answer to "does this search history?".
    if chars.is_empty() && width >= PLACEHOLDER.chars().count() + 2 {
        write_str(
            row,
            field.end,
            field.start + 2,
            PLACEHOLDER,
            c.label,
            c.field_bg,
            false,
        );
    }
    // Clipped-edge markers, never over the caret (the caret's cell is the one place the
    // user is looking, and it already proves where the edit position is).
    if scroll > 0 && cursor != scroll {
        row[field.start] = chrome_band::cell(SCROLL_LEFT, c.label, c.field_bg, false, false);
    }
    let last = scroll + width - 1;
    if last < chars.len() && cursor != last {
        row[field.end - 1] = chrome_band::cell(SCROLL_RIGHT, c.label, c.field_bg, false, false);
    }
    // BORDER, when the fill cannot carry the boundary. Stamped LAST so it covers every
    // cell the passes above wrote — text, caret, placeholder and edge markers alike —
    // and applied only where `band_colors` says the well and the band share a tone
    // (every Windows High-Contrast scheme; no theme-derived one). Without it an HC
    // user sees an editable field with no edge at all: the fill is the only thing this
    // well ever drew to say "you can type here". See [`BandColors::well_rule`].
    if let Some(rule) = c.well_rule {
        for cell in &mut row[field] {
            cell.underline = aterm_core::terminal::UnderlineStyle::Single;
            cell.underline_color = Some(rule);
        }
    }
    scroll
}

/// Where the field row's three pieces land. The WELL's geometry is a function of `cols`
/// alone — the status is right-aligned inside a reserved zone whose width comes from
/// [`status_zone`], never from the status text — so the field cannot resize under the
/// caret as the match count changes while you type. The SINGLE source of the row's
/// geometry, so the click hit-test can never drift from the paint.
struct FieldLayout {
    well: Range<usize>,
    /// `(start_col, segs)` of the `[Aa] [.*]` run, when it fits.
    indicators: Option<(usize, Vec<Seg>)>,
    /// `(start_col, segs)` of the status readout, when there is one and it fits.
    status: Option<(usize, Vec<Seg>)>,
}

fn field_layout(v: &FindBarView, c: &BandColors, cols: usize, well_start: usize) -> FieldLayout {
    let zone = status_zone(cols);
    let room = cols.saturating_sub(well_start + RIGHT_PAD);
    // Widest reserve the window can afford while the well keeps MIN_FIELD cells. The
    // ANSWER (status) outranks the toggles: the indicators go first.
    let (reserve, with_indicators) = if room >= MIN_FIELD + GAP + zone + 1 + IND_W {
        (GAP + zone + 1 + IND_W, true)
    } else if room >= MIN_FIELD + GAP + zone {
        (GAP + zone, false)
    } else if room >= MIN_FIELD + GAP + IND_W {
        (GAP + IND_W, true)
    } else {
        (0, false)
    };
    let well_end = cols
        .saturating_sub(RIGHT_PAD + reserve)
        .min(well_start + MAX_FIELD)
        .max(well_start);
    let mut layout = FieldLayout {
        well: well_start..well_end,
        indicators: None,
        status: None,
    };
    if reserve == 0 {
        return layout;
    }
    let right_edge = cols.saturating_sub(RIGHT_PAD);
    let indicator_start = right_edge.saturating_sub(IND_W);
    if with_indicators {
        layout.indicators = Some((indicator_start, indicator_segs(v, c)));
    }
    // The status sits in the `zone` cells left of the indicators (or of the right
    // margin when they were dropped), right-aligned so the readout hugs one fixed edge.
    let status_right = if with_indicators {
        indicator_start.saturating_sub(1)
    } else {
        right_edge
    };
    if (reserve > GAP + IND_W || !with_indicators)
        && let Some(status) = status_seg(v, c, zone)
    {
        let start = status_right.saturating_sub(status.text.chars().count());
        if start >= layout.well.end {
            layout.status = Some((start, vec![status]));
        }
    }
    layout
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(query: &str) -> FindBarView {
        FindBarView {
            query: query.to_string(),
            cursor: query.len(),
            idx: 1,
            total: 0,
            case_sensitive: false,
            is_regex: false,
            regex_error: false,
            truncated: false,
        }
    }

    fn paint(v: &FindBarView, cols: usize) -> FindBarPaint {
        find_bar_paint(v, cols, FIND_BAR_ROWS, Theme::default(), true)
    }

    fn text(row: &[RenderCell]) -> String {
        row.iter().map(|cell| cell.ch).collect()
    }

    /// The whole panel as one string (rows joined) — for content assertions that don't
    /// care which row a run landed on.
    fn all(p: &FindBarPaint) -> String {
        p.rows
            .iter()
            .map(|r| text(r))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The well's contents as a string, and the column the block caret landed on.
    fn well(p: &FindBarPaint) -> (String, usize) {
        let row = &p.rows[p.field_row];
        let c = chrome_band::band_colors(Theme::default());
        let caret = p
            .field_cols
            .clone()
            .find(|&col| row[col].bg == c.caret)
            .expect("the caret is always inside the well");
        (
            row[p.field_cols.clone()].iter().map(|c| c.ch).collect(),
            caret,
        )
    }

    /// Every row is exactly `cols` wide, the panel is [`FIND_BAR_ROWS`] tall, and it
    /// carries the prompt + query + status across matched / no-match / empty.
    #[test]
    fn shape_and_content() {
        let cols = 90;
        let p = paint(
            &FindBarView {
                idx: 2,
                total: 3,
                ..view("foo")
            },
            cols,
        );
        assert_eq!(p.rows.len(), FIND_BAR_ROWS);
        assert!(p.rows.iter().all(|r| r.len() == cols));
        let s = all(&p);
        assert!(s.contains("Find: foo"), "{s}");
        assert!(s.contains("2/3"), "{s}");

        let s = all(&paint(
            &FindBarView {
                idx: 12,
                total: 34,
                truncated: true,
                ..view("e")
            },
            cols,
        ));
        assert!(s.contains("12/34+"), "{s}");

        let s = all(&paint(&view("zzz"), cols));
        assert!(s.contains("no matches"), "{s}");

        // Empty: the placeholder says what find searches, and the hints teach the map.
        let s = all(&paint(&view(""), cols));
        assert!(s.contains(PLACEHOLDER), "{s}");
        assert!(s.contains("cancel"), "{s}");
    }

    /// The query lives in a WELL (the terminal's own bg) at least [`MIN_FIELD`] cells
    /// wide, and the hints sit on their own row — never squeezing the field.
    #[test]
    fn well_is_a_real_field() {
        let cols = 90;
        let p = paint(&view("needle"), cols);
        assert!(
            p.field_cols.len() >= MIN_FIELD,
            "well {:?} must keep {MIN_FIELD} cells",
            p.field_cols
        );
        let field = &p.rows[p.field_row];
        let c = chrome_band::band_colors(Theme::default());
        assert!(
            p.field_cols
                .clone()
                .all(|col| field[col].bg == c.field_bg || field[col].bg == c.caret),
            "the whole well is painted in the well background (bar the caret)"
        );
        assert_ne!(c.field_bg, c.bar_bg, "the well must read as recessed");
        // Hints are on their own row, below the field.
        assert!(text(&p.rows[FIND_BAR_ROWS - 1]).contains("accept"));
        assert!(!text(field).contains("accept"));
    }

    /// THE FIELD MUST NOT MOVE UNDER THE CARET. The well's geometry depends on the
    /// WINDOW width only — never on how wide the status text happens to be — so typing
    /// the first character (which turns "nothing to report" into `1/3` or `no matches`)
    /// cannot yank the field's right edge sideways mid-edit.
    #[test]
    fn well_geometry_is_independent_of_the_status_text() {
        for cols in [60usize, 80, 100, 132, 200] {
            let empty = paint(&view(""), cols).field_cols;
            for v in [
                FindBarView {
                    idx: 1,
                    total: 3,
                    ..view("q")
                },
                FindBarView {
                    idx: 100_000,
                    total: 100_000,
                    truncated: true,
                    ..view("q")
                },
                view("zzz"), // no matches
                FindBarView {
                    truncated: true,
                    ..view("zzz")
                },
                FindBarView {
                    is_regex: true,
                    regex_error: true,
                    ..view("(")
                },
                FindBarView {
                    case_sensitive: true,
                    is_regex: true,
                    ..view("q")
                },
            ] {
                assert_eq!(
                    paint(&v, cols).field_cols,
                    empty,
                    "cols={cols}: the well moved with the status/toggle state"
                );
            }
        }
    }

    /// The caret is a REVERSE-VIDEO block on the character at the edit position — it
    /// costs no cell, so the text never shifts as it moves (the phantom-space bug).
    #[test]
    fn caret_is_a_block_that_does_not_move_the_text() {
        let cols = 90;
        let (text_at_end, caret_end) = well(&paint(&view("abc"), cols));
        let (text_at_start, caret_start) = well(&paint(
            &FindBarView {
                cursor: 0,
                ..view("abc")
            },
            cols,
        ));
        let (text_mid, caret_mid) = well(&paint(
            &FindBarView {
                cursor: 1,
                ..view("abc")
            },
            cols,
        ));
        assert_eq!(text_at_end, text_at_start, "the text never reflows");
        assert_eq!(text_at_end, text_mid);
        assert!(text_at_end.starts_with("abc"), "{text_at_end:?}");
        let field_start = paint(&view("abc"), cols).field_cols.start;
        assert_eq!(caret_start - field_start, 0, "caret on `a`");
        assert_eq!(caret_mid - field_start, 1, "caret on `b`");
        assert_eq!(caret_end - field_start, 3, "caret past the last character");
        // Reverse video: the well's background becomes the ink.
        let c = chrome_band::band_colors(Theme::default());
        let p = paint(
            &FindBarView {
                cursor: 1,
                ..view("abc")
            },
            cols,
        );
        let cell = &p.rows[p.field_row][caret_mid];
        assert_eq!((cell.ch, cell.bg, cell.fg), ('b', c.caret, c.field_bg));
    }

    /// A query longer than the well scrolls horizontally so the caret stays visible —
    /// at the end (tail shown), at the start (head shown), and in between — and the
    /// clipped edges are MARKED, so a truncated query never reads as the whole query.
    #[test]
    fn overlong_query_scrolls_to_the_caret() {
        let cols = 90;
        let long: String = ('a'..='z').cycle().take(400).collect();
        let end = paint(&view(&long), cols);
        let width = end.field_cols.len();
        assert_eq!(
            end.field_scroll,
            long.chars().count() + 1 - width,
            "caret at the end pins the well to the tail"
        );
        let (shown, _) = well(&end);
        assert!(
            shown.starts_with(SCROLL_LEFT),
            "clipped head marked: {shown}"
        );

        let head = paint(
            &FindBarView {
                cursor: 0,
                ..view(&long)
            },
            cols,
        );
        assert_eq!(head.field_scroll, 0, "^A scrolls the well back to the head");
        let (shown, caret) = well(&head);
        assert_eq!(caret, head.field_cols.start);
        assert!(
            shown.ends_with(SCROLL_RIGHT),
            "clipped tail marked: {shown}"
        );

        let mid = paint(
            &FindBarView {
                cursor: 200,
                ..view(&long)
            },
            cols,
        );
        let (_, caret) = well(&mid);
        assert!(
            mid.field_cols.contains(&caret),
            "the caret is inside the well at every edit position"
        );
    }

    /// The clickable indicator spans line up EXACTLY with the painted `[Aa]` / `[.*]`
    /// cells (brackets included — a click on the bracket is not a dead pixel).
    #[test]
    fn indicator_cols_match_painted_cells() {
        let cols = 90;
        let p = paint(&view("q"), cols);
        let row = &p.rows[p.field_row];
        let case = p.case_cols.clone().expect("[Aa] drawn at this width");
        let regex = p.regex_cols.clone().expect("[.*] drawn at this width");
        let at: String = row[case].iter().map(|c| c.ch).collect();
        assert_eq!(at, "[Aa]", "case span covers the painted toggle");
        let at: String = row[regex].iter().map(|c| c.ch).collect();
        assert_eq!(at, "[.*]", "regex span covers the painted toggle");
    }

    /// NARROW WINDOWS KEEP THE ANSWER. When the row cannot hold everything, the
    /// ornament goes first: the toggles are dropped so the match/no-match readout
    /// survives — a search that found nothing must never look like one that found
    /// seven.
    #[test]
    fn the_status_outranks_the_toggles_when_space_runs_out() {
        let narrow = paint(&view("zzz"), 52);
        assert!(
            narrow.case_cols.is_none() && narrow.regex_cols.is_none(),
            "the toggles are dropped first"
        );
        let s = all(&narrow);
        assert!(
            s.contains("no matches") || s.contains("0 hits"),
            "the answer survives, in whatever form fits: {s}"
        );
        assert!(
            narrow.field_cols.len() >= MIN_FIELD,
            "and so does the field"
        );
        // Wider: both fit.
        let roomy = paint(&view("zzz"), 90);
        assert!(roomy.case_cols.is_some());
        assert!(all(&roomy).contains("no matches"));
        // Narrower still: the field keeps whatever is left rather than vanishing.
        let tiny = paint(&view("q"), 24);
        assert!(!tiny.field_cols.is_empty());
    }

    /// A search that found NOTHING says so where the eye already is — the query itself
    /// takes the warn tone — not only in a readout at the far side of the row.
    #[test]
    fn a_failed_search_colours_the_query_itself() {
        let cols = 90;
        let c = chrome_band::band_colors(Theme::default());
        let miss = paint(&view("zzz"), cols);
        let row = &miss.rows[miss.field_row];
        assert_eq!(row[miss.field_cols.start].fg, c.warn, "no match ⇒ warn ink");
        let hit = paint(
            &FindBarView {
                idx: 1,
                total: 2,
                ..view("zzz")
            },
            cols,
        );
        let row = &hit.rows[hit.field_row];
        assert_eq!(row[hit.field_cols.start].fg, c.value, "matched ⇒ value ink");
        let bad = paint(
            &FindBarView {
                is_regex: true,
                regex_error: true,
                ..view("(")
            },
            cols,
        );
        let row = &bad.rows[bad.field_row];
        assert_eq!(row[bad.field_cols.start].fg, c.warn, "bad regex ⇒ warn ink");
    }

    /// The seam rides the content-facing edge of the BAND and runs EDGE TO EDGE —
    /// applied after the text, so it is one rule rather than stubs around the glyphs.
    #[test]
    fn seam_faces_the_content_and_is_unbroken() {
        use aterm_core::terminal::UnderlineStyle;
        let cols = 90;
        let bottom = find_bar_paint(&view("q"), cols, FIND_BAR_ROWS, Theme::default(), true);
        assert!(
            bottom.rows[0].iter().all(|c| c.overline),
            "bottom panel → an unbroken overline on its top edge"
        );
        assert!(
            bottom
                .rows
                .iter()
                .flatten()
                .all(|c| c.underline == UnderlineStyle::None),
            "bottom panel → no underline"
        );
        let top = find_bar_paint(&view("q"), cols, FIND_BAR_ROWS, Theme::default(), false);
        assert!(
            top.rows[FIND_BAR_ROWS - 1]
                .iter()
                .all(|c| c.underline == UnderlineStyle::Single),
            "top panel → an unbroken underline on its bottom edge, through the hints"
        );
        assert!(
            top.rows.iter().flatten().all(|c| !c.overline),
            "top panel → no overline"
        );
    }

    /// THE QUERY FIELD ALWAYS HAS AN EDGE. The well is drawn as a FILL — the only
    /// thing that says "you can type here" — and under every stock Windows
    /// High-Contrast scheme `COLOR_WINDOW == COLOR_BTNFACE`, so that fill is the same
    /// tone as the band and the field disappears. HC separates surfaces with borders,
    /// so a border is stamped instead: unbroken across the whole well, over the text,
    /// the caret and the edge markers alike.
    #[test]
    fn the_query_well_keeps_a_boundary_under_a_forced_palette() {
        use aterm_core::terminal::UnderlineStyle;
        let cols = 90;
        // Off an OS palette the fill carries the edge and nothing is stamped.
        let plain = paint(&view("abc"), cols);
        assert!(
            plain.rows[plain.field_row][plain.field_cols.clone()]
                .iter()
                .all(|c| c.underline == UnderlineStyle::None),
            "a theme-derived well is an inset already: no border"
        );

        for (name, palette) in chrome_band::hc_fixtures::STOCK {
            chrome_band::hc_fixtures::with_forced(palette, || {
                let c = chrome_band::band_colors(Theme::default());
                assert_eq!(
                    c.field_bg, c.bar_bg,
                    "{name}: the fixture is only interesting because the tones collapse"
                );
                let p = paint(&view("abc"), cols);
                let well = &p.rows[p.field_row][p.field_cols.clone()];
                assert!(
                    well.iter()
                        .all(|cell| cell.underline == UnderlineStyle::Single
                            && cell.underline_color == c.well_rule),
                    "{name}: the well must carry an unbroken border, caret cell included"
                );
                // And it must stop AT the well: the band around it is not a field.
                assert_eq!(
                    p.rows[p.field_row][p.field_cols.start - 1].underline,
                    UnderlineStyle::None,
                    "{name}: the border marks the field, not the whole row"
                );
            });
        }
    }

    /// The hints row teaches the emacs-isearch keymap in words that actually RENDER:
    /// `esc` is spelled out because U+238B is a font-fallback lottery.
    #[test]
    fn chord_hints_use_glyphs_that_render() {
        let cols = 100;
        let s = all(&paint(&view("q"), cols));
        // PER PLATFORM, like `hint_text` itself: ⌘/⌥ glyphs exist only on a Mac
        // keyboard — a Windows find bar must never teach ⌥⌘R, which there means
        // Win+Alt+R, the Xbox Game Bar chord.
        #[cfg(target_os = "macos")]
        {
            assert!(s.contains("⌘S next"), "{s}");
            assert!(s.contains("⌘R prev"), "{s}");
            assert!(s.contains("⌥⌘C case"), "{s}");
            assert!(s.contains("⌥⌘R regex"), "{s}");
        }
        #[cfg(not(target_os = "macos"))]
        {
            assert!(s.contains("Ctrl+S next"), "{s}");
            assert!(s.contains("Ctrl+R prev"), "{s}");
            assert!(s.contains("Alt+C case"), "{s}");
            assert!(s.contains("Alt+R regex"), "{s}");
            assert!(!s.contains('⌘'), "no command glyph off macOS: {s}");
            assert!(!s.contains('⌥'), "no option glyph off macOS: {s}");
        }
        assert!(s.contains("⏎ accept"), "{s}");
        assert!(s.contains("esc cancel"), "{s}");
        assert!(
            !s.contains('⎋'),
            "the unrenderable escape glyph is gone: {s}"
        );
    }

    /// An ACTIVE toggle reverses (ink and ground swap) at the SAME width, so its state
    /// is visible across the window and nothing reflows when it flips.
    #[test]
    fn active_toggle_reverses_at_a_fixed_width() {
        let cols = 90;
        let c = chrome_band::band_colors(Theme::default());
        let cell_at = |p: &FindBarPaint, span: Range<usize>| {
            let row = &p.rows[p.field_row];
            (row[span.start + 1].fg, row[span.start + 1].bg)
        };
        let off = paint(&view("q"), cols);
        let on = paint(
            &FindBarView {
                case_sensitive: true,
                ..view("q")
            },
            cols,
        );
        assert_eq!(
            off.case_cols, on.case_cols,
            "the toggle's geometry never changes with its state"
        );
        assert_eq!(
            cell_at(&off, off.case_cols.clone().unwrap()),
            (c.label, c.bar_bg),
            "inactive = dim on the band"
        );
        assert_eq!(
            cell_at(&on, on.case_cols.clone().unwrap()),
            (c.bar_bg, c.value),
            "active = reversed"
        );
    }

    /// A truncated scan stays honest at every width: the full phrase when the status
    /// zone is wide enough, the same `+` marker the count form uses when it is not.
    #[test]
    fn honest_no_match_states() {
        let wide = all(&paint(
            &FindBarView {
                truncated: true,
                ..view("needle")
            },
            180,
        ));
        assert!(wide.contains("no matches (partial history)"), "{wide}");
        let narrow = all(&paint(
            &FindBarView {
                truncated: true,
                ..view("needle")
            },
            90,
        ));
        assert!(narrow.contains("no matches+"), "{narrow}");
        let s = all(&paint(
            &FindBarView {
                is_regex: true,
                regex_error: true,
                ..view("(")
            },
            90,
        ));
        assert!(s.contains("bad regex"), "{s}");
    }

    /// Every width × height stays exactly `cols` wide and never panics — including a
    /// query far wider than the row, a zero-width window, and the degraded 1/2-row
    /// bands a very short terminal gets.
    #[test]
    fn narrow_short_and_overlong_never_panic() {
        for cols in [0usize, 1, 4, 8, 12, 20, 40, 200] {
            for height in [1usize, 2, 3] {
                for cursor in [0usize, 7, 200] {
                    let p = find_bar_paint(
                        &FindBarView {
                            cursor,
                            idx: 5,
                            total: 9,
                            ..view(&"x".repeat(200))
                        },
                        cols,
                        height,
                        Theme::default(),
                        true,
                    );
                    assert_eq!(p.rows.len(), height, "cols={cols} height={height}");
                    assert!(p.rows.iter().all(|r| r.len() == cols));
                    assert!(p.field_row < height);
                    assert!(p.field_cols.end <= cols.max(p.field_cols.start));
                }
            }
        }
        // A degraded 1-row band keeps the FIELD (the pad and hints go first).
        let one = find_bar_paint(&view("kept"), 90, 1, Theme::default(), true);
        assert_eq!(one.field_row, 0);
        assert!(text(&one.rows[0]).contains("Find: "));
        assert!(text(&one.rows[0]).contains("kept"));
    }

    /// A multi-byte query is painted (and scrolled) by CHARACTER, and a cursor that
    /// lands mid-codepoint is floored rather than panicking.
    #[test]
    fn multibyte_query_is_char_indexed() {
        let cols = 90;
        let p = paint(
            &FindBarView {
                cursor: "é".len(),
                ..view("éßπ")
            },
            cols,
        );
        let (shown, caret) = well(&p);
        assert!(shown.starts_with("éßπ"), "{shown}");
        assert_eq!(caret - p.field_cols.start, 1, "the caret is on ß");
        // Mid-codepoint cursor: floored to the boundary before it, never a panic.
        let p = paint(
            &FindBarView {
                cursor: 1,
                ..view("éßπ")
            },
            cols,
        );
        let (_, caret) = well(&p);
        assert_eq!(caret - p.field_cols.start, 0, "floored back onto é");
    }
}
