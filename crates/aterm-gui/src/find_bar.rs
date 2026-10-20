// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Cmd-F FIND PANEL: a pinned chrome BAND painted over the terminal grid
//! (directly below the tab strip, or in a split over the focused pane's top rows)
//! while find mode is active (`WindowState.search`).
//! Before this the find state was surfaced ONLY in the window title + the
//! current-match selection highlight (the FIND-1 readiness finding), so
//! pressing ⌘F looked like it did nothing — the search engine was wired, but
//! INVISIBLE. Then it was one cramped row, which read as terminal output rather than
//! as UI. It is now a [`FIND_BAR_ROWS`]-row panel:
//!
//! ```text
//!                                                                  (pad, not on Windows)
//!   Find: needle                          2/7      [Aa] [.*]       (field)
//!   ⌘S next  ⌘R prev  ⌥⌘C case  ⌥⌘R regex  ⏎ accept  esc cancel     (hints)
//! ```
//!
//! Its separator rule runs along the band's content-facing pixel EDGE: under the
//! hints row the splice draws it as a one-pixel rail on the band's last pixel row
//! ([`FindSeam::BottomRail`], the message band's rail), so it never crosses a
//! descender; a cell too short to keep its words off a rail falls back to the face's
//! underline ([`FindSeam::BottomUnderline`]).
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
//! that is where the eye already is.
//!
//! Windows sets the field row as a toolbar ([`FIND_TOOLBAR`]): the well with
//! padded ` Aa ` / ` .* ` toggle buttons right after it, each on a resting
//! control fill, and at the band's far end the count with clickable `↑` / `↓` /
//! `✕` buttons (Shift+F3, F3, Esc), `✕` last, as the find bars of Windows
//! Terminal and VS Code close. The well gives up width before the toggles do,
//! so a split pane keeps them:
//!
//! ```text
//!   Find: Merge▮______________  Aa   .*                  2/39  ↑  ↓  ✕
//!   F3/Shift+F3 next/prev  Enter accept  Esc cancel
//! ```
//!
//! (` Aa ` and ` .* ` are the two filled chips.)
//!
//! On Windows the bar's pixels are its Fluent face ([`fluent_bar`]), laid over
//! these cells the way the tab strip and the pane headers lay theirs: the words
//! in the system UI face, the query box and the toggles as inset, rounded
//! controls, an on toggle in the accent. The cells keep every character for the
//! text grid and stand in wherever no UI face is installed.
//!
//! A count is only ever painted bare when it is an exact census of the present
//! screen. Two qualifiers say otherwise and survive at EVERY width — the ladder
//! shortens by dropping the `i/` position, never the marker:
//!
//! * `+` — the total is a floor. Where the status zone is wide enough the bar
//!   names why: the depth cap (`none in newest 100,000 lines`, narrower
//!   `none in newest 100k`; `3/7+ (newest 100,000 lines)` for hits; the limit
//!   is `search_history_lines`), the match limit (`1/100000+ (match limit)`),
//!   or a rewrap (below); a floor with none of these still reads
//!   `(partial history)`.
//! * `…` — the terminal changed since the count was taken (output arrived, the
//!   screen repainted). The search is deliberately not re-run per PTY batch, so
//!   the number is a past census and can be wrong in EITHER direction; it
//!   refreshes on the next edit or ⌘S/⌘R. Spelled out as `(stale)`.
//!
//! While a width change's rewrap holds the history AWAY (design ruling 237), a
//! zero-match is not a miss — the text may be there, and the search runs again by
//! itself at the re-attach — so no width says `no matches`: the ladder is `none
//! yet, history rewrapping` › `none yet (rewrapping)` › `rewrapping history` ›
//! `rewrapping` › `rewrap`, in the label tone, and the query is not tinted as a
//! failure. Below 49 columns the status zone is dropped for the field (for every
//! result), and the band's `Rewrapping scrollback` row carries the wait.
//!
//! Pure + themed like the paste banner and the link caption (it reuses
//! [`crate::settings::blank_row`]/[`crate::settings::write_str`] +
//! [`crate::chrome_band::band_colors`]), so the band builder unit-tests with no window
//! and is drawn by `App::splice_find_bar` (app_render.rs), which OVERWRITES its rows
//! in place — the top of the grid normally, or the BOTTOM when the current match
//! would otherwise sit under the panel (adaptive placement, so the match is never
//! hidden).

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

/// Rows the find panel occupies: a blank pad, the field row, the hints row. The
/// splice shrinks the band on a terminal too short to hold it (see
/// [`find_bar_paint`]).
///
/// Windows drops the pad: there the panel is the compact two-line find box a
/// Windows Terminal or VS Code user knows, and the pad read as an empty row
/// under the tab strip (the 2026-10-17 audit). Its rows are the degraded 2-row
/// panel this builder always supported.
pub(crate) const FIND_BAR_ROWS: usize = if cfg!(windows) { 2 } else { 3 };

/// Where the panel's separator rule runs: the edge that faces the terminal content.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum FindSeam {
    /// The panel floated to the BOTTOM (content above): an overline on its first
    /// row, which the renderer draws on the row's top pixel row, the band's edge.
    Top,
    /// The panel at the TOP (content below), its rule drawn by the splice as a
    /// one-pixel rail on the band's last pixel row (a `ChromeRaster`, the message
    /// band's seam), clear of every descender. The builder leaves the rows bare.
    BottomRail,
    /// The panel at the top on a cell too short to keep its words off a rail
    /// (`aterm_render::band::seam_can_rail`): the face's underline on the last row.
    BottomUnderline,
}

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
    /// The count is a floor (`+`): older history went unsearched, the list hit the
    /// match limit, or a rewrap held the history away. A zero-match is then never a
    /// bare `no matches`.
    pub truncated: bool,
    /// How many of the newest lines the depth cap let the search read; `None` when
    /// it read the whole buffer. A zero-match reads `no matches in newest 100,000
    /// lines` where it fits.
    pub searched_lines: Option<usize>,
    /// The list stopped at the match limit: `1/100000+ (match limit)`.
    pub match_capped: bool,
    /// A regex scan could not finish some line within its budget, so the rest
    /// of that line went unsearched: `3/7+ (long line partly searched)`.
    pub cut_short: bool,
    /// The terminal changed since `total` was counted — new output, a repaint, a
    /// `clear` — and the search is deliberately NOT re-run per PTY batch (that is
    /// one full scan per burst on the UI thread). The count is therefore a past
    /// census of a screen that has since moved, so the bar marks it `…` instead
    /// of asserting `1/6` at a window that now holds eight hits.
    ///
    /// Distinct from [`Self::truncated`] on purpose: `+` says "at least this
    /// many", a floor; `…` says "this many, as of before the last output" and
    /// can be wrong in EITHER direction. Both can hold at once (`1/6+…`).
    pub stale: bool,
    /// The search ran while a rewrap held the session's history detached (design
    /// ruling 237). The text may be there; the history is away. So a zero-match
    /// is `none yet`/`rewrapping`, never `no matches`, at EVERY width, and the
    /// query is not tinted as a failure. Implies [`Self::truncated`].
    pub history_away: bool,
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
    /// Cell-column spans of the `↑` (previous match), `↓` (next match) and `✕`
    /// (close, as Esc) buttons ([`FIND_TOOLBAR`]), or `None` off Windows and when
    /// the row was too narrow to draw them.
    pub prev_cols: Option<Range<usize>>,
    pub next_cols: Option<Range<usize>>,
    pub close_cols: Option<Range<usize>>,
    /// Columns of the input well on the field row.
    pub field_cols: Range<usize>,
    /// Character index of the query shown in the well's FIRST cell — the horizontal
    /// scroll offset. `field_cols.start + (i − field_scroll)` is where character `i`
    /// landed, so a click maps back to an edit position with no re-layout.
    pub field_scroll: usize,
    /// The separator rule's tone, which the splice paints a [`FindSeam::BottomRail`]
    /// in, so the rail and the cell-drawn seams cannot disagree.
    pub seam: [u8; 3],
    /// The accent line a focused Fluent text box closes on, which the splice lays
    /// along the bottom of the well ([`WELL_ACCENT_LINE`]): the band's accent, or
    /// `None` off Windows and under an OS-forced palette (whose well carries its
    /// own border, [`BandColors::well_rule`]).
    pub well_line: Option<[u8; 3]>,
    /// Cell columns of the status readout on the field row (`2/7`, `no
    /// matches`), or `None` when none was painted.
    pub status_cols: Option<Range<usize>>,
    /// Index within [`Self::rows`] of the key-hint row, or `None` on a 1-row
    /// panel.
    pub hint_row: Option<usize>,
}

/// Windows closes the query well on a 2 px accent line along its bottom edge, the
/// focus line of a WinUI TextBox, so the field reads as the input it is (the
/// 2026-10-17 audit: a square box with no stroke and no focus line). macOS and
/// Linux keep the plain inset.
pub(crate) const WELL_ACCENT_LINE: bool = cfg!(windows);

/// Windows sets the field row as the find toolbar a Windows Terminal or VS Code
/// user knows: the case and regex toggles as two padded `Aa` / `.*` buttons
/// right after the well, a cell apart, each on the band's resting control fill
/// ([`BandColors::chip_ground`], the message band's resting chip) so an OFF
/// toggle still reads as a button, an ON one in the accent
/// ([`toggle_on_colors`]); and at the row's far end, against the right
/// margin, the count and clickable `↑` / `↓` / `✕` (previous, next, close)
/// buttons, `✕` last. Packed into one cluster from the well, a full-width pane
/// left the group floating at four-fifths of the band with the `✕` short of the
/// end (the 2026-10-18 re-judge's `find-toolbar-floats-mid-band`). The well
/// gives up width ([`TOOLBAR_MIN_FIELD`]) before the toggles do, so a split pane
/// keeps them (at 55 columns the bracketed `[Aa] [.*]` were dropped for the
/// count, the 2026-10-18 judge's `find-bar-still-cell-band`); the buttons go
/// first, then the toggles, then the count. The caret wears the chrome accent
/// the well's focus line does ([`caret_ink`]). macOS and Linux keep the
/// bracketed toggles at the row's right edge and the cursor-coloured caret.
pub(crate) const FIND_TOOLBAR: bool = cfg!(windows);

/// The toolbar's well never shrinks below this many cells ([`FIND_TOOLBAR`]):
/// about the width of Windows Terminal's search box, and what a 55-column split
/// pane has left beside the toggles, the count and the buttons.
const TOOLBAR_MIN_FIELD: usize = 15;

/// …and never grows past this: a find box, not a second command line. A
/// full-width pane's surplus is band between the toggles and the count.
const TOOLBAR_MAX_FIELD: usize = 40;

/// One toolbar toggle, ` Aa ` or ` .* `: its glyphs padded by a cell each side,
/// so the resting fill and the ON fill are a chip, not a glyph-tight block.
const TOGGLE_W: usize = 4;

/// Both toolbar toggles and the band cell between them, so two resting chips
/// read as two buttons, not one bar.
const TOGGLES_W: usize = 2 * TOGGLE_W + 1;

/// One toolbar button, ` ↑ `: a glyph padded to a three-cell click target.
const BUTTON_W: usize = 3;

/// The toolbar buttons' glyphs, in paint order: previous, next, close.
const TOOLBAR_BUTTONS: [char; 3] = ['\u{2191}', '\u{2193}', '\u{2715}'];

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
    // The qualifiers, in one place because every form below carries them and
    // NO form is allowed to drop them to fit: a shortened count that has quietly
    // shed its `+` or `…` is a bare assertion the bar knows is not true. The
    // ladder therefore shortens the WORDS first (`100,000` to `100k`, then the
    // marks alone), then drops the `i/` position (which the user can still read
    // off the highlight), and never the marker.
    //
    // `+`: the count is a floor — the depth cap stopped the search (`newest
    // 100,000 lines`), the list hit the match limit, a regex could not finish a
    // long line (`long line partly searched`), or a rewrap held the history
    // away. `…`: the terminal has changed since `total` was counted, so it is a
    // past census and can be off in either direction.
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
        // RULING 237: the history is away for a rewrap, so nothing found is NOT
        // a miss — the search runs again by itself at the re-attach. No form
        // says `no matches`; the plain word `rewrapping` survives down to the
        // narrowest status zone that shows anything, and the long form where it
        // fits. Not the warn tone: nothing failed. (No `…`: that mark is the
        // bar's word for "stale", and a result that re-runs itself is moot.)
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
            // A regex could not finish a long line: the miss is not a census,
            // and the bar's zone has room to say why.
            forms.push(format!("no matches ({word}){stale_mark}"));
        }
        if let (Some(long), Some(short)) = (facts.depth(false), facts.depth(true)) {
            // The depth cap stopped it: how far it searched outlives every
            // other word, down to the bare mark.
            let short_count = short.trim_end_matches(" lines");
            forms.push(format!("none in {long}{stale_mark}"));
            forms.push(format!("none in {short}{stale_mark}"));
            forms.push(format!("none in {short_count}{stale_mark}"));
        }
        forms.push(format!("no matches{mark}"));
        forms.push(format!("0 hits{mark}"));
        (forms, c.warn, false)
    } else {
        // Hits: the cause where it fits (`match limit`, `newest 100,000 lines`,
        // `rewrapping`, `stale`), then the marks alone.
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

/// The `[Aa]` / `[.*]` toggle indicators. Bracketed so they read as CONTROLS, and
/// REVERSED (ink and ground swapped) when active — a state you can see across the
/// window, at a width that never changes as it flips, so nothing reflows on a toggle.
fn indicator_segs(v: &FindBarView, c: &BandColors) -> Vec<Seg> {
    let toggle = |text: &str, on: bool| {
        let mut s = seg(text, if on { c.bar_bg } else { c.label }, on);
        s.bg = on.then_some(c.value);
        s
    };
    if FIND_TOOLBAR {
        // Padded chips a cell apart ([`TOGGLES_W`]): OFF on the resting control
        // fill ([`toggle_rest_fill`]) in the band's words, floored to AA there
        // (Nord's words sit at 4.1:1 on its chip), ON in the accent with the
        // on-accent ink ([`toggle_on_colors`]).
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
/// [`BUTTON_W`] cells, in the band's word ink: they are live controls, like the
/// `Find:` prompt, where an inactive toggle is quiet.
fn button_segs(c: &BandColors) -> Vec<Seg> {
    TOOLBAR_BUTTONS
        .iter()
        .map(|glyph| seg(format!(" {glyph} "), c.value, false))
        .collect()
}

/// The well's caret ink: on Windows ([`FIND_TOOLBAR`]) the band's accent, the
/// colour of the well's focus line ([`WELL_ACCENT_LINE`]), floored to AA in the
/// well, so caret and focus line are one mark in the user's accent where the
/// caret was the terminal cursor's green whatever the accent; elsewhere, and
/// under an OS-forced palette (whose well carries a border, not a focus line),
/// the band's own caret ink ([`BandColors::caret`]).
fn caret_ink(c: &BandColors) -> [u8; 3] {
    if FIND_TOOLBAR && c.well_rule.is_none() {
        aterm_messages::ink::ensure_contrast(c.accent, c.field_bg, WELL_INK)
    } else {
        c.caret
    }
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
///
/// Windows teaches F3 / Shift+F3, the find-next reflex of every Windows app (the
/// find field answers them, `App::search_repeat_action`; Ctrl+S / Ctrl+R still
/// work), and spells Enter and Esc as the keys are labelled there.
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

/// Build the find panel: `height` rows, each exactly `cols` cells wide (so the splice
/// overwrites whole grid rows in place). `height` is normally [`FIND_BAR_ROWS`]; a
/// terminal too short degrades gracefully — 2 rows drop the pad, 1 row keeps only the
/// field. Pure + width-clamped: an over-long query, status or hint is truncated, never
/// panics, and every returned row is exactly `cols` long.
///
/// `seam` places the thin separator rule on the edge that faces the terminal content
/// ([`FindSeam`]): along the band's bottom edge when the panel sits at the TOP (content
/// below — the normal placement), drawn by the splice as a rail
/// ([`FindSeam::BottomRail`], the builder leaves the rows bare) or, on a cell too short
/// for one, as an UNDERLINE under the LAST row; or an OVERLINE on the FIRST row when
/// adaptive placement floats it to the BOTTOM (content above — see
/// `App::splice_find_bar`, which flips placement so the current match is never hidden).
/// A cell-drawn rule is applied AFTER the text, so it runs edge to edge instead of
/// breaking into stubs wherever a glyph landed.
pub(crate) fn find_bar_paint(
    v: &FindBarView,
    cols: usize,
    height: usize,
    palette: impl Into<chrome_band::ChromePalette>,
    seam: FindSeam,
) -> FindBarPaint {
    let c = chrome_band::band_colors(palette);
    let height = height.max(1);
    // 3 rows: pad / field / hints. 2: field / hints. 1: field only — the pad is the
    // first thing to go, the hints the second, the field never.
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

    // ---- field row: `Find: ` + the well, then the status/indicators (and, on
    // Windows, the buttons).
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
    // `[Aa]` and `[.*]` (off Windows), or the toolbar's padded chips, are 4 cells
    // each, separated by one: each whole chip is the click target, so a pointer
    // aimed at a bracket or a chip's padding is not a dead pixel.
    let (case_cols, regex_cols) = match layout.indicators.as_ref() {
        Some((start, _)) => (
            Some(*start..start + TOGGLE_W),
            Some(start + TOGGLE_W + 1..start + 2 * TOGGLE_W + 1),
        ),
        None => (None, None),
    };
    // The buttons abut too, each a glyph padded to `BUTTON_W` cells.
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
    // ONE TONE as well as one run. The two branches used to be asymmetric — the
    // underline named its colour, the overline did not — and an uncoloured rule falls
    // back to each cell's own ink. At the full height that was invisible, because the
    // top row is the blank pad and every cell of it is already `label`. It stops being
    // invisible on the DEGRADED panel this builder deliberately supports: at 1 or 2
    // rows the pad is the first thing dropped, so `seam_row` IS the field row, whose
    // inks vary by construction (the bold `Find:` prompt, the well's query, `warn`
    // across a failing one, and the reverse-video caret whose fg is the well's
    // background). The rule would brighten under the prompt, turn red across a bad
    // query, and go missing at the caret.
    //
    // A rail is the splice's to draw: a pixel row is not a cell attribute, and the
    // underline it replaces ran through the hints' descenders five pixels above the
    // band's real edge.
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

/// The CURRENT match's fill: the chrome accent, solid — the colour every other
/// match's tint is stepped toward ([`aterm_messages::tokens::ChromeTokens::search_match`]
/// seeds on it) — held [`CURRENT_RISE`]:1 off the background and [`CURRENT_STEP`]:1
/// off `others`, that tint. The current match used to be the selection colour,
/// which the tint is kept apart from by hue alone: 1.11:1 in the default dark theme
/// (the 2026-10-17 audit), so on a screen of hits the one Enter accepts did not
/// stand out. A step in LUMINANCE does, in every theme. The splice paints it as the
/// live selection's colour while the bar is open, its text in
/// [`current_match_ink`].
///
/// An accent that falls short walks toward the pole away from the background (the
/// tint sits on the background's side of it, so both ratios grow along the walk)
/// until it clears both floors; the pole itself always does.
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

/// The CURRENT match's text ink on its `fill` ([`current_match_fill`]): the
/// background the fill was walked away from — the side the fill stands off, as a
/// block cursor's glyph inverts to the background — walked on toward its pole
/// until it reads at [`CURRENT_INK`], or as far as a pole reaches where none
/// does (always past AA: black or white clears 4.58:1 on any fill). The default
/// theme's `#111318` stays itself on its bright green (13.5:1); Default Light's
/// near-white goes to white on its dark green (5.1:1). The splice paints the
/// selection's text in it. It used to be the renderer's automatic floor on each
/// cell's own ink, which stops at the first tenth-step that clears AA: the
/// default theme's `#d0d0d0` text landed on a washed-out `#535353` over the
/// green (the 2026-10-18 judge).
pub(crate) fn current_match_ink(
    tokens: &aterm_messages::tokens::ChromeTokens,
    fill: [u8; 3],
) -> [u8; 3] {
    aterm_messages::ink::ensure_contrast(tokens.bg, fill, CURRENT_INK)
}

/// The contrast the current match's text reaches for on its fill: WCAG AAA (the
/// best a pole reaches where none clears it, never under AA).
const CURRENT_INK: f64 = 7.0;

/// The least contrast the well's caret holds on the well: WCAG AA.
const WELL_INK: f64 = 4.5;

/// The least contrast an off toolbar toggle's glyphs hold on its resting fill
/// ([`FIND_TOOLBAR`]): WCAG AA.
const TOGGLE_INK: f64 = 4.5;

/// The least contrast an off toolbar toggle's resting fill stands from the
/// band ([`toggle_rest_fill`]).
const TOGGLE_REST_STEP: f64 = 1.4;

/// An ON toolbar toggle's fill and ink ([`FIND_TOOLBAR`]): the band's accent
/// (held 3:1 off the band), its ink the band's on-accent floored to
/// [`TOGGLE_INK`] on it, as Windows Terminal's search box checks a toggle
/// (WinUI's `AccentFillColorDefaultBrush` under
/// `TextOnAccentFillColorPrimaryBrush`). It was the band's words reversed,
/// `#2B2D33` on light and `#D0D0D0` on dark, the accent kept for the focus
/// line and the current match (the 2026-10-18 judge's
/// `J4F-2-find-bar-not-fluent`). Under an OS-forced palette, `COLOR_HIGHLIGHT`
/// and `COLOR_HIGHLIGHTTEXT`.
fn toggle_on_colors(c: &BandColors) -> ([u8; 3], [u8; 3]) {
    (
        c.accent,
        chrome_band::ensure_contrast(c.on_accent, c.accent, TOGGLE_INK),
    )
}

/// An off toolbar toggle's resting fill ([`FIND_TOOLBAR`]): the message band's
/// resting chip ([`BandColors::chip_ground`]), stepped on toward the band's
/// words, in 32nds, where it stands less than [`TOGGLE_REST_STEP`]:1 from the
/// band. The chip is a fixed mix toward the ink, which separates less on a
/// light band: Default Light's `#cccdd0` on `#e2e3e5` was 1.24:1 where
/// Default's is 1.56:1 (the 2026-10-18 judge's
/// `light-find-chip-off-state-weak`). A fill, not a rim: Windows Terminal's
/// search box rests its toggles on no fill and no stroke and gives the
/// elevation border to the checked state, so a stroked off chip would borrow
/// the on state's look. Under an OS-forced palette the chip is that palette's
/// own window colour, untouched.
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
/// non-text floor (WCAG 1.4.11) a mark you must find is held to.
const CURRENT_RISE: f64 = 3.0;

/// The least contrast the current match's fill stands from every other match's
/// tint, so the two read as different marks without colour vision.
const CURRENT_STEP: f64 = 2.0;

/// The query's ink: the warn tone across a search that found nothing or a bad
/// regex, else the band's words. A search that ran while the history was away
/// found nothing YET — not a failure, so not the failure tint (ruling 237).
fn query_ink(v: &FindBarView, c: &BandColors) -> [u8; 3] {
    if !v.query.is_empty() && (v.regex_error || (v.total == 0 && !v.history_away)) {
        c.warn
    } else {
        c.value
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
    let ink = query_ink(v, c);
    // The caret can sit one past the last character, so the scrollable extent is
    // `len + 1` even though only `len` cells carry a glyph.
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
    /// `(start_col, segs)` of the toolbar's `↑ ↓ ✕` buttons ([`FIND_TOOLBAR`]),
    /// when they fit.
    buttons: Option<(usize, Vec<Seg>)>,
}

/// The Windows field row ([`FIND_TOOLBAR`]), a toolbar in two groups:
/// `Find: [well] Aa .*` from the left, and `[status zone] ↑ ↓ ✕` against the
/// right margin, the status right-aligned in its zone against the buttons so
/// the count sits by the arrows that step it and `✕` ends the row. The well
/// takes what the two groups leave, up to [`TOOLBAR_MAX_FIELD`]; the rest of a
/// wide row is band between the groups. As in [`field_layout`] every column is
/// a function of `cols` alone, never of the status text, so nothing moves under
/// the caret while typing. Where the row runs short the buttons go first (F3,
/// Shift+F3 and Esc do what they do, and the hint row teaches them), then the
/// toggles; the count outranks both, and the well keeps [`TOOLBAR_MIN_FIELD`]
/// cells.
fn toolbar_layout(v: &FindBarView, c: &BandColors, cols: usize, well_start: usize) -> FieldLayout {
    let zone = status_zone(cols);
    let room = cols.saturating_sub(well_start + RIGHT_PAD);
    let buttons = TOOLBAR_BUTTONS.len() * BUTTON_W;
    // Each rung: (with toggles, with buttons, the least cells right of the well).
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
    // The right group, from the margin in: the buttons, then the status zone.
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

// ---- The Fluent toolbar: the bar's pixels on Windows ----------------------------

/// The window geometry the bar's pixel face is drawn at ([`fluent_bar`]): the
/// cell box in device pixels and the window's scale (1 at 100 %).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct FluentGeom {
    pub cell_w: usize,
    pub cell_h: usize,
    pub scale: f32,
}

/// How far the query box and the toggle chips stand off the field row's top
/// edge (the seam above the band) and off its foot, in epx. They sat flush on
/// the row under the seam, square (the 2026-10-18 judge's
/// `J4F-2-find-bar-not-fluent`: the chips spanned y25-47 under a y24 seam).
const CONTROL_INSET_EPX: f32 = 3.0;

/// The corner radius of the query box and the chips: WinUI's
/// `ControlCornerRadius`.
const CONTROL_RADIUS_EPX: f32 = 4.0;

/// The space between `Find:` and the query box.
const LABEL_GAP_EPX: f32 = 8.0;

/// The query box's text inset, left and right; the `‹` / `›` edge marks sit
/// in it.
const BOX_PAD_EPX: f32 = 8.0;

/// The query box's focus line: a focused WinUI TextBox's bottom border.
const FOCUS_LINE_EPX: f32 = 2.0;

/// A chip's inset inside its cell span, left and right.
const CHIP_SIDE_EPX: f32 = 1.0;

/// The space between two items of the hint row (`F3 next`, `Shift+F3 prev`),
/// which the cells part with two blanks.
const HINT_GAP_EPX: f32 = 14.0;

/// The UI face's descender, in em (Segoe UI's is 0.21): the query's line is
/// centred with its descenders, so a `g` stays off the box's focus line.
const UI_DESCENT_EM: f32 = 0.21;

/// The UI face's cap height, in em (Segoe UI's is 0.7): `↑` `↓` `✕` are
/// centred on the count's capitals beside them.
const UI_CAP_EM: f32 = 0.7;

/// The scalar advance floor required by the Fluent well's bounded scans.
/// A query whose actual font metrics cannot meet it keeps the cell bar.
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
    /// ([`crate::tab_bar::chrome_label_base_px`]).
    fn base_px(self) -> f32 {
        crate::tab_bar::chrome_label_base_px(self.cell_h as f32, self.scale())
    }

    /// The field row's words (`Find:`, the query, the count, `Aa` / `.*`): the
    /// tab title's size.
    fn body(self) -> StepPx {
        TypeStep::Body.px(self.base_px())
    }

    /// The hint row's words: a step below, a pane header's size.
    fn hint(self) -> StepPx {
        TypeStep::Secondary.px(self.base_px())
    }

    /// The controls' top and bottom (exclusive) on the field row, px from its
    /// top: [`CONTROL_INSET_EPX`] off the seam and the foot, given up where the
    /// row is too short for a line of the body size.
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

/// The query box on the field row, px from the panel's left edge: from a gap
/// past `Find:` to the well's last cell, its text inset [`BOX_PAD_EPX`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct WellBox {
    pub x0: f32,
    pub x1: f32,
    pub text_x0: f32,
    pub text_x1: f32,
}

/// The query box over the well's cells `field` (panel columns). It starts
/// [`LABEL_GAP_EPX`] past the label as set in the UI face, never right of the
/// well's first cell, so every cell of the well is inside it; the cells it
/// takes from the prompt are still the box's to a click ([`well_click`]).
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
/// that keeps the caret (before character `cursor`) inside `room` px, so the
/// box shows as much of the query's head as it can, typing past its right edge
/// scrolls the text under a caret that stays put, and Home brings the head
/// back, as the cell well does ([`paint_well`]).
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
/// query drawn from character `scroll`: where a click there puts the caret.
/// Past the text it is the end.
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

/// A click at `x` px from the panel's left edge on the field row, when the bar
/// drew its pixel face for `query` ([`fluent_ready`]): the caret position it
/// asks for, as a character index, or `None` off the query box. `field` is the
/// well's cells (panel columns) and `scroll` the box's scroll as painted
/// ([`FluentBar::scroll`]), the same inputs the paint measured with.
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

/// Whether the bar draws its pixel face for `query`: on Windows
/// ([`FIND_TOOLBAR`]), with a UI face installed that sets the bar's words and
/// every character of the query (the strip band's own test,
/// [`tray_raster::strip_band_run_coverable`]). A query no face draws, a colour
/// emoji say, keeps the cell bar, which draws it through the terminal's cascade,
/// rather than leave a hole in the box. The actual advances must also meet
/// the bounded scan's floor at this geometry's size; a narrow or zero-advance
/// scalar keeps the cell bar instead of truncating the query's visible range.
/// Success carries the font epoch settled before that metric check.
pub(crate) fn fluent_ready(query: &str, g: FluentGeom) -> Option<u64> {
    if !FIND_TOOLBAR
        || !tray_raster::strip_band_run_coverable(PROMPT)
        || (!query.is_empty() && !tray_raster::strip_band_run_coverable(query))
    {
        return None;
    }
    // Settle the same font epoch the raster key will keep before measuring
    // the floor: this call may install a completed semantic fallback.
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
    /// One raster per band row, each the panel's width and a cell tall: the
    /// image the splice lays over that row's cells.
    pub rows: Vec<Arc<ImageData>>,
    /// The query box's horizontal scroll: the first character it shows.
    pub scroll: usize,
    /// The caret's left edge, px from the panel's left edge.
    pub caret_x: f32,
}

/// What [`fluent_bar`] draws from: the painted cells (their spans and words),
/// the view behind them, the band's colours, the window geometry, the seam,
/// and the IME composition's characters in the query, if any.
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

/// The find toolbar's Fluent face on Windows: one raster per band row, laid
/// by the splice over the row's cells (the pane headers' mechanism, which keeps
/// the cells' characters for the text grid). The label, the query, the count
/// and the toggles' `Aa` / `.*` are set in the system UI face at the tab
/// title's size, the hint row a step below, as a pane header is: they were the
/// terminal face, where the strip, the headers and Settings use Segoe UI. The
/// query box and the chips are WinUI controls, rounded [`CONTROL_RADIUS_EPX`]
/// and inset [`CONTROL_INSET_EPX`] off the seam and the row's foot, one height;
/// the box closes on its focus line and carries an I-beam caret in the
/// accent; an on toggle is the accent with the on-accent ink
/// ([`toggle_on_colors`]); `↑` `↓` `✕` are drawn strokes in the strip's close
/// mark's weight.
///
/// The query is set in the UI face, as Windows Terminal's search box sets it (a
/// XAML TextBox in the system face): the terminal face at the cell's size fills
/// its row (Iosevka's ink spans 17 of the 23 rows at 100 %), so it cannot sit
/// in a box inset off the seam, and it stood a third taller than the label
/// beside it. The grid's hits keep the terminal face.
///
/// `None` off Windows, where no UI face is installed or one cannot draw the
/// query ([`fluent_ready`]), and for an empty panel: the cells stand. Cached on
/// everything the pixels depend on, the font epoch included, so an unchanged
/// bar keeps its rasters' identity (the renderer's image cache keys on it).
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
/// the cells.
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
        // The rule on the edge that faces the content, a pixel row of the
        // raster, which owns every pixel of the band (the cells give up their
        // rules under it).
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
/// and a cell tall, drawn over the row's text (z 0).
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
/// buttons. Returns the box's scroll and caret x with the prims.
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
    // One baseline for the row's words, the query's: its line (descenders
    // included) centred in the box above the focus line.
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
    // `Find:`, the band's words, clear of the box.
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

    // The query box: the terminal's ground, rounded, its focus line along the
    // foot inside the corners; under an OS-forced palette a border instead.
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

    // The toggles: rounded chips, one height with the box; off on the resting
    // fill, on in the accent.
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

    // The count, right-aligned where its cells end, by the arrows.
    if let Some((span, words, ink)) = &key.status {
        let w = tray_raster::ui_text_width_for(TextFace::Ui, words, px);
        text(&mut prims, g.x(span.end) - w, baseline, words, *ink);
    }

    // `↑` `↓` `✕`: round-capped strokes in the strip close mark's weight,
    // centred on the line of words they end.
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
            // The shaft, and the head at its tip: up for `↑`, down for `↓`.
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
/// placeholder), the IME composition's underline, the I-beam caret in the
/// accent, and `‹` / `›` where the query runs past an edge. `(top, bottom)` is
/// the box above its focus line. Returns the scroll and the caret's x.
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
        // The placeholder after the caret, where it fits whole.
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
    // The edge marks, in the box's inset, so a clipped query never reads as
    // the whole one.
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
/// row's, in the band's label tone, a [`HINT_GAP_EPX`] apart; an item that
/// would cross the right margin is left out whole.
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
            .find(|&col| row[col].bg == caret_ink(&c))
            .expect("the caret is always inside the well");
        (
            row[p.field_cols.clone()].iter().map(|c| c.ch).collect(),
            caret,
        )
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
        assert_eq!(
            (cell.ch, cell.bg, cell.fg),
            ('b', caret_ink(&c), c.field_bg)
        );
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
    /// cells (brackets included — a click on the bracket is not a dead pixel), or on
    /// Windows the padded ` Aa ` / ` .* ` chips, and the toolbar's buttons with their
    /// padded `↑` / `↓` / `✕`.
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
    /// ornament goes first: the toggles are dropped so the match/no-match readout
    /// survives — a search that found nothing must never look like one that found
    /// seven. (Windows' toolbar gives the well up first, so its toggles last to a
    /// narrower row: [`the_toolbar_keeps_its_toggles_in_a_split_pane`].)
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
        // Wider: both fit.
        let roomy = paint(&view("zzz"), 90);
        assert!(roomy.case_cols.is_some());
        assert!(all(&roomy).contains("no matches"));
        // Narrower still: the field keeps whatever is left rather than vanishing.
        let tiny = paint(&view("q"), 24);
        assert!(!tiny.field_cols.is_empty());
    }

    /// A SPLIT PANE KEEPS THE TOGGLES on Windows ([`FIND_TOOLBAR`]): at the 55
    /// columns half of a 111-column window gives a pane, the bracketed toggles were
    /// dropped for the count (the 2026-10-18 judge). The toolbar's well gives up
    /// width instead, so the toggles, the count and the `↑ ↓ ✕` buttons stand in
    /// that order on disjoint spans, the two toggles a cell apart, and the count
    /// sits by the arrows. NEGATIVE CONTROL: the right-edge layout macOS and Linux
    /// keep drops the toggles at this width.
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
        // A pane too narrow for the buttons keeps its toggles still.
        let p = paint(&v, 45);
        assert!(
            p.close_cols.is_none() && p.case_cols.is_some(),
            "the buttons go first"
        );
    }

    /// THE TOOLBAR ENDS ON ITS CLOSE BUTTON at every width that draws one
    /// ([`FIND_TOOLBAR`]): the count and `↑ ↓ ✕` stand against the right margin,
    /// `✕` last, the count by the arrows, while the well and its toggles keep the
    /// left; a wide pane's surplus is band between the two groups, and the well
    /// stops at [`TOOLBAR_MAX_FIELD`]. Where the buttons have gone the count takes
    /// the margin. NEGATIVE CONTROL: the 2026-10-18 build packed one cluster from
    /// the well, so a full 111-column pane ended it at column 94 of 109, the `✕`
    /// floating at four-fifths of the band (the re-judge's
    /// `find-toolbar-floats-mid-band`); that layout fails the first assertion.
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
        // From the narrowest row that still paints the count.
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
        // A full-width pane leaves its surplus between the groups.
        let wide = paint(&v, 111);
        assert_eq!(wide.field_cols.len(), TOOLBAR_MAX_FIELD);
        let gap = wide.prev_cols.expect("buttons").start - wide.regex_cols.expect("toggles").end;
        assert!(gap > 20, "the band between the groups: {gap} cells");
    }

    /// AN OFF TOGGLE IS STILL A BUTTON ([`FIND_TOOLBAR`]): its chip wears the
    /// band's resting control fill ([`toggle_rest_fill`]), at least
    /// [`TOGGLE_REST_STEP`]:1 off the band, with its glyphs in the band's words at
    /// WCAG AA on it, in every built-in theme, light or dark; an ON toggle
    /// wears the accent ([`toggle_on_colors`]). NEGATIVE CONTROLS: the 2026-10-18 build drew an off
    /// toggle as bare label-ink glyphs on the band, no fill (the re-judge's
    /// `find-toolbar-floats-mid-band`); and the message band's resting chip it
    /// then wore stands 1.24:1 off Default Light's band (the judge's
    /// `light-find-chip-off-state-weak`), where Default keeps it as is.
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
    /// authored blue accent the caret is that blue, floored to AA in the well, as the
    /// well's focus line is the accent. NEGATIVE CONTROL: off Windows the caret stays
    /// the band's cursor-coloured caret.
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
    /// At the top it is the splice's rail by default: the builder then leaves every
    /// cell bare, so no underline crosses the hints' descenders.
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
        // AND IT IS ONE TONE ON THE DEGRADED PANEL. At 1 or 2 rows the pad is dropped,
        // so the seam row IS the multi-ink field row; an uncoloured overline falls back
        // to each cell's own `fg` and the rule would brighten under the prompt, go warn
        // across a failing query and vanish at the reverse-video caret. At the full
        // height the pad row hid that, because every cell of it is already `label`.
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
    /// [`CURRENT_RISE`]:1 off the background and [`CURRENT_STEP`]:1 off the tint every
    /// other match wears — a step in luminance. NEGATIVE CONTROL: the selection colour
    /// it replaced sits under that step from the tint in the default theme (1.11:1).
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
    /// fill stands off, walked toward its pole for AAA, in every built-in theme: at
    /// least AA always, AAA wherever a pole reaches it, the default theme's own
    /// background on its green and white on Default Light's dark green. NEGATIVE
    /// CONTROL: the renderer's automatic floor it replaced settles the default
    /// theme's `#d0d0d0` text on a mid-grey under 6:1 over the bright green fill.
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
        // Off an OS palette the fill carries the edge and nothing is stamped; on
        // Windows the splice closes the well on its accent focus line instead.
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
                // And it must stop AT the well: the band around it is not a field.
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
    /// across the window and nothing reflows when it flips: reversed (ink and
    /// ground swap) off Windows, the accent on Windows ([`toggle_on_colors`]).
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
        // Windows' toolbar rests an inactive toggle on the control fill in the
        // band's words (`an_off_toggle_rests_on_a_control_fill_in_every_builtin_theme`).
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

    /// THE HISTORY IS AWAY, NOT EMPTY (ruling 237): while a rewrap holds the
    /// history detached, a search that found nothing on the screen says so in
    /// words that never read `no matches` — at EVERY width from 40 columns up —
    /// and the query keeps its ordinary ink (nothing failed; the search runs
    /// again at the re-attach). The long form where it fits, the plain word
    /// where it does not; below the narrowest status zone the bar says nothing
    /// rather than the opposite of the truth.
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
        // Every width: never the opposite of the truth.
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
        // The ordinary miss is still a miss, tinted as one.
        let (row, ink) = field_row(&view("needle"), 80);
        assert!(row.contains("no matches"), "{row}");
        assert_eq!(ink, c.warn);
        // Hits on the screen while the rest is away: a floor.
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

    /// A count is painted BARE only when it is an exact census of the present
    /// screen. Output since the count marks it `…`; a capped index marks it `+`;
    /// both mark it `+…`. And no width sheds a marker to fit — the ladder gives
    /// up the `i/` position first, because a shortened `1/6` that has quietly
    /// dropped its qualifier is the exact lie this marker exists to prevent.
    #[test]
    fn a_count_the_screen_has_moved_past_is_never_painted_bare() {
        // The STATUS row only: the key-hint row spells `Ctrl+S`, and a `+` there
        // is not a qualifier.
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
        // Wherever the exact form finds room to state a number, the qualified
        // form finds room to qualify it — the ladder shortens the POSITION, not
        // the marker. (A window too narrow for any status at all paints none;
        // that is the pre-existing "even the shortest form won't fit" arm, and
        // saying nothing is not a false claim.)
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

        // The no-match form carries the same fact.
        let none = FindBarView {
            idx: 1,
            total: 0,
            stale: true,
            ..view("needle")
        };
        let narrow = status_row(&none, 60);
        assert!(narrow.contains('…'), "{narrow}");
    }

    /// Every width × height stays exactly `cols` wide and never panics — including a
    /// query far wider than the row, a zero-width window, and the degraded 1/2-row
    /// bands a very short terminal gets.
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
        // A degraded 1-row band keeps the FIELD (the pad and hints go first).
        let one = find_bar_paint(&view("kept"), 90, 1, Theme::default(), FindSeam::Top);
        assert_eq!(one.field_row, 0);
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

    /// AN ON TOGGLE WEARS THE ACCENT ([`FIND_TOOLBAR`]), as Windows Terminal's
    /// search box checks one: the band's accent under its on-accent ink at WCAG
    /// AA, in every built-in theme, and under every stock High Contrast scheme
    /// `COLOR_HIGHLIGHT` under `COLOR_HIGHLIGHTTEXT`. NEGATIVE CONTROL: the
    /// 2026-10-18 build drew it as the band's words reversed, a `value` fill,
    /// grey where the default themes' accent is green.
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

    // ---- The Fluent face ([`fluent_bar`]) ----

    /// Install this host's UI faces on the test thread; `false` where it has
    /// none (a fontless host has nothing to assert, as the strip band's tests).
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
    /// the count and the hint row are drawn at the UI face's own widths, the
    /// field row at the tab title's size and the hints a step below, each band
    /// row one raster the panel's width over its cells, as the strip and the
    /// pane headers draw theirs. NEGATIVE CONTROL: the cells under them set
    /// `Find:` across five 9 px terminal cells, and the hints one cell a
    /// character, far wider than the face draws them.
    #[test]
    fn the_windows_bar_sets_its_words_in_the_ui_face() {
        if !FIND_TOOLBAR || !with_ui_faces() {
            return;
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
        // Ink sits inside the advance, short of it by the side bearings: a face
        // whose ink runs past the UI face's advance is not the UI face.
        let face = measure("Find:", body);
        let inked = label.len() as f32;
        assert!(
            inked <= face && inked >= face - 4.0,
            "{label:?}: inside the UI face's {face} px advance"
        );
        assert!(label.len() < 5 * 9 * 3 / 4, "CONTROL: under the cells' run");
        // The count ends where its cells do, at the face's width.
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
        // The hints, a step below, their items a gap apart, above the seam.
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
    /// 100 % and 200 % each stands [`CONTROL_INSET_EPX`] off the seam above the
    /// band and as far off the field row's foot, one height, its corners rounded
    /// [`CONTROL_RADIUS_EPX`]; the box closes on its accent focus line inside
    /// its corners. NEGATIVE CONTROL: the cells under them fill the row edge to
    /// edge (the judge's chip spanned y25-47 under a y24 seam, square).
    #[test]
    fn the_box_and_the_toggles_are_inset_rounded_controls() {
        if !FIND_TOOLBAR || !with_ui_faces() {
            return;
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
    /// band's accent with its word in the on-accent ink, dark or light, where
    /// an off chip keeps the resting fill. NEGATIVE CONTROL: the off chip.
    #[test]
    fn an_on_toggle_is_drawn_in_the_accent() {
        use aterm_messages::ink::contrast;
        if !FIND_TOOLBAR || !with_ui_faces() {
            return;
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
    /// [`char_at`], [`well_click`]): the caret stands the query's measured width
    /// in; a query past the box scrolls by the least that keeps the caret in;
    /// a click lands on the nearest character boundary, anywhere in the box,
    /// the prompt's cells it took included. NEGATIVE CONTROL: one character per
    /// terminal cell, the cell well's mapping, puts the same click short.
    #[test]
    fn the_query_box_scrolls_and_hit_tests_in_its_own_face() {
        if !FIND_TOOLBAR || !with_ui_faces() {
            return;
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
        // A long query, the caret at its end: the least scroll that keeps it in.
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
        // Clicks: the box is the box, its inset over the prompt's cells too.
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
    /// ([`fluent_bar`]), in the band's words, and nowhere else. NEGATIVE CONTROL:
    /// with no composition the box carries no underline.
    #[test]
    fn the_composition_is_underlined_under_its_own_characters() {
        if !FIND_TOOLBAR || !with_ui_faces() {
            return;
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
    /// High Contrast scheme sets the field and the band one colour, so the box
    /// carries a border in the window text and no accent focus line, and an on
    /// toggle is `COLOR_HIGHLIGHT`.
    #[test]
    fn the_box_keeps_an_edge_under_a_forced_palette() {
        use aterm_messages::ink::contrast;
        if !FIND_TOOLBAR || !with_ui_faces() {
            return;
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
    /// (a colour emoji), and off Windows ([`fluent_ready`]).
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
