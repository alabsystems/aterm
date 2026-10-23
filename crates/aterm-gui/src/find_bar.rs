// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Cmd-F FIND PANEL.

use std::ops::Range;
use std::sync::Arc;

use aterm_core::grid::extra::{ImageData, ImageFormat, ImageScaling};
use aterm_core::terminal::RenderCell;
#[cfg(test)]
use aterm_render::Theme;

use crate::chrome_band::{self, BandColors};
use crate::settings::{blank_row, write_str};
use crate::tray_raster;
use crate::type_scale::{StepPx, TypeStep};
use crate::widget::{DrawPrim, TextFace, TextWeight, rgba, text_prim};

/// Rows the find panel occupies: a blank pad, the field row, the hints row (fewer on a terminal too short; see [`find_bar_paint`]).
pub(crate) const FIND_BAR_ROWS: usize = if cfg!(windows) { 2 } else { 3 };

/// Where the panel's separator rule runs: the edge that faces the terminal content.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum FindSeam {
    /// The panel floated to the BOTTOM (content above).
    Top,
    /// The panel at the TOP (content below), its rule drawn by the splice as a one-pixel rail on the band's last pixel row (a `ChromeRaster`, the message band's seam), clear of every descender.
    BottomRail,
    /// The panel at the top on a cell too short to keep its words off a rail (`aterm_render::band::seam_can_rail`).
    BottomUnderline,
}

/// Left indent of the `Find:` prompt.
const LEFT_PAD: usize = 2;

/// Right margin kept clear of the window edge.
const RIGHT_PAD: usize = 2;

/// Blank cells between the input well and the right-side run.
const GAP: usize = 3;

/// The `Find: ` prompt drawn ahead of the well. Its width fixes where the well begins.
const PROMPT: &str = "Find: ";

/// The input well never shrinks below this many cells: the right side is dropped first. Only
const MIN_FIELD: usize = 28;

/// …and never grows past this: a wider trough reads as a hole, not a field.
const MAX_FIELD: usize = 48;

/// `[Aa] [.*]`: both toggles; the on state reverses the same cells.
const IND_W: usize = 9;

/// Cells reserved for the status readout, from the window width only, so the well does not
fn status_zone(cols: usize) -> usize {
    (cols / 6).clamp(6, 28)
}

/// Shown dimmed in an empty well — the same affordance a native search field has.
const PLACEHOLDER: &str = "search the screen + scrollback";

/// Drawn in the well's edge cell when the query continues past it.
const SCROLL_LEFT: char = '‹';
const SCROLL_RIGHT: char = '›';

/// The find state the panel paints: a value copy, so the pure builder needs no `App`.
pub(crate) struct FindBarView {
    pub query: String,
    /// The caret as a byte offset into `query`, matching [`crate::app_search::SearchState::cursor`].
    pub cursor: usize,
    /// 1-based position of the current match (meaningful only when `total > 0`).
    pub idx: usize,
    pub total: usize,
    pub case_sensitive: bool,
    pub is_regex: bool,
    /// The query was an invalid regex (regex mode only) — shown as `bad regex`.
    pub regex_error: bool,
    /// The count is a floor (`+`).
    pub truncated: bool,
    /// How many of the newest lines the depth cap let the search read; `None` for the whole buffer.
    pub searched_lines: Option<usize>,
    /// The list stopped at the match limit: `1/100000+ (match limit)`.
    pub match_capped: bool,
    /// A regex scan could not finish some line within its budget: `3/7+ (line cut short)`.
    pub cut_short: bool,
    /// The terminal changed since `total` was counted (`…`); it can hold with [`Self::truncated`] (`1/6+…`).
    pub stale: bool,
    /// The search ran while a rewrap held the history detached: a miss is `none yet` / `rewrapping`, never `no matches`, and not tinted.
    pub history_away: bool,
}

impl FindBarView {
    /// The caret as a character index into the query, clamped and floored onto a char
    fn cursor_chars(&self) -> usize {
        let mut byte = self.cursor.min(self.query.len());
        while byte > 0 && !self.query.is_char_boundary(byte) {
            byte -= 1;
        }
        self.query[..byte].chars().count()
    }
}

/// The painted panel and the geometry the mouse and the splice need.
pub(crate) struct FindBarPaint {
    pub rows: Vec<Vec<RenderCell>>,
    /// Index within [`Self::rows`] of the row carrying the well + indicators.
    pub field_row: usize,
    /// Cell-column spans of the clickable `Aa` (case) / `.*` (regex) indicators, or `None` when the window was too narrow to draw them.
    pub case_cols: Option<Range<usize>>,
    pub regex_cols: Option<Range<usize>>,
    /// Cell-column spans of the `↑` (previous match), `↓` (next match) and `✕` (close, as Esc) buttons ([`FIND_TOOLBAR`]), or `None` off Windows and when the row was too narrow to draw them.
    pub prev_cols: Option<Range<usize>>,
    pub next_cols: Option<Range<usize>>,
    pub close_cols: Option<Range<usize>>,
    /// Columns of the input well on the field row.
    pub field_cols: Range<usize>,
    /// Character index of the query shown in the well's first cell (the horizontal scroll).
    pub field_scroll: usize,
    /// The separator rule's tone, which the splice paints a [`FindSeam::BottomRail`] in, so the rail and the cell-drawn seams cannot disagree.
    pub seam: [u8; 3],
    /// The accent line a focused Fluent text box closes on, which the splice lays along the bottom of the well ([`WELL_ACCENT_LINE`]).
    pub well_line: Option<[u8; 3]>,
    /// Cell columns of the status readout on the field row (`2/7`, `no matches`), or `None` when none was painted.
    pub status_cols: Option<Range<usize>>,
    /// Index within [`Self::rows`] of the key-hint row, or `None` on a 1-row panel.
    pub hint_row: Option<usize>,
}

/// On Windows, 2 px accent line along the bottom edge of the query well (`0` on macOS and Linux).
pub(crate) const WELL_ACCENT_LINE: bool = cfg!(windows);

/// Windows sets the field row as the find toolbar a Windows Terminal or VS Code user knows.
pub(crate) const FIND_TOOLBAR: bool = cfg!(windows);

/// The toolbar's well never shrinks below this many cells ([`FIND_TOOLBAR`]):
const TOOLBAR_MIN_FIELD: usize = 15;

/// …and never grows past this: a find box, not a second command line. A
const TOOLBAR_MAX_FIELD: usize = 40;

/// One toolbar toggle, ` Aa ` or ` .* `: its glyphs padded by a cell each side,
const TOGGLE_W: usize = 4;

/// Both toolbar toggles and the band cell between them, so two resting chips
const TOGGLES_W: usize = 2 * TOGGLE_W + 1;

/// One toolbar button, ` ↑ `: a glyph padded to a three-cell click target.
const BUTTON_W: usize = 3;

/// The toolbar buttons' glyphs, in paint order: previous, next, close.
const TOOLBAR_BUTTONS: [char; 3] = ['\u{2191}', '\u{2193}', '\u{2715}'];

/// One coloured run on the panel's right side; `bg` overrides the band background (a
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

/// The status readout, widest form that fits `zone` cells first: bad regex, match position,
fn status_seg(v: &FindBarView, c: &BandColors, zone: usize) -> Option<Seg> {
    if v.query.is_empty() {
        return None;
    }
    let facts = crate::app_search::CountFacts {
        truncated: v.truncated,
        stale: v.stale,
        history_away: v.history_away,
        searched_lines: v.searched_lines,
        match_capped: v.match_capped,
        cut_short: v.cut_short,
    };
    let mark = facts.mark();
    let stale_mark = if v.stale { "…" } else { "" };
    let (candidates, fg, bold) = if v.regex_error {
        (
            vec!["bad regex".to_string(), "invalid".to_string()],
            c.warn,
            true,
        )
    } else if v.history_away && v.total == 0 {
        (
            vec![
                crate::app_search::HISTORY_AWAY_NONE.to_string(),
                "none yet (rewrapping)".to_string(),
                "rewrapping history".to_string(),
                "rewrapping".to_string(),
                "rewrap".to_string(),
            ],
            c.label,
            false,
        )
    } else if v.total == 0 {
        use crate::app_search::parenthesized;
        let mut forms = vec![format!(
            "{}{}",
            facts.none_base(false),
            parenthesized(&facts.none_words())
        )];
        if let Some(word) = facts.cut_short_word(true) {
            // A regex could not finish a long line: the miss is not a census.
            forms.push(format!("no matches ({word}){stale_mark}"));
        }
        if let (Some(long), Some(short)) = (facts.depth(false), facts.depth(true)) {
            let short_count = short.trim_end_matches(" lines");
            forms.push(format!("none in {long}{stale_mark}"));
            forms.push(format!("none in {short}{stale_mark}"));
            forms.push(format!("none in {short_count}{stale_mark}"));
        }
        forms.push(format!("no matches{mark}"));
        forms.push(format!("0 hits{mark}"));
        (forms, c.warn, false)
    } else {
        use crate::app_search::parenthesized;
        let (i, n, plus) = (v.idx, v.total, facts.plus());
        (
            vec![
                format!("{i}/{n}{plus}{}", parenthesized(&facts.hit_words(false))),
                format!("{i}/{n}{plus}{}", parenthesized(&facts.hit_words(true))),
                format!("{i}/{n}{mark}"),
                format!("{n}{mark}"),
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

/// The `[Aa]` / `[.*]` toggles: bracketed so they read as controls, reversed when active, at
fn indicator_segs(v: &FindBarView, c: &BandColors) -> Vec<Seg> {
    let toggle = |text: &str, on: bool| {
        let mut s = seg(text, if on { c.bar_bg } else { c.label }, on);
        s.bg = on.then_some(c.value);
        s
    };
    if FIND_TOOLBAR {
        let rest_fill = toggle_rest_fill(c);
        let rest_ink = chrome_band::ensure_contrast(c.value, rest_fill, TOGGLE_INK);
        let (on_fill, on_ink) = toggle_on_colors(c);
        let chip = |text: &str, on: bool| {
            let mut s = toggle(text, on);
            (s.fg, s.bg) = if on {
                (on_ink, Some(on_fill))
            } else {
                (rest_ink, Some(rest_fill))
            };
            s
        };
        return vec![
            chip(" Aa ", v.case_sensitive),
            seg(" ", c.label, false),
            chip(" .* ", v.is_regex),
        ];
    }
    vec![
        toggle("[Aa]", v.case_sensitive),
        seg(" ", c.label, false),
        toggle("[.*]", v.is_regex),
    ]
}

/// The toolbar's `↑` / `↓` / `✕` buttons ([`FIND_TOOLBAR`]), each padded to
fn button_segs(c: &BandColors) -> Vec<Seg> {
    TOOLBAR_BUTTONS
        .iter()
        .map(|glyph| seg(format!(" {glyph} "), c.value, false))
        .collect()
}

/// The well's caret ink: on Windows ([`FIND_TOOLBAR`]) the band's accent, the
fn caret_ink(c: &BandColors) -> [u8; 3] {
    if FIND_TOOLBAR && c.well_rule.is_none() {
        aterm_messages::ink::ensure_contrast(c.accent, c.field_bg, WELL_INK)
    } else {
        c.caret
    }
}

/// The keymap taught on the hints row, widest variant that fits first.
fn hint_text(cols: usize) -> &'static str {
    #[cfg(target_os = "macos")]
    const FULL: &str = "⌘S next  ⌘R prev  ⌥⌘C case  ⌥⌘R regex  ⏎ accept  esc cancel";
    #[cfg(target_os = "macos")]
    const SHORT: &str = "⌘S/⌘R next/prev  ⏎ accept  esc cancel";
    #[cfg(windows)]
    const FULL: &str = "F3 next  Shift+F3 prev  Alt+C case  Alt+R regex  Enter accept  Esc cancel";
    #[cfg(windows)]
    const SHORT: &str = "F3/Shift+F3 next/prev  Enter accept  Esc cancel";
    #[cfg(not(any(target_os = "macos", windows)))]
    const FULL: &str = "Ctrl+S next  Ctrl+R prev  Alt+C case  Alt+R regex  ⏎ accept  esc cancel";
    #[cfg(not(any(target_os = "macos", windows)))]
    const SHORT: &str = "Ctrl+S/R next/prev  ⏎ accept  esc cancel";
    #[cfg(windows)]
    const TINY: &str = "Enter accept  Esc cancel";
    #[cfg(not(windows))]
    const TINY: &str = "⏎ accept  esc cancel";
    let room = cols.saturating_sub(LEFT_PAD + RIGHT_PAD);
    for hint in [FULL, SHORT] {
        if hint.chars().count() <= room {
            return hint;
        }
    }
    TINY
}

/// Build the find panel.
pub(crate) fn find_bar_paint(
    v: &FindBarView,
    cols: usize,
    height: usize,
    palette: impl Into<chrome_band::ChromePalette>,
    seam: FindSeam,
) -> FindBarPaint {
    let c = chrome_band::band_colors(palette);
    let height = height.max(1);
    let field_row = usize::from(height >= 3);
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
            prev_cols: None,
            next_cols: None,
            close_cols: None,
            field_cols: 0..0,
            field_scroll: 0,
            seam: c.seam,
            well_line: None,
            status_cols: None,
            hint_row,
        };
    }

    let well_start = (LEFT_PAD + PROMPT.chars().count()).min(cols);
    let layout = if FIND_TOOLBAR {
        toolbar_layout(v, &c, cols, well_start)
    } else {
        field_layout(v, &c, cols, well_start)
    };
    let field_cols = layout.well.clone();
    let field_scroll = {
        let row = &mut rows[field_row];
        write_str(row, cols, LEFT_PAD, PROMPT, c.value, c.bar_bg, true);
        let scroll = paint_well(row, v, &c, field_cols.clone());
        for (start, segs) in [
            layout.indicators.as_ref(),
            layout.status.as_ref(),
            layout.buttons.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            write_segs(row, cols, *start, segs, c.bar_bg);
        }
        scroll
    };
    let (case_cols, regex_cols) = match layout.indicators.as_ref() {
        Some((start, _)) => (
            Some(*start..start + TOGGLE_W),
            Some(start + TOGGLE_W + 1..start + 2 * TOGGLE_W + 1),
        ),
        None => (None, None),
    };
    let button = |i: usize| {
        layout
            .buttons
            .as_ref()
            .map(|(start, _)| start + i * BUTTON_W..start + (i + 1) * BUTTON_W)
    };
    let (prev_cols, next_cols, close_cols) = (button(0), button(1), button(2));
    let status_cols = layout.status.as_ref().map(|(start, segs)| {
        *start..start + segs.iter().map(|s| s.text.chars().count()).sum::<usize>()
    });

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

    match seam {
        FindSeam::Top => chrome_band::seal_band_top(&mut rows[0], c.seam),
        FindSeam::BottomUnderline => {
            for cell in &mut rows[height - 1] {
                cell.underline = aterm_core::terminal::UnderlineStyle::Single;
                cell.underline_color = Some(c.seam);
            }
        }
        FindSeam::BottomRail => {}
    }

    let well_line =
        (WELL_ACCENT_LINE && c.well_rule.is_none() && !field_cols.is_empty()).then_some(c.accent);
    FindBarPaint {
        rows,
        field_row,
        case_cols,
        regex_cols,
        prev_cols,
        next_cols,
        close_cols,
        field_cols,
        field_scroll,
        seam: c.seam,
        well_line,
        status_cols,
        hint_row,
    }
}

/// The CURRENT match's fill: the chrome accent, solid.
pub(crate) fn current_match_fill(
    tokens: &aterm_messages::tokens::ChromeTokens,
    others: [u8; 3],
) -> [u8; 3] {
    use aterm_messages::ink::{bg_is_light, contrast, mix3};
    let away = if bg_is_light(tokens.bg) {
        [0, 0, 0]
    } else {
        [0xFF, 0xFF, 0xFF]
    };
    (0..=10u8)
        .map(|step| mix3(tokens.accent, away, f32::from(step) / 10.0))
        .find(|fill| {
            contrast(*fill, tokens.bg) >= CURRENT_RISE && contrast(*fill, others) >= CURRENT_STEP
        })
        .unwrap_or(away)
}

/// The CURRENT match's text ink on its `fill` ([`current_match_fill`]): the background the fill was walked away from.
pub(crate) fn current_match_ink(
    tokens: &aterm_messages::tokens::ChromeTokens,
    fill: [u8; 3],
) -> [u8; 3] {
    aterm_messages::ink::ensure_contrast(tokens.bg, fill, CURRENT_INK)
}

/// The contrast the current match's text reaches for on its fill: WCAG AAA (the
const CURRENT_INK: f64 = 7.0;

/// The least contrast the well's caret holds on the well: WCAG AA.
const WELL_INK: f64 = 4.5;

/// The least contrast an off toolbar toggle's glyphs hold on its resting fill
const TOGGLE_INK: f64 = 4.5;

/// The least contrast an off toolbar toggle's resting fill stands from the
const TOGGLE_REST_STEP: f64 = 1.4;

/// An ON toolbar toggle's fill and ink ([`FIND_TOOLBAR`]): the band's accent
fn toggle_on_colors(c: &BandColors) -> ([u8; 3], [u8; 3]) {
    (
        c.accent,
        chrome_band::ensure_contrast(c.on_accent, c.accent, TOGGLE_INK),
    )
}

/// An off toolbar toggle's resting fill ([`FIND_TOOLBAR`]): the message band's
fn toggle_rest_fill(c: &BandColors) -> [u8; 3] {
    use chrome_band::{contrast, ensure_contrast, mix3};
    if chrome_band::forced_chrome().is_some()
        || contrast(c.chip_ground, c.bar_bg) >= TOGGLE_REST_STEP
    {
        return c.chip_ground;
    }
    (1..=32u8)
        .map(|step| mix3(c.chip_ground, c.value, f32::from(step) / 32.0))
        .find(|fill| contrast(*fill, c.bar_bg) >= TOGGLE_REST_STEP)
        .unwrap_or_else(|| ensure_contrast(c.chip_ground, c.bar_bg, TOGGLE_REST_STEP))
}

/// The least contrast the current match's fill stands from the background: the
const CURRENT_RISE: f64 = 3.0;

/// The least contrast the current match's fill stands from every other match's
const CURRENT_STEP: f64 = 2.0;

/// The query's ink: the warn tone across a search that found nothing or a bad
fn query_ink(v: &FindBarView, c: &BandColors) -> [u8; 3] {
    if !v.query.is_empty() && (v.regex_error || (v.total == 0 && !v.history_away)) {
        c.warn
    } else {
        c.value
    }
}

/// Paint the input well: the visible slice of the query on the terminal's own background,
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
    let ink = query_ink(v, c);
    let max_start = (chars.len() + 1).saturating_sub(width);
    let scroll = cursor.saturating_sub(width - 1).min(max_start);
    let caret = caret_ink(c);
    for i in 0..width {
        let index = scroll + i;
        let col = field.start + i;
        let ch = chars.get(index).copied().unwrap_or(' ');
        row[col] = if index == cursor {
            // Reverse video: the well's background becomes the ink.
            chrome_band::cell(ch, c.field_bg, caret, false, false)
        } else {
            chrome_band::cell(ch, ink, c.field_bg, false, false)
        };
        if index > chars.len() {
            break;
        }
    }
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
    if scroll > 0 && cursor != scroll {
        row[field.start] = chrome_band::cell(SCROLL_LEFT, c.label, c.field_bg, false, false);
    }
    let last = scroll + width - 1;
    if last < chars.len() && cursor != last {
        row[field.end - 1] = chrome_band::cell(SCROLL_RIGHT, c.label, c.field_bg, false, false);
    }
    if let Some(rule) = c.well_rule {
        for cell in &mut row[field] {
            cell.underline = aterm_core::terminal::UnderlineStyle::Single;
            cell.underline_color = Some(rule);
        }
    }
    scroll
}

/// Where the field row's pieces land: the single source of the row's geometry, so the click
struct FieldLayout {
    well: Range<usize>,
    /// `(start_col, segs)` of the `[Aa] [.*]` run, when it fits.
    indicators: Option<(usize, Vec<Seg>)>,
    /// `(start_col, segs)` of the status readout, when there is one and it fits.
    status: Option<(usize, Vec<Seg>)>,
    /// `(start_col, segs)` of the toolbar's `↑ ↓ ✕` buttons ([`FIND_TOOLBAR`]),
    buttons: Option<(usize, Vec<Seg>)>,
}

/// The Windows field row ([`FIND_TOOLBAR`]), a toolbar in two groups:
fn toolbar_layout(v: &FindBarView, c: &BandColors, cols: usize, well_start: usize) -> FieldLayout {
    let zone = status_zone(cols);
    let room = cols.saturating_sub(well_start + RIGHT_PAD);
    let buttons = TOOLBAR_BUTTONS.len() * BUTTON_W;
    let rungs = [
        (true, true, 1 + TOGGLES_W + 1 + zone + 1 + buttons),
        (true, false, 1 + TOGGLES_W + 1 + zone),
        (false, false, GAP + zone),
    ];
    let Some(&(with_toggles, with_buttons, take)) = rungs
        .iter()
        .find(|&&(_, _, take)| room >= TOOLBAR_MIN_FIELD + take)
    else {
        return FieldLayout {
            well: well_start..cols.saturating_sub(RIGHT_PAD).max(well_start),
            indicators: None,
            status: None,
            buttons: None,
        };
    };
    let well_end = well_start + (room - take).min(TOOLBAR_MAX_FIELD);
    let mut layout = FieldLayout {
        well: well_start..well_end,
        indicators: None,
        status: None,
        buttons: None,
    };
    if with_toggles {
        layout.indicators = Some((well_end + 1, indicator_segs(v, c)));
    }
    let right_edge = well_start + room;
    let zone_end = if with_buttons {
        let start = right_edge - buttons;
        layout.buttons = Some((start, button_segs(c)));
        start - 1
    } else {
        right_edge
    };
    if let Some(status) = status_seg(v, c, zone) {
        let start = zone_end.saturating_sub(status.text.chars().count());
        layout.status = Some((start, vec![status]));
    }
    layout
}

fn field_layout(v: &FindBarView, c: &BandColors, cols: usize, well_start: usize) -> FieldLayout {
    let zone = status_zone(cols);
    let room = cols.saturating_sub(well_start + RIGHT_PAD);
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
        buttons: None,
    };
    if reserve == 0 {
        return layout;
    }
    let right_edge = cols.saturating_sub(RIGHT_PAD);
    let indicator_start = right_edge.saturating_sub(IND_W);
    if with_indicators {
        layout.indicators = Some((indicator_start, indicator_segs(v, c)));
    }
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

/// The window geometry the bar's pixel face is drawn at ([`fluent_bar`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct FluentGeom {
    pub cell_w: usize,
    pub cell_h: usize,
    pub scale: f32,
}

/// How far the query box and the toggle chips stand off the field row's top
const CONTROL_INSET_EPX: f32 = 3.0;

/// The corner radius of the query box and the chips: WinUI's
const CONTROL_RADIUS_EPX: f32 = 4.0;

/// The space between `Find:` and the query box.
const LABEL_GAP_EPX: f32 = 8.0;

/// The query box's text inset, left and right; the `‹` / `›` edge marks sit
const BOX_PAD_EPX: f32 = 8.0;

/// The query box's focus line: a focused WinUI TextBox's bottom border.
const FOCUS_LINE_EPX: f32 = 2.0;

/// A chip's inset inside its cell span, left and right.
const CHIP_SIDE_EPX: f32 = 1.0;

/// The space between two items of the hint row (`F3 next`, `Shift+F3 prev`),
const HINT_GAP_EPX: f32 = 14.0;

/// The UI face's descender, in em (Segoe UI's is 0.21): the query's line is
const UI_DESCENT_EM: f32 = 0.21;

/// The UI face's cap height, in em (Segoe UI's is 0.7): `↑` `↓` `✕` are
const UI_CAP_EM: f32 = 0.7;

/// The scalar advance floor required by the Fluent well's bounded scans.
const MIN_ADVANCE_EM: f32 = 0.2;

impl FluentGeom {
    fn scale(self) -> f32 {
        if self.scale.is_finite() && self.scale > 0.0 {
            self.scale
        } else {
            1.0
        }
    }

    /// `v` effective pixels in whole device pixels, at least one.
    fn epx(self, v: f32) -> f32 {
        (v * self.scale()).round().max(1.0)
    }

    /// The left edge of cell column `col`, px from the panel's left edge.
    fn x(self, col: usize) -> f32 {
        (col * self.cell_w) as f32
    }

    /// The chrome's label base at this row and scale: the tab title's
    fn base_px(self) -> f32 {
        crate::tab_bar::chrome_label_base_px(self.cell_h as f32, self.scale())
    }

    /// The field row's words (`Find:`, the query, the count, `Aa` / `.*`): the
    fn body(self) -> StepPx {
        TypeStep::Body.px(self.base_px())
    }

    /// The hint row's words: a step below, a pane header's size.
    fn hint(self) -> StepPx {
        TypeStep::Secondary.px(self.base_px())
    }

    /// The controls' top and bottom (exclusive) on the field row, px from its
    fn control_y(self) -> (f32, f32) {
        let h = self.cell_h as f32;
        let line = (self.body().get() * 1.25).ceil();
        let inset = self
            .epx(CONTROL_INSET_EPX)
            .min(((h - line) / 2.0).floor())
            .max(0.0);
        (inset, h - inset)
    }

    fn caret_w(self) -> f32 {
        self.epx(1.0)
    }
}

/// The query box on the field row, px from the panel's left edge.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct WellBox {
    pub x0: f32,
    pub x1: f32,
    pub text_x0: f32,
    pub text_x1: f32,
}

/// The query box over the well's cells `field` (panel columns).
pub(crate) fn well_box(field: &Range<usize>, g: FluentGeom) -> WellBox {
    let label = tray_raster::ui_text_width_for(TextFace::Ui, PROMPT.trim_end(), g.body().get());
    let cells = (g.x(field.start), g.x(field.end));
    let x0 = (g.x(LEFT_PAD) + label + g.epx(LABEL_GAP_EPX))
        .round()
        .min(cells.0);
    let x1 = cells.1.max(x0);
    let pad = g.epx(BOX_PAD_EPX);
    let text_x0 = (x0 + pad).min(x1);
    WellBox {
        x0,
        x1,
        text_x0,
        text_x1: (x1 - pad).max(text_x0),
    }
}

/// The width of `chars` set in the UI face at `px`.
fn ui_width(chars: &[char], px: f32) -> f32 {
    if chars.is_empty() {
        return 0.0;
    }
    let s: String = chars.iter().collect();
    tray_raster::ui_text_width_for(TextFace::Ui, &s, px)
}

fn minimum_advance(px: f32) -> f32 {
    (px * MIN_ADVANCE_EM).max(0.5)
}

/// The most characters `room` px can show at `px`, for an admitted query.
fn reach(room: f32, px: f32) -> usize {
    (room.max(0.0) / minimum_advance(px)).ceil() as usize + 1
}

/// The query box's horizontal scroll: the first character it shows, the least
fn well_scroll(chars: &[char], cursor: usize, room: f32, caret_w: f32, px: f32) -> usize {
    let cursor = cursor.min(chars.len());
    if room < caret_w {
        return cursor;
    }
    let fits = |scroll: usize| ui_width(&chars[scroll..cursor], px) + caret_w <= room;
    let (mut lo, mut hi) = (cursor.saturating_sub(reach(room, px)), cursor);
    while lo < hi {
        let mid = (lo + hi) / 2;
        if fits(mid) {
            hi = mid;
        } else {
            lo = mid + 1;
        }
    }
    lo
}

/// The character boundary nearest `x` px (from the text's left edge) in a
fn char_at(chars: &[char], scroll: usize, x: f32, room: f32, px: f32) -> usize {
    let scroll = scroll.min(chars.len());
    if x <= 0.0 {
        return scroll;
    }
    let end = chars.len().min(scroll + reach(room, px));
    let at = |i: usize| ui_width(&chars[scroll..i], px);
    if at(end) <= x {
        return end;
    }
    let (mut lo, mut hi) = (scroll, end);
    while lo < hi {
        let mid = (lo + hi) / 2;
        if at(mid) >= x {
            hi = mid;
        } else {
            lo = mid + 1;
        }
    }
    if lo > scroll && x - at(lo - 1) < at(lo) - x {
        lo - 1
    } else {
        lo
    }
}

/// A click at `x` px from the panel's left edge on the field row, when the bar drew its pixel face for `query` ([`fluent_ready`]).
pub(crate) fn well_click(
    query: &str,
    scroll: usize,
    field: &Range<usize>,
    g: FluentGeom,
    x: f32,
) -> Option<usize> {
    fluent_ready(query, g)?;
    let well = well_box(field, g);
    if !(well.x0..well.x1).contains(&x) {
        return None;
    }
    let chars: Vec<char> = query.chars().collect();
    let px = g.body().get();
    Some(char_at(
        &chars,
        scroll,
        x - well.text_x0,
        well.text_x1 - well.text_x0,
        px,
    ))
}

/// Whether the bar draws its pixel face for `query`.
pub(crate) fn fluent_ready(query: &str, g: FluentGeom) -> Option<u64> {
    if !FIND_TOOLBAR
        || !tray_raster::strip_band_run_coverable(PROMPT)
        || (!query.is_empty() && !tray_raster::strip_band_run_coverable(query))
    {
        return None;
    }
    let fonts = tray_raster::strip_band_font_epoch();
    let px = g.body().get();
    if !query.is_empty()
        && !tray_raster::ui_text_advances_at_least(TextFace::Ui, query, px, minimum_advance(px))
    {
        return None;
    }
    Some(fonts)
}

/// The bar's pixel face for one frame ([`fluent_bar`]).
pub(crate) struct FluentBar {
    /// One raster per band row, each the panel's width and a cell tall: the image the splice lays over that row's cells.
    pub rows: Vec<Arc<ImageData>>,
    /// The query box's horizontal scroll: the first character it shows.
    pub scroll: usize,
    /// The caret's left edge, px from the panel's left edge.
    pub caret_x: f32,
}

/// What [`fluent_bar`] draws from.
pub(crate) struct FluentInput<'a> {
    pub view: &'a FindBarView,
    pub paint: &'a FindBarPaint,
    pub colors: BandColors,
    pub geom: FluentGeom,
    pub seam: FindSeam,
    pub preedit: Option<Range<usize>>,
}

/// Everything the pixels are a function of, for [`fluent_bar`]'s cache.
#[derive(Clone, PartialEq, Eq, Hash)]
struct FluentKey {
    query: String,
    cursor: usize,
    ink: [u8; 3],
    status: Option<(Range<usize>, String, [u8; 3])>,
    toggles: (bool, bool),
    spans: [Option<Range<usize>>; 5],
    field_cols: Range<usize>,
    rows: (usize, usize, Option<usize>),
    cols: usize,
    colors: BandColors,
    well_line: Option<[u8; 3]>,
    cell: (usize, usize),
    scale: u32,
    seam: FindSeam,
    preedit: Option<Range<usize>>,
    fonts: u64,
}

/// Frames [`fluent_bar`] keeps: a window's bar and its neighbour's.
const FLUENT_CACHE: usize = 4;

thread_local! {
    static FLUENT_BARS: std::cell::RefCell<Vec<(FluentKey, Arc<FluentBar>)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// The find toolbar's Fluent face on Windows.
pub(crate) fn fluent_bar(input: &FluentInput<'_>) -> Option<Arc<FluentBar>> {
    let p = input.paint;
    let cols = p.rows.first()?.len();
    if cols == 0 || input.geom.cell_w == 0 || input.geom.cell_h == 0 {
        return None;
    }
    let fonts = fluent_ready(&input.view.query, input.geom)?;
    let status = p.status_cols.clone().map(|span| {
        let row = &p.rows[p.field_row];
        let text: String = row[span.clone()].iter().map(|cell| cell.ch).collect();
        let ink = row[span.start].fg;
        (span, text, ink)
    });
    let key = FluentKey {
        query: input.view.query.clone(),
        cursor: input.view.cursor_chars(),
        ink: query_ink(input.view, &input.colors),
        status,
        toggles: (input.view.case_sensitive, input.view.is_regex),
        spans: [
            p.case_cols.clone(),
            p.regex_cols.clone(),
            p.prev_cols.clone(),
            p.next_cols.clone(),
            p.close_cols.clone(),
        ],
        field_cols: p.field_cols.clone(),
        rows: (p.rows.len(), p.field_row, p.hint_row),
        cols,
        colors: input.colors,
        well_line: p.well_line,
        cell: (input.geom.cell_w, input.geom.cell_h),
        scale: input.geom.scale().to_bits(),
        seam: input.seam,
        preedit: input.preedit.clone(),
        fonts,
    };
    if let Some(bar) = FLUENT_BARS.with(|bars| {
        bars.borrow()
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, bar)| Arc::clone(bar))
    }) {
        return Some(bar);
    }
    let bar = Arc::new(raster_bar(input, &key)?);
    FLUENT_BARS.with(|bars| {
        let mut bars = bars.borrow_mut();
        if bars.len() >= FLUENT_CACHE {
            bars.remove(0);
        }
        bars.push((key, Arc::clone(&bar)));
    });
    Some(bar)
}

/// Rasterize the bar for [`fluent_bar`]; `key` carries the words it read off
fn raster_bar(input: &FluentInput<'_>, key: &FluentKey) -> Option<FluentBar> {
    let (p, c, g) = (input.paint, &input.colors, input.geom);
    let cols = key.cols;
    let height = p.rows.len();
    let row_w = g.x(cols);
    let row_h = g.cell_h as f32;
    let mut scroll = 0;
    let mut caret_x = 0.0;
    let mut rows = Vec::with_capacity(height);
    for row in 0..height {
        let mut prims = if row == p.field_row {
            let (s, x, prims) = field_prims(input, key);
            (scroll, caret_x) = (s, x);
            prims
        } else if Some(row) == p.hint_row {
            hint_prims(c, g, cols)
        } else {
            Vec::new()
        };
        let seam_y = match input.seam {
            FindSeam::Top => (row == 0).then_some(0.0),
            FindSeam::BottomRail | FindSeam::BottomUnderline => {
                (row + 1 == height).then_some(row_h - 1.0)
            }
        };
        if let Some(y) = seam_y {
            prims.push(DrawPrim::Panel {
                x: 0.0,
                y,
                w: row_w,
                h: 1.0,
                radius: 0.0,
                fill: rgba(p.seam, 255),
            });
        }
        rows.push(row_image(&prims, g, cols, c.bar_bg)?);
    }
    Some(FluentBar {
        rows,
        scroll,
        caret_x,
    })
}

/// One band row's raster: `prims` over the band's ground, the panel's width
fn row_image(
    prims: &[DrawPrim],
    g: FluentGeom,
    cols: usize,
    ground: [u8; 3],
) -> Option<Arc<ImageData>> {
    let width = u16::try_from(cols.checked_mul(g.cell_w)?).ok()?;
    let height = u16::try_from(g.cell_h).ok()?;
    let bytes = tray_raster::rasterize_tray_pixels(
        prims,
        u32::from(width),
        u32::from(height),
        1.0,
        rgba(ground, 255),
    );
    Some(Arc::new(ImageData {
        bytes,
        format: ImageFormat::RawRgba8 { width, height },
        cols: u16::try_from(cols).ok()?,
        rows: 1,
        z_index: 0,
        band_lift_px: 0,
        cell_offset_px: (0, 0),
        scaling: ImageScaling::Fit,
        source_rect: None,
    }))
}

/// The field row: `Find:`, the query box, the chips, the count and the
fn field_prims(input: &FluentInput<'_>, key: &FluentKey) -> (usize, f32, Vec<DrawPrim>) {
    let (p, v, c, g) = (input.paint, input.view, &input.colors, input.geom);
    let body = g.body();
    let px = body.get();
    let (y0, y1) = g.control_y();
    let radius = g.epx(CONTROL_RADIUS_EPX).min((y1 - y0) / 2.0);
    let focus = if p.well_line.is_some() {
        g.epx(FOCUS_LINE_EPX)
    } else {
        0.0
    };
    let baseline = (tray_raster::row_baseline(y0, (y1 - y0 - focus).max(1.0), px)
        - px * UI_DESCENT_EM / 2.0)
        .round();
    let text = |prims: &mut Vec<DrawPrim>, x: f32, base: f32, s: &str, ink: [u8; 3]| {
        prims.push(text_prim(
            x.round(),
            base,
            s.to_string(),
            body,
            TextWeight::Regular,
            TextFace::Ui,
            rgba(ink, 255),
        ));
    };
    let mut prims = Vec::new();
    let well = well_box(&p.field_cols, g);
    prims.push(DrawPrim::ClipPush {
        x: 0.0,
        y: 0.0,
        w: (well.x0 - g.epx(2.0)).max(0.0),
        h: g.cell_h as f32,
    });
    text(
        &mut prims,
        g.x(LEFT_PAD),
        baseline,
        PROMPT.trim_end(),
        c.value,
    );
    prims.push(DrawPrim::ClipPop);

    let (scroll, caret_x) = if well.x1 > well.x0 {
        let (bx, bw, bh) = (well.x0, well.x1 - well.x0, y1 - y0);
        prims.push(DrawPrim::Panel {
            x: bx,
            y: y0,
            w: bw,
            h: bh,
            radius,
            fill: rgba(c.field_bg, 255),
        });
        if let Some(line) = p.well_line {
            prims.push(DrawPrim::ClipPush {
                x: bx,
                y: y1 - focus,
                w: bw,
                h: focus,
            });
            prims.push(DrawPrim::Panel {
                x: bx,
                y: y0,
                w: bw,
                h: bh,
                radius,
                fill: rgba(line, 255),
            });
            prims.push(DrawPrim::ClipPop);
        }
        if let Some(rule) = c.well_rule {
            prims.push(DrawPrim::Stroke {
                x: bx + 0.5,
                y: y0 + 0.5,
                w: bw - 1.0,
                h: bh - 1.0,
                radius,
                width: 1.0,
                color: rgba(rule, 255),
            });
        }
        well_text(&mut prims, input, key, &well, (y0, y1 - focus), baseline)
    } else {
        (0, well.x0)
    };

    let chip_base = tray_raster::row_baseline(y0, y1 - y0, px).round();
    let rest_fill = toggle_rest_fill(c);
    let rest_ink = chrome_band::ensure_contrast(c.value, rest_fill, TOGGLE_INK);
    let (on_fill, on_ink) = toggle_on_colors(c);
    for (span, on, label) in [
        (&p.case_cols, v.case_sensitive, "Aa"),
        (&p.regex_cols, v.is_regex, ".*"),
    ] {
        let Some(span) = span else { continue };
        let side = g.epx(CHIP_SIDE_EPX);
        let (xa, xb) = (g.x(span.start) + side, g.x(span.end) - side);
        let (fill, ink) = if on {
            (on_fill, on_ink)
        } else {
            (rest_fill, rest_ink)
        };
        prims.push(DrawPrim::Panel {
            x: xa,
            y: y0,
            w: xb - xa,
            h: y1 - y0,
            radius,
            fill: rgba(fill, 255),
        });
        let w = tray_raster::ui_text_width_for(TextFace::Ui, label, px);
        text(&mut prims, (xa + xb - w) / 2.0, chip_base, label, ink);
    }

    if let Some((span, words, ink)) = &key.status {
        let w = tray_raster::ui_text_width_for(TextFace::Ui, words, px);
        text(&mut prims, g.x(span.end) - w, baseline, words, *ink);
    }

    let base = g.base_px();
    let stroke = (base / 9.0).max(1.0);
    let cy = (baseline - px * UI_CAP_EM / 2.0).round();
    let ink = rgba(c.value, 255);
    let line = |prims: &mut Vec<DrawPrim>, from: (f32, f32), to: (f32, f32)| {
        prims.push(DrawPrim::Line {
            x1: from.0,
            y1: from.1,
            x2: to.0,
            y2: to.1,
            width: stroke,
            color: ink,
        });
    };
    for (span, glyph) in [
        (&p.prev_cols, TOOLBAR_BUTTONS[0]),
        (&p.next_cols, TOOLBAR_BUTTONS[1]),
        (&p.close_cols, TOOLBAR_BUTTONS[2]),
    ] {
        let Some(span) = span else { continue };
        let cx = ((g.x(span.start) + g.x(span.end)) / 2.0).round();
        if glyph == TOOLBAR_BUTTONS[2] {
            let r = (base * 0.26).max(2.0);
            line(&mut prims, (cx - r, cy - r), (cx + r, cy + r));
            line(&mut prims, (cx - r, cy + r), (cx + r, cy - r));
        } else {
            let (a, b) = (base * 0.34, base * 0.24);
            let dir = if glyph == TOOLBAR_BUTTONS[0] {
                -1.0
            } else {
                1.0
            };
            let tip = (cx, cy + dir * a);
            line(&mut prims, (cx, cy - dir * a), tip);
            line(&mut prims, (cx - b, tip.1 - dir * b), tip);
            line(&mut prims, (cx + b, tip.1 - dir * b), tip);
        }
    }
    (scroll, caret_x, prims)
}

/// The query box's contents: the visible slice of the query (or the
fn well_text(
    prims: &mut Vec<DrawPrim>,
    input: &FluentInput<'_>,
    key: &FluentKey,
    well: &WellBox,
    (top, bottom): (f32, f32),
    baseline: f32,
) -> (usize, f32) {
    let (v, c, g) = (input.view, &input.colors, input.geom);
    let body = g.body();
    let px = body.get();
    let chars: Vec<char> = v.query.chars().collect();
    let cursor = key.cursor.min(chars.len());
    let caret_w = g.caret_w();
    let room = well.text_x1 - well.text_x0;
    let scroll = well_scroll(&chars, cursor, room, caret_w, px);
    let shown_end = chars.len().min(scroll + reach(room, px));
    let words = |prims: &mut Vec<DrawPrim>, x: f32, s: String, ink: [u8; 3]| {
        prims.push(text_prim(
            x.round(),
            baseline,
            s,
            body,
            TextWeight::Regular,
            TextFace::Ui,
            rgba(ink, 255),
        ));
    };
    prims.push(DrawPrim::ClipPush {
        x: well.text_x0,
        y: top,
        w: room,
        h: (bottom - top).max(0.0),
    });
    if chars.is_empty() {
        let x = well.text_x0 + caret_w + g.epx(3.0);
        if tray_raster::ui_text_width_for(TextFace::Ui, PLACEHOLDER, px) <= well.text_x1 - x {
            words(prims, x, PLACEHOLDER.to_string(), c.label);
        }
    } else {
        words(
            prims,
            well.text_x0,
            chars[scroll..shown_end].iter().collect(),
            key.ink,
        );
        if let Some(run) = &input.preedit {
            let (a, b) = (
                run.start.clamp(scroll, shown_end),
                run.end.clamp(scroll, shown_end),
            );
            if b > a {
                let xa = well.text_x0 + ui_width(&chars[scroll..a], px);
                let xb = well.text_x0 + ui_width(&chars[scroll..b], px);
                let t = g.epx(1.0);
                prims.push(DrawPrim::Panel {
                    x: xa.round(),
                    y: (baseline + g.epx(2.0)).min(bottom - t),
                    w: (xb - xa).round().max(1.0),
                    h: t,
                    radius: 0.0,
                    fill: rgba(c.value, 255),
                });
            }
        }
    }
    prims.push(DrawPrim::ClipPop);
    let caret_x = (well.text_x0 + ui_width(&chars[scroll..cursor], px))
        .min(well.text_x1 - caret_w)
        .max(well.text_x0)
        .round();
    let (ct, cb) = (top + g.epx(2.0), bottom - g.epx(1.0));
    prims.push(DrawPrim::Panel {
        x: caret_x,
        y: ct,
        w: caret_w,
        h: (cb - ct).max(1.0),
        radius: 0.0,
        fill: rgba(caret_ink(c), 255),
    });
    let mark = |prims: &mut Vec<DrawPrim>, ch: char, x0: f32, x1: f32| {
        let s = ch.to_string();
        let w = tray_raster::ui_text_width_for(TextFace::Ui, &s, px);
        words(prims, x0 + (x1 - x0 - w) / 2.0, s, c.label);
    };
    if scroll > 0 {
        mark(prims, SCROLL_LEFT, well.x0, well.text_x0);
    }
    if shown_end < chars.len() || ui_width(&chars[scroll..shown_end], px) > room {
        mark(prims, SCROLL_RIGHT, well.text_x1, well.x1);
    }
    (scroll, caret_x)
}

/// The hint row: the keymap's items in the UI face a step below the field
fn hint_prims(c: &BandColors, g: FluentGeom, cols: usize) -> Vec<DrawPrim> {
    let size = g.hint();
    let px = size.get();
    let baseline = tray_raster::row_baseline(0.0, g.cell_h as f32, px).round();
    let end = g.x(cols.saturating_sub(RIGHT_PAD));
    let mut x = g.x(LEFT_PAD);
    let mut prims = Vec::new();
    for item in hint_text(cols).split("  ").map(str::trim) {
        if item.is_empty() {
            continue;
        }
        let w = tray_raster::ui_text_width_for(TextFace::Ui, item, px);
        if x + w > end {
            break;
        }
        prims.push(text_prim(
            x.round(),
            baseline,
            item.to_string(),
            size,
            TextWeight::Regular,
            TextFace::Ui,
            rgba(c.label, 255),
        ));
        x += w + g.epx(HINT_GAP_EPX);
    }
    prims
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
            searched_lines: None,
            match_capped: false,
            cut_short: false,
            stale: false,
            history_away: false,
        }
    }

    fn paint(v: &FindBarView, cols: usize) -> FindBarPaint {
        find_bar_paint(v, cols, FIND_BAR_ROWS, Theme::default(), FindSeam::Top)
    }

    fn text(row: &[RenderCell]) -> String {
        row.iter().map(|cell| cell.ch).collect()
    }

    /// The whole panel as one string, rows joined.
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
            .find(|&col| row[col].bg == caret_ink(&c))
            .expect("the caret is always inside the well");
        (
            row[p.field_cols.clone()].iter().map(|c| c.ch).collect(),
            caret,
        )
    }

    /// The well's geometry depends on the window width only, never on the status text, so it
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
                view("zzz"),
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

    /// The caret is a reverse-video block that costs no cell, so the text never shifts.
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
        let c = chrome_band::band_colors(Theme::default());
        let p = paint(
            &FindBarView {
                cursor: 1,
                ..view("abc")
            },
            cols,
        );
        let cell = &p.rows[p.field_row][caret_mid];
        assert_eq!(
            (cell.ch, cell.bg, cell.fg),
            ('b', caret_ink(&c), c.field_bg)
        );
    }

    /// An overlong query scrolls to keep the caret visible, with the clipped edges marked.
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
    #[test]
    fn indicator_cols_match_painted_cells() {
        let cols = 90;
        let p = paint(&view("q"), cols);
        let row = &p.rows[p.field_row];
        let case = p.case_cols.clone().expect("[Aa] drawn at this width");
        let regex = p.regex_cols.clone().expect("[.*] drawn at this width");
        let at = |span: Range<usize>| row[span].iter().map(|c| c.ch).collect::<String>();
        let (aa, re) = if FIND_TOOLBAR {
            (" Aa ", " .* ")
        } else {
            ("[Aa]", "[.*]")
        };
        assert_eq!(at(case), aa, "case span covers the painted toggle");
        assert_eq!(at(regex), re, "regex span covers the painted toggle");
        let buttons = [
            p.prev_cols.clone(),
            p.next_cols.clone(),
            p.close_cols.clone(),
        ];
        if FIND_TOOLBAR {
            for (span, glyph) in buttons.into_iter().zip(TOOLBAR_BUTTONS) {
                let span = span.expect("the buttons are drawn at this width");
                assert_eq!(at(span), format!(" {glyph} "), "a button covers its glyph");
            }
        } else {
            assert!(
                buttons.iter().all(Option::is_none),
                "no buttons off Windows"
            );
        }
    }

    /// NARROW WINDOWS KEEP THE ANSWER. When the row cannot hold everything, the
    #[test]
    fn the_status_outranks_the_toggles_when_space_runs_out() {
        let (narrow_cols, min_field) = if FIND_TOOLBAR {
            (36, TOOLBAR_MIN_FIELD)
        } else {
            (52, MIN_FIELD)
        };
        let narrow = paint(&view("zzz"), narrow_cols);
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
            narrow.field_cols.len() >= min_field,
            "and so does the field"
        );
        let roomy = paint(&view("zzz"), 90);
        assert!(roomy.case_cols.is_some());
        assert!(all(&roomy).contains("no matches"));
        let tiny = paint(&view("q"), 24);
        assert!(!tiny.field_cols.is_empty());
    }

    /// A SPLIT PANE KEEPS THE TOGGLES on Windows ([`FIND_TOOLBAR`]): at the 55
    #[test]
    fn the_toolbar_keeps_its_toggles_in_a_split_pane() {
        let v = FindBarView {
            idx: 2,
            total: 39,
            ..view("Merge")
        };
        let c = chrome_band::band_colors(Theme::default());
        let well_start = LEFT_PAD + PROMPT.chars().count();
        assert!(
            field_layout(&v, &c, 55, well_start).indicators.is_none(),
            "the right-edge layout drops the toggles at 55 columns"
        );
        if !FIND_TOOLBAR {
            return;
        }
        for cols in [55usize, 60, 80, 111, 200] {
            let p = paint(&v, cols);
            let case = p.case_cols.clone().expect("the toggles are kept");
            let regex = p.regex_cols.clone().expect("the toggles are kept");
            let prev = p.prev_cols.clone().expect("the buttons fit");
            let next = p.next_cols.clone().expect("the buttons fit");
            let close = p.close_cols.clone().expect("the buttons fit");
            assert!(p.field_cols.len() >= TOOLBAR_MIN_FIELD, "{cols}");
            assert!(p.field_cols.len() <= TOOLBAR_MAX_FIELD, "{cols}");
            let row: Vec<char> = p.rows[p.field_row].iter().map(|cell| cell.ch).collect();
            let count = row
                .windows(4)
                .position(|w| w == ['2', '/', '3', '9'])
                .expect("the count is painted");
            assert_eq!(
                p.field_cols.end + 1,
                case.start,
                "{cols}: the toggles follow the well"
            );
            assert_eq!(case.end + 1, regex.start, "{cols}: a cell apart");
            assert!(regex.end < count, "{cols}: then the count");
            assert_eq!(
                count + 5,
                prev.start,
                "{cols}: the count sits by the arrows"
            );
            assert_eq!(prev.end, next.start, "{cols}");
            assert_eq!(next.end, close.start, "{cols}");
            assert!(
                close.end <= cols - RIGHT_PAD,
                "{cols}: inside the right margin"
            );
        }
        let p = paint(&v, 45);
        assert!(
            p.close_cols.is_none() && p.case_cols.is_some(),
            "the buttons go first"
        );
    }

    /// THE TOOLBAR ENDS ON ITS CLOSE BUTTON at every width that draws one
    #[test]
    fn the_toolbar_ends_on_its_close_button_at_every_width() {
        if !FIND_TOOLBAR {
            return;
        }
        let v = FindBarView {
            idx: 2,
            total: 39,
            ..view("Merge")
        };
        let mut drew_buttons = 0;
        for cols in 34..=240usize {
            let p = paint(&v, cols);
            let row: String = p.rows[p.field_row].iter().map(|cell| cell.ch).collect();
            let count_end = row
                .find("2/39")
                .map(|at| row[..at].chars().count() + 4)
                .expect("the count outranks every control");
            assert!(p.field_cols.len() <= TOOLBAR_MAX_FIELD, "{cols}");
            match (p.prev_cols.clone(), p.close_cols.clone()) {
                (Some(prev), Some(close)) => {
                    drew_buttons += 1;
                    assert_eq!(close.end, cols - RIGHT_PAD, "{cols}: `✕` ends the row");
                    assert_eq!(count_end + 1, prev.start, "{cols}: the count by `↑`");
                    let regex = p
                        .regex_cols
                        .clone()
                        .expect("the toggles outlast the buttons");
                    assert!(regex.end < count_end - 4, "{cols}: two groups, disjoint");
                }
                (None, None) => {
                    assert_eq!(
                        count_end,
                        cols - RIGHT_PAD,
                        "{cols}: the count at the margin"
                    );
                }
                spans => panic!("{cols}: the buttons go together: {spans:?}"),
            }
            if let Some(case) = p.case_cols.clone() {
                assert_eq!(
                    case.start,
                    p.field_cols.end + 1,
                    "{cols}: toggles by the well"
                );
            }
        }
        assert!(
            drew_buttons > 150,
            "the sweep must judge the buttons: {drew_buttons}"
        );
        let wide = paint(&v, 111);
        assert_eq!(wide.field_cols.len(), TOOLBAR_MAX_FIELD);
        let gap = wide.prev_cols.expect("buttons").start - wide.regex_cols.expect("toggles").end;
        assert!(gap > 20, "the band between the groups: {gap} cells");
    }

    /// AN OFF TOGGLE IS STILL A BUTTON ([`FIND_TOOLBAR`]): its chip wears the
    #[test]
    fn an_off_toggle_rests_on_a_control_fill_in_every_builtin_theme() {
        use aterm_messages::ink::contrast;
        if !FIND_TOOLBAR {
            return;
        }
        let light = chrome_band::band_colors(Theme::from(
            &aterm_types::scheme::builtin("Default Light").expect("builtin"),
        ));
        assert!(
            contrast(light.chip_ground, light.bar_bg) < TOGGLE_REST_STEP,
            "CONTROL: Default Light's message-band chip is under the step"
        );
        let dark = chrome_band::band_colors(Theme::default());
        assert_eq!(
            toggle_rest_fill(&dark),
            dark.chip_ground,
            "Default keeps its chip"
        );
        for name in aterm_types::scheme::builtin_names() {
            let scheme = aterm_types::scheme::builtin(name).expect("a built-in scheme");
            let theme = Theme::from(&scheme);
            let c = chrome_band::band_colors(theme);
            let fill = toggle_rest_fill(&c);
            assert!(
                contrast(fill, c.bar_bg) >= TOGGLE_REST_STEP,
                "{name}: the off chip {fill:?} stands off the band {:?}",
                c.bar_bg
            );
            assert!(
                contrast(fill, c.bar_bg) < TOGGLE_REST_STEP + 0.1 || fill == c.chip_ground,
                "{name}: stepped no further than the floor"
            );
            for (case_sensitive, is_regex) in [(false, false), (true, false), (false, true)] {
                let v = FindBarView {
                    case_sensitive,
                    is_regex,
                    ..view("q")
                };
                let p = find_bar_paint(&v, 111, FIND_BAR_ROWS, theme, FindSeam::Top);
                let row = &p.rows[p.field_row];
                for (span, on) in [
                    (p.case_cols.clone().expect("drawn"), case_sensitive),
                    (p.regex_cols.clone().expect("drawn"), is_regex),
                ] {
                    let gap = row[span.end].bg;
                    assert_eq!(
                        gap, c.bar_bg,
                        "{name}: the band between and after the chips"
                    );
                    for cell in &row[span] {
                        if on {
                            assert_eq!(cell.bg, c.accent, "{name}: ON is the accent");
                            assert!(contrast(cell.fg, cell.bg) >= 4.5, "{name}: ON ink");
                        } else {
                            assert_eq!(cell.bg, fill, "{name}: OFF rests on a fill");
                            assert_ne!(cell.bg, c.bar_bg, "{name}: a step off the band");
                            assert!(
                                contrast(cell.fg, cell.bg) >= 4.5,
                                "{name}: {:?} on {:?}",
                                cell.fg,
                                cell.bg
                            );
                        }
                    }
                }
            }
        }
    }

    /// THE CARET WEARS THE FOCUS LINE'S ACCENT on Windows ([`caret_ink`]): with an
    #[test]
    fn the_caret_wears_the_accent_the_focus_line_does() {
        use aterm_messages::ink::contrast;
        let blue = [0x4C, 0xC2, 0xFF];
        let palette = chrome_band::ChromePalette {
            theme: Theme::default(),
            ansi: None,
            authored: aterm_messages::tokens::AuthoredChrome {
                accent: Some(blue),
                ..aterm_messages::tokens::AuthoredChrome::NONE
            },
        };
        let c = chrome_band::band_colors(palette);
        let p = find_bar_paint(&view("ab"), 90, FIND_BAR_ROWS, palette, FindSeam::Top);
        let caret = p.rows[p.field_row][p.field_cols.start + 2].bg;
        assert_eq!(caret, caret_ink(&c));
        assert!(contrast(caret, c.field_bg) >= WELL_INK);
        if FIND_TOOLBAR {
            assert_eq!(caret, c.accent, "the accent clears AA in the well as is");
            assert_ne!(caret, c.caret, "not the cursor's colour");
            assert_eq!(p.well_line, Some(c.accent), "one mark with the focus line");
        } else {
            assert_eq!(caret, c.caret);
        }
    }

    /// A failed search tints the query itself.
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

    /// The seam rides the content-facing edge and runs edge to edge.
    #[test]
    fn seam_faces_the_content_and_is_unbroken() {
        use aterm_core::terminal::UnderlineStyle;
        let cols = 90;
        let bottom = find_bar_paint(
            &view("q"),
            cols,
            FIND_BAR_ROWS,
            Theme::default(),
            FindSeam::Top,
        );
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
        let top = find_bar_paint(
            &view("q"),
            cols,
            FIND_BAR_ROWS,
            Theme::default(),
            FindSeam::BottomUnderline,
        );
        assert!(
            top.rows[FIND_BAR_ROWS - 1]
                .iter()
                .all(|c| c.underline == UnderlineStyle::Single),
            "top panel on a short cell → an unbroken underline on its bottom edge"
        );
        assert!(
            top.rows.iter().flatten().all(|c| !c.overline),
            "top panel → no overline"
        );
        let railed = find_bar_paint(
            &view("q"),
            cols,
            FIND_BAR_ROWS,
            Theme::default(),
            FindSeam::BottomRail,
        );
        assert!(
            railed
                .rows
                .iter()
                .flatten()
                .all(|c| !c.overline && c.underline == UnderlineStyle::None),
            "top panel with a rail → no cell rule at all: the rail is the splice's"
        );
        assert_eq!(
            railed.seam,
            chrome_band::band_colors(Theme::default()).seam,
            "the rail's tone is the band's seam"
        );
        let band = chrome_band::band_colors(Theme::default());
        for height in [1_usize, 2] {
            let short = find_bar_paint(
                &view("nomatch"),
                cols,
                height,
                Theme::default(),
                FindSeam::Top,
            );
            let seam = &short.rows[0];
            assert!(
                seam.iter()
                    .all(|c| c.overline && c.overline_color == Some(band.label)),
                "{height}-row panel: the seam is one tone across the field row, breaks/tones at {:?}",
                seam.iter()
                    .enumerate()
                    .filter(|(_, c)| !c.overline || c.overline_color != Some(band.label))
                    .map(|(col, c)| (col, c.overline, c.overline_color))
                    .collect::<Vec<_>>()
            );
            let inks: std::collections::BTreeSet<[u8; 3]> = seam.iter().map(|c| c.fg).collect();
            assert!(
                inks.len() > 1,
                "the field row is meant to carry several inks, so this proof is not vacuous: {inks:?}"
            );
        }
    }

    /// THE CURRENT MATCH IS UNMISTAKABLE in every built-in theme: its fill stands
    #[test]
    fn the_current_match_stands_apart_from_the_others_in_every_builtin_theme() {
        use crate::settings::u32_rgb;
        use aterm_messages::ink::contrast;
        let old = Theme::default();
        let others = chrome_band::ChromePalette::from(old)
            .tokens()
            .search_match(u32_rgb(old.selection));
        assert!(
            contrast(u32_rgb(old.selection), others) < CURRENT_STEP,
            "the selection-coloured current match was indistinct from the others"
        );
        let names = aterm_types::scheme::builtin_names();
        assert!(names.len() > 4, "the sweep must actually sweep");
        for name in names {
            let scheme = aterm_types::scheme::builtin(name).expect("a built-in scheme");
            let theme = Theme::from(&scheme);
            let tokens = chrome_band::ChromePalette::from(theme).tokens();
            let others = tokens.search_match(u32_rgb(theme.selection));
            let fill = current_match_fill(&tokens, others);
            assert!(
                contrast(fill, tokens.bg) >= CURRENT_RISE,
                "{name}: the current match {fill:?} must stand off the background {:?}",
                tokens.bg
            );
            assert!(
                contrast(fill, others) >= CURRENT_STEP,
                "{name}: the current match {fill:?} must stand off the other matches {others:?}"
            );
        }
    }

    /// THE CURRENT MATCH'S TEXT READS AT FULL STRENGTH: its ink is the background the
    #[test]
    fn the_current_match_text_wears_the_pole_its_fill_stands_off() {
        use crate::settings::u32_rgb;
        use aterm_messages::ink::contrast;
        let fill_of = |theme: Theme| {
            let tokens = chrome_band::ChromePalette::from(theme).tokens();
            let fill = current_match_fill(&tokens, tokens.search_match(u32_rgb(theme.selection)));
            (tokens, fill)
        };
        let theme = Theme::default();
        let (tokens, fill) = fill_of(theme);
        let floored = u32_rgb(aterm_render::floor_selection_fg(
            theme.fg,
            aterm_render::rgb_to_u32(fill),
        ));
        assert!(
            contrast(floored, fill) < 6.0,
            "the old floor's grey: {floored:?} on {fill:?}"
        );
        assert_eq!(current_match_ink(&tokens, fill), tokens.bg);
        let light = aterm_types::scheme::builtin("Default Light").expect("a built-in");
        let (tokens, fill) = fill_of(Theme::from(&light));
        let ink = current_match_ink(&tokens, fill);
        assert!(
            contrast(ink, fill) > contrast(tokens.bg, fill),
            "past the page's own ink"
        );
        assert_eq!(ink, [0xFF; 3], "white, the most a light side reaches");
        for name in aterm_types::scheme::builtin_names() {
            let scheme = aterm_types::scheme::builtin(name).expect("a built-in scheme");
            let (tokens, fill) = fill_of(Theme::from(&scheme));
            let ink = current_match_ink(&tokens, fill);
            let reach = contrast([0; 3], fill).max(contrast([0xFF; 3], fill));
            assert!(
                contrast(ink, fill) >= 4.5,
                "{name}: {ink:?} on the current match {fill:?}, AA at least"
            );
            if reach >= CURRENT_INK {
                assert!(
                    contrast(ink, fill) >= CURRENT_INK,
                    "{name}: {ink:?} on {fill:?}, AAA where a pole reaches it"
                );
            }
        }
    }

    /// Under every stock Windows High-Contrast scheme the well's fill matches the band, so an
    #[test]
    fn the_query_well_keeps_a_boundary_under_a_forced_palette() {
        use aterm_core::terminal::UnderlineStyle;
        let cols = 90;
        let plain = paint(&view("abc"), cols);
        assert!(
            plain.rows[plain.field_row][plain.field_cols.clone()]
                .iter()
                .all(|c| c.underline == UnderlineStyle::None),
            "a theme-derived well is an inset already: no border"
        );
        let band = chrome_band::band_colors(Theme::default());
        assert_eq!(
            plain.well_line,
            WELL_ACCENT_LINE.then_some(band.accent),
            "the focus line is the band's accent, on Windows only"
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
                assert_eq!(
                    p.rows[p.field_row][p.field_cols.start - 1].underline,
                    UnderlineStyle::None,
                    "{name}: the border marks the field, not the whole row"
                );
                assert_eq!(p.well_line, None, "{name}: the border, not a focus line");
            });
        }
    }

    /// An ACTIVE toggle changes colour at the SAME width, so its state is visible
    #[test]
    fn active_toggle_flips_at_a_fixed_width() {
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
        let inactive = if FIND_TOOLBAR {
            (
                chrome_band::ensure_contrast(c.value, toggle_rest_fill(&c), TOGGLE_INK),
                toggle_rest_fill(&c),
            )
        } else {
            (c.label, c.bar_bg)
        };
        assert_eq!(
            cell_at(&off, off.case_cols.clone().unwrap()),
            inactive,
            "inactive = dim on the band, or on its resting fill"
        );
        let active = if FIND_TOOLBAR {
            (
                chrome_band::ensure_contrast(c.on_accent, c.accent, TOGGLE_INK),
                c.accent,
            )
        } else {
            (c.bar_bg, c.value)
        };
        assert_eq!(
            cell_at(&on, on.case_cols.clone().unwrap()),
            active,
            "active = the accent on Windows, reversed elsewhere"
        );
    }

    /// While a rewrap holds the history away, a miss never reads `no matches` at any width
    #[test]
    fn a_search_while_the_history_is_away_never_says_no_matches() {
        let c = chrome_band::band_colors(Theme::default());
        let away = |stale: bool| FindBarView {
            truncated: true,
            history_away: true,
            stale,
            ..view("needle")
        };
        let field_row = |v: &FindBarView, cols: usize| {
            let p = paint(v, cols);
            let row = text(&p.rows[p.field_row]);
            let ink = p.rows[p.field_row][p.field_cols.start].fg;
            (row, ink)
        };
        for cols in 40..=220 {
            for stale in [false, true] {
                let (row, ink) = field_row(&away(stale), cols);
                assert!(
                    !row.contains("no matches") && !row.contains("0 hits"),
                    "{cols}: {row}"
                );
                assert_ne!(ink, c.warn, "{cols}: the query is not a failure");
            }
        }
        let (row, ink) = field_row(&view("needle"), 80);
        assert!(row.contains("no matches"), "{row}");
        assert_eq!(ink, c.warn);
        let (row, _) = field_row(
            &FindBarView {
                idx: 1,
                total: 3,
                ..away(false)
            },
            60,
        );
        assert!(row.contains("1/3+"), "{row}");
    }

    /// A count is painted bare only when it is an exact census: output since marks it `…`, a
    #[test]
    fn a_count_the_screen_has_moved_past_is_never_painted_bare() {
        let status_row = |v: &FindBarView, cols: usize| {
            let p = paint(v, cols);
            text(&p.rows[p.field_row])
        };
        let counted = FindBarView {
            idx: 1,
            total: 6,
            ..view("needle")
        };
        let bare = status_row(&counted, 90);
        assert!(bare.contains("1/6"), "{bare}");
        assert!(
            !bare.contains('…') && !bare.contains('+'),
            "an exact census is stated plainly: {bare}"
        );

        let moved = FindBarView {
            idx: 1,
            total: 6,
            stale: true,
            ..view("needle")
        };
        let mut widths_checked = 0;
        for cols in [40usize, 50, 60, 72, 90, 140, 200] {
            if !status_row(&counted, cols).contains("1/6") {
                continue;
            }
            widths_checked += 1;
            let painted = status_row(&moved, cols);
            assert!(
                painted.contains('…') || painted.contains("(stale)"),
                "cols={cols} dropped the staleness marker: {painted}"
            );
        }
        assert!(widths_checked >= 4, "the width sweep must actually sweep");
        let both = FindBarView {
            truncated: true,
            stale: true,
            ..FindBarView {
                idx: 1,
                total: 6,
                ..view("needle")
            }
        };
        let painted = status_row(&both, 90);
        assert!(painted.contains("1/6+…"), "{painted}");

        let none = FindBarView {
            idx: 1,
            total: 0,
            stale: true,
            ..view("needle")
        };
        let narrow = status_row(&none, 60);
        assert!(narrow.contains('…'), "{narrow}");
    }

    /// Every width × height (zero-width, degraded 1/2-row, overlong query) stays exactly
    #[test]
    fn narrow_short_and_overlong_never_panic() {
        for cols in [0usize, 1, 4, 8, 12, 20, 40, 200] {
            for height in [1usize, 2, 3] {
                for (cursor, seam) in [
                    (0usize, FindSeam::Top),
                    (7, FindSeam::BottomRail),
                    (200, FindSeam::BottomUnderline),
                ] {
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
                        seam,
                    );
                    assert_eq!(p.rows.len(), height, "cols={cols} height={height}");
                    assert!(p.rows.iter().all(|r| r.len() == cols));
                    assert!(p.field_row < height);
                    assert!(p.field_cols.end <= cols.max(p.field_cols.start));
                }
            }
        }
        let one = find_bar_paint(&view("kept"), 90, 1, Theme::default(), FindSeam::Top);
        assert_eq!(one.field_row, 0);
        assert!(text(&one.rows[0]).contains("kept"));
    }

    /// A multi-byte query is painted and scrolled by character; a mid-codepoint cursor floors.
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

    /// AN ON TOGGLE WEARS THE ACCENT ([`FIND_TOOLBAR`]), as Windows Terminal's
    #[test]
    fn an_on_toggle_wears_the_accent_with_its_on_accent_ink() {
        use aterm_messages::ink::contrast;
        if !FIND_TOOLBAR {
            return;
        }
        let on = |theme: Theme| {
            let v = FindBarView {
                case_sensitive: true,
                ..view("q")
            };
            let p = find_bar_paint(&v, 111, FIND_BAR_ROWS, theme, FindSeam::Top);
            let span = p.case_cols.clone().expect("drawn");
            let cell = &p.rows[p.field_row][span.start + 1];
            (cell.fg, cell.bg)
        };
        for name in aterm_types::scheme::builtin_names() {
            let theme = Theme::from(&aterm_types::scheme::builtin(name).expect("a built-in"));
            let c = chrome_band::band_colors(theme);
            let (fg, bg) = on(theme);
            assert_eq!(bg, c.accent, "{name}: the accent");
            assert!(contrast(fg, bg) >= TOGGLE_INK, "{name}: {fg:?} on {bg:?}");
            if name.starts_with("Default") {
                assert_ne!(bg, c.value, "{name}: CONTROL: not the reversed words");
            }
        }
        for (name, palette) in chrome_band::hc_fixtures::STOCK {
            chrome_band::hc_fixtures::with_forced(palette, || {
                assert_eq!(
                    on(Theme::default()),
                    (palette.highlight_text, palette.highlight),
                    "{name}"
                );
            });
        }
    }

    /// Install this host's UI faces on the test thread; `false` where it has
    fn with_ui_faces() -> bool {
        tray_raster::prepare_ui_fonts_for_direct_view_test();
        tray_raster::strip_band_ui_ready()
    }

    /// The 100 % and 200 % cell boxes of the Windows default face.
    const X1: FluentGeom = FluentGeom {
        cell_w: 9,
        cell_h: 23,
        scale: 1.0,
    };
    const X2: FluentGeom = FluentGeom {
        cell_w: 18,
        cell_h: 47,
        scale: 2.0,
    };

    fn fluent(
        v: &FindBarView,
        g: FluentGeom,
        theme: Theme,
        preedit: Option<Range<usize>>,
    ) -> (FindBarPaint, Arc<FluentBar>) {
        let paint = find_bar_paint(v, 111, FIND_BAR_ROWS, theme, FindSeam::BottomRail);
        let bar = fluent_bar(&FluentInput {
            view: v,
            paint: &paint,
            colors: chrome_band::band_colors(theme),
            geom: g,
            seam: FindSeam::BottomRail,
            preedit,
        })
        .expect("the Windows bar draws its face");
        (paint, bar)
    }

    fn pixel(image: &ImageData, x: f32, y: f32) -> [u8; 3] {
        let ImageFormat::RawRgba8 { width, .. } = image.format else {
            panic!("a raw raster");
        };
        let i = (y as usize * usize::from(width) + x as usize) * 4;
        [image.bytes[i], image.bytes[i + 1], image.bytes[i + 2]]
    }

    /// The x extent of the pixels unlike `ground` in `xs` × `ys`.
    fn ink_span(
        image: &ImageData,
        xs: Range<usize>,
        ys: Range<usize>,
        ground: [u8; 3],
    ) -> Option<Range<usize>> {
        let (mut lo, mut hi) = (usize::MAX, 0);
        for y in ys {
            for x in xs.clone() {
                if pixel(image, x as f32, y as f32) != ground {
                    (lo, hi) = (lo.min(x), hi.max(x + 1));
                }
            }
        }
        (hi > lo).then_some(lo..hi)
    }

    /// THE WINDOWS BAR SETS ITS WORDS IN THE UI FACE ([`fluent_bar`]): `Find:`,
    #[test]
    fn the_windows_bar_sets_its_words_in_the_ui_face() {
        if !FIND_TOOLBAR {
            return;
        }
        if !with_ui_faces() {
            aterm_spec::host::could_not_run("no system UI face here for the strip band");
        }
        let c = chrome_band::band_colors(Theme::default());
        let v = FindBarView {
            idx: 2,
            total: 18,
            ..view("Merge")
        };
        let (p, bar) = fluent(&v, X1, Theme::default(), None);
        assert_eq!(bar.rows.len(), FIND_BAR_ROWS);
        for image in &bar.rows {
            assert_eq!(
                image.format,
                ImageFormat::RawRgba8 {
                    width: 999,
                    height: 23
                }
            );
            assert_eq!((image.cols, image.rows, image.z_index), (111, 1, 0));
        }
        let field = &bar.rows[p.field_row];
        let body = X1.body().get();
        let measure = |s: &str, px: f32| tray_raster::ui_text_width_for(TextFace::Ui, s, px);
        let well = well_box(&p.field_cols, X1);
        let label = ink_span(field, 0..well.x0 as usize, 0..23, c.bar_bg).expect("drawn");
        assert!(
            label.start.abs_diff(X1.x(LEFT_PAD) as usize) <= 2,
            "{label:?}"
        );
        let face = measure("Find:", body);
        let inked = label.len() as f32;
        assert!(
            inked <= face && inked >= face - 4.0,
            "{label:?}: inside the UI face's {face} px advance"
        );
        assert!(label.len() < 5 * 9 * 3 / 4, "CONTROL: under the cells' run");
        let status = p.status_cols.clone().expect("a count");
        assert_eq!(text(&p.rows[p.field_row][status.clone()]), "2/18");
        let count = ink_span(
            field,
            X1.x(status.start - 4) as usize..X1.x(status.end) as usize + 2,
            0..23,
            c.bar_bg,
        )
        .expect("drawn");
        assert!(
            count.end.abs_diff(X1.x(status.end) as usize) <= 2,
            "{count:?}"
        );
        let face = measure("2/18", body);
        assert!(count.len() as f32 <= face && count.len() as f32 >= face - 4.0);
        let hints = &bar.rows[p.hint_row.expect("a hint row")];
        let small = X1.hint().get();
        assert!(small < body, "the hints a step below");
        let items: Vec<&str> = hint_text(111).split("  ").collect();
        let wide = items.iter().map(|item| measure(item, small)).sum::<f32>()
            + X1.epx(HINT_GAP_EPX) * (items.len() - 1) as f32;
        let drawn = ink_span(hints, 0..999, 0..22, c.bar_bg).expect("drawn");
        assert!(drawn.start.abs_diff(X1.x(LEFT_PAD) as usize) <= 2);
        let inked = drawn.len() as f32;
        assert!(
            inked <= wide + 1.0 && inked >= wide - 4.0,
            "{drawn:?}: {wide}"
        );
        assert!(
            (drawn.len() as f32) < hint_text(111).chars().count() as f32 * 9.0 * 0.75,
            "CONTROL: under the cells' run"
        );
        assert!(
            (0..999).all(|x| pixel(hints, x as f32, 22.0) == p.seam),
            "the seam on the band's last pixel row"
        );
    }

    /// THE QUERY BOX AND THE TOGGLES ARE FLUENT CONTROLS ([`fluent_bar`]): at
    #[test]
    fn the_box_and_the_toggles_are_inset_rounded_controls() {
        if !FIND_TOOLBAR {
            return;
        }
        if !with_ui_faces() {
            aterm_spec::host::could_not_run("no system UI face here for the strip band");
        }
        let c = chrome_band::band_colors(Theme::default());
        let rest = toggle_rest_fill(&c);
        let v = FindBarView {
            idx: 2,
            total: 18,
            ..view("Merge")
        };
        for g in [X1, X2] {
            let (p, bar) = fluent(&v, g, Theme::default(), None);
            let field = &bar.rows[p.field_row];
            let (y0, y1) = g.control_y();
            assert_eq!(y0, 3.0 * g.scale, "{g:?}: 3 epx off the seam");
            assert_eq!(g.cell_h as f32 - y1, y0, "{g:?}: as far off the foot");
            let r = g.epx(CONTROL_RADIUS_EPX);
            assert_eq!(r, 4.0 * g.scale, "{g:?}: 4 epx corners");
            let well = well_box(&p.field_cols, g);
            let bx = well.x1 - g.epx(BOX_PAD_EPX) - 2.0;
            assert_eq!(
                pixel(field, bx, y0 - 1.0),
                c.bar_bg,
                "{g:?}: band over the box"
            );
            assert_eq!(pixel(field, bx, y0), c.field_bg, "{g:?}: the box");
            assert_eq!(
                pixel(field, bx, y1 - 1.0),
                c.accent,
                "{g:?}: its focus line"
            );
            assert_eq!(pixel(field, bx, y1), c.bar_bg, "{g:?}: band under it");
            assert_ne!(
                pixel(field, well.x0, y0),
                c.field_bg,
                "{g:?}: a round corner"
            );
            assert_eq!(pixel(field, well.x0 + r, y0 + r), c.field_bg);
            assert_ne!(
                pixel(field, well.x0, y1 - 1.0),
                c.accent,
                "{g:?}: line in corner"
            );
            let mid = ((y0 + y1) / 2.0).floor();
            for span in [p.case_cols.clone(), p.regex_cols.clone()] {
                let span = span.expect("the toggles are drawn");
                let xa = g.x(span.start) + g.epx(CHIP_SIDE_EPX);
                assert_eq!(pixel(field, xa + r, y0 - 1.0), c.bar_bg, "{g:?}: band over");
                assert_eq!(pixel(field, xa + r, y0), rest, "{g:?}: the chip's top");
                assert_eq!(pixel(field, xa + r, y1 - 1.0), rest, "{g:?}: its foot");
                assert_eq!(pixel(field, xa + r, y1), c.bar_bg, "{g:?}: band under");
                assert_eq!(pixel(field, xa, mid), rest, "{g:?}: its side");
                assert_ne!(pixel(field, xa, y0), rest, "{g:?}: a round corner");
                assert_eq!(
                    p.rows[p.field_row][span.start].bg, rest,
                    "CONTROL: the cell under it fills the row"
                );
            }
        }
    }

    /// AN ON TOGGLE IS DRAWN IN THE ACCENT ([`fluent_bar`]): its chip is the
    #[test]
    fn an_on_toggle_is_drawn_in_the_accent() {
        use aterm_messages::ink::contrast;
        if !FIND_TOOLBAR {
            return;
        }
        if !with_ui_faces() {
            aterm_spec::host::could_not_run("no system UI face here for the strip band");
        }
        let light = Theme::from(&aterm_types::scheme::builtin("Default Light").expect("built-in"));
        for theme in [Theme::default(), light] {
            let c = chrome_band::band_colors(theme);
            let v = FindBarView {
                case_sensitive: true,
                idx: 1,
                total: 3,
                ..view("Merge")
            };
            let (p, bar) = fluent(&v, X1, theme, None);
            let field = &bar.rows[p.field_row];
            let (y0, y1) = X1.control_y();
            let mid = ((y0 + y1) / 2.0).floor();
            let (on_fill, on_ink) = toggle_on_colors(&c);
            let case = p.case_cols.clone().expect("drawn");
            let (xa, xb) = (X1.x(case.start) + 1.0, X1.x(case.end) - 1.0);
            assert_eq!(pixel(field, xa, mid), on_fill);
            assert_eq!(on_fill, c.accent);
            let word = (y0 as usize..y1 as usize)
                .flat_map(|y| (xa as usize..xb as usize).map(move |x| (x, y)))
                .map(|(x, y)| pixel(field, x as f32, y as f32))
                .max_by(|a, b| contrast(*a, on_fill).total_cmp(&contrast(*b, on_fill)))
                .expect("pixels");
            assert!(
                contrast(word, on_fill) >= 3.0,
                "{word:?} reads on the accent"
            );
            assert!(
                contrast(word, on_ink) < 1.6,
                "{word:?} is the on-accent ink"
            );
            let regex = p.regex_cols.clone().expect("drawn");
            assert_eq!(
                pixel(field, X1.x(regex.start) + 1.0, mid),
                toggle_rest_fill(&c),
                "CONTROL: the off chip rests"
            );
        }
    }

    /// THE QUERY BOX SCROLLS AND HIT-TESTS IN THE FACE IT DRAWS ([`well_scroll`],
    #[test]
    fn the_query_box_scrolls_and_hit_tests_in_its_own_face() {
        if !FIND_TOOLBAR {
            return;
        }
        if !with_ui_faces() {
            aterm_spec::host::could_not_run("no system UI face here for the strip band");
        }
        let px = X1.body().get();
        let caret_w = X1.caret_w();
        let v = FindBarView {
            idx: 1,
            total: 3,
            ..view("Merge")
        };
        let (p, bar) = fluent(&v, X1, Theme::default(), None);
        let well = well_box(&p.field_cols, X1);
        let chars: Vec<char> = "Merge".chars().collect();
        assert_eq!(bar.scroll, 0);
        assert_eq!(bar.caret_x, (well.text_x0 + ui_width(&chars, px)).round());
        for i in 0..=chars.len() {
            let x = ui_width(&chars[..i], px);
            assert_eq!(char_at(&chars, 0, x, 400.0, px), i, "boundary {i}");
        }
        assert_eq!(
            char_at(&chars, 0, 1000.0, 400.0, px),
            chars.len(),
            "past: the end"
        );
        assert_eq!(char_at(&chars, 0, -3.0, 400.0, px), 0);
        let long: Vec<char> = "search the screen and the scrollback "
            .repeat(8)
            .chars()
            .collect();
        let room = 120.0;
        let s = well_scroll(&long, long.len(), room, caret_w, px);
        assert!(s > 0);
        assert!(
            ui_width(&long[s..], px) + caret_w <= room,
            "the caret is in"
        );
        assert!(
            ui_width(&long[s - 1..], px) + caret_w > room,
            "by the least scroll"
        );
        assert_eq!(
            well_scroll(&long, 0, room, caret_w, px),
            0,
            "Home: the head"
        );
        assert!(
            well.x0 < X1.x(p.field_cols.start),
            "it starts in the prompt's cells"
        );
        let at = |x: f32| well_click("Merge", 0, &p.field_cols, X1, x);
        assert_eq!(at(well.text_x0 + ui_width(&chars[..3], px)), Some(3));
        assert_eq!(at(well.x0 + 1.0), Some(0));
        assert_eq!(at(well.x0 - 1.0), None, "the label is not the box");
        assert_eq!(at(well.x1 + 1.0), None);
        let target = "target here";
        let x = X1.x(p.field_cols.start + 6) + 4.0;
        let by_face = well_click(target, 0, &p.field_cols, X1, x).expect("in the box");
        let tc: Vec<char> = target.chars().collect();
        let want = char_at(&tc, 0, x - well.text_x0, 400.0, px);
        assert_eq!(by_face, want);
        assert_ne!(by_face, 6, "CONTROL: one character per cell is elsewhere");
    }

    /// THE COMPOSITION IS UNDERLINED UNDER ITS OWN CHARACTERS IN THE BOX
    #[test]
    fn the_composition_is_underlined_under_its_own_characters() {
        if !FIND_TOOLBAR {
            return;
        }
        if !with_ui_faces() {
            aterm_spec::host::could_not_run("no system UI face here for the strip band");
        }
        let c = chrome_band::band_colors(Theme::default());
        let v = FindBarView {
            idx: 1,
            total: 3,
            ..view("abcd")
        };
        let (p, composed) = fluent(&v, X1, Theme::default(), Some(2..4));
        let (_, plain) = fluent(&v, X1, Theme::default(), None);
        let (a, b) = (&composed.rows[p.field_row], &plain.rows[p.field_row]);
        let changed: Vec<(usize, usize)> = (0..23)
            .flat_map(|y| (0..999).map(move |x| (x, y)))
            .filter(|&(x, y)| pixel(a, x as f32, y as f32) != pixel(b, x as f32, y as f32))
            .collect();
        assert!(!changed.is_empty(), "the composition is underlined");
        let well = well_box(&p.field_cols, X1);
        let chars: Vec<char> = "abcd".chars().collect();
        let px = X1.body().get();
        let from = (well.text_x0 + ui_width(&chars[..2], px)).round() as usize;
        let to = (well.text_x0 + ui_width(&chars, px)).round() as usize;
        for &(x, y) in &changed {
            assert!((from..to + 1).contains(&x), "x {x} outside {from}..{to}");
            assert_eq!(pixel(a, x as f32, y as f32), c.value);
        }
    }

    /// UNDER A FORCED PALETTE THE BOX KEEPS AN EDGE ([`fluent_bar`]): every stock
    #[test]
    fn the_box_keeps_an_edge_under_a_forced_palette() {
        use aterm_messages::ink::contrast;
        if !FIND_TOOLBAR {
            return;
        }
        if !with_ui_faces() {
            aterm_spec::host::could_not_run("no system UI face here for the strip band");
        }
        for (name, palette) in chrome_band::hc_fixtures::STOCK {
            chrome_band::hc_fixtures::with_forced(palette, || {
                let c = chrome_band::band_colors(Theme::default());
                let v = FindBarView {
                    case_sensitive: true,
                    idx: 1,
                    total: 3,
                    ..view("Merge")
                };
                let (p, bar) = fluent(&v, X1, Theme::default(), None);
                assert_eq!(p.well_line, None, "{name}");
                let field = &bar.rows[p.field_row];
                let (y0, y1) = X1.control_y();
                let well = well_box(&p.field_cols, X1);
                let bx = well.x1 - 12.0;
                let mid = ((y0 + y1) / 2.0).floor();
                assert_eq!(pixel(field, bx, mid), c.field_bg, "{name}: inside");
                for y in [y0, y1 - 1.0] {
                    let edge = pixel(field, bx, y);
                    assert!(
                        contrast(edge, c.field_bg) >= 3.0,
                        "{name}: the border at {y}"
                    );
                    assert_ne!(edge, palette.highlight, "{name}: no focus line");
                }
                let case = p.case_cols.clone().expect("drawn");
                assert_eq!(
                    pixel(field, X1.x(case.start) + 1.0, mid),
                    palette.highlight,
                    "{name}"
                );
            });
        }
    }

    /// THE CELLS STAND where no UI face is installed, for a query no face draws
    #[test]
    fn the_cells_stand_without_a_face_for_the_query() {
        let bar = |query: &str| {
            let v = view(query);
            let paint = find_bar_paint(&v, 111, FIND_BAR_ROWS, Theme::default(), FindSeam::Top);
            fluent_bar(&FluentInput {
                view: &v,
                paint: &paint,
                colors: chrome_band::band_colors(Theme::default()),
                geom: X1,
                seam: FindSeam::Top,
                preedit: None,
            })
            .is_some()
        };
        tray_raster::clear_ui_fonts_for_test();
        assert!(!bar("Merge"), "no UI face: the cells");
        if with_ui_faces() {
            assert_eq!(bar("Merge"), FIND_TOOLBAR, "the face on Windows only");
            assert!(!bar("Merge \u{1F642}"), "a colour emoji keeps the cells");
        }
    }
}
