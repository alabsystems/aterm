// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The in-grid tab context menu: the popup a right-click on an in-grid tab chip opens,
//! painted as terminal cells over the frame like the rest of aterm's chrome (strip,
//! find bar, config banner).
//!
//! It renders [`crate::session_chrome::compose_tab_menu`]'s model (identity headers,
//! the timeline tail, then `Rename Session…` / `Copy Session ID` / `Copy CWD` /
//! `Close Tab`) off macOS; macOS renders the same model as an `NSMenu` off the strip's
//! `TabView`.
//!
//! # Why cells and not `TrackPopupMenu`
//!
//! * `TrackPopupMenu` runs its own modal message loop, and every call site is inside
//!   winit's event callback. The vendored runner takes the handler out of a `Cell`
//!   during dispatch and routes `RedrawRequested` unbuffered to `call_event_handler`,
//!   which panics on re-entry
//!   (`crates/aterm-window/src/platform_impl/windows/event_loop/runner.rs`), so the
//!   `WM_PAINT` after a popup would kill the app. (`drag_window()` is safe because
//!   winit posts `WM_NCLBUTTONDOWN` and returns first; a popup at the pointer has no
//!   equally cheap deferral point.)
//! * No `HMENU` FFI exists in `crates/`, and adding one for one gesture is a new
//!   surface.
//! * The top half is a session identity card (emoji icon, description, relative-age
//!   timeline), which reads as a broken menu when rendered as disabled system rows.
//! * Everything around it (band, chip, `✕`, `+`) is themed cells; a system-tinted
//!   popup would be the one visible seam.
//!
//! Costs: Esc/arrows/Enter are implemented here, and there is no UI-Automation menu
//! role (a screen reader sees cells); accessibility parity needs an accessibility node.
//!
//! # Shape
//!
//! Geometry, hit-testing, keyboard stepping and cell paint are pure functions over
//! the model, so the hit-test ↔ paint ↔ dispatch chain is unit-tested headlessly.
//! Opening from a press, blitting into `input_scratch` and posting the chosen action
//! live in `app_mouse.rs` / `app_render.rs` / `app_tabs.rs`.

use aterm_core::terminal::RenderCell;
use aterm_render::Theme;

use crate::chrome_band::{self, BandColors};
use crate::session_chrome::TabMenuEntry;
use crate::tab_model::TabId;

/// Cells of horizontal breathing room inside each vertical border.
const H_PAD: usize = 1;
/// Narrowest the card may be, in CONTENT cells — a menu whose rows read
/// `Copy…`/`Close…` is worse than one that runs a little wide.
const MIN_CONTENT: usize = 16;
/// Widest the card may be, in content cells. Headers (whose description can reach
/// [`crate::session_chrome::DESCRIPTION_DISPLAY_MAX`] graphemes) ellipsize to this.
const MAX_CONTENT: usize = 44;

/// One open tab context menu on one window.
///
/// `tab` is the stable [`TabId`] captured at pop time, never the index (see
/// `Wake::TabMenuAction`): the menu can outlive a background exit or a control-socket
/// `tab move`, and acting on "slot 3 now" would hit a different session. `index` is
/// only the paint anchor (which chip the card hangs under) and may go stale; every
/// action re-resolves through `tab`.
pub(crate) struct TabMenu {
    /// The stable identity of the right-clicked tab — the dispatch subject.
    pub tab: TabId,
    /// The tab's position when the menu popped: the paint anchor only.
    pub index: usize,
    /// The composed model, snapshotted at pop time so the card never reflows under
    /// the pointer; actions resolve their payloads fresh at dispatch.
    pub entries: Vec<TabMenuEntry>,
    /// The strip COLUMN the press landed on — the card's left edge wants to sit
    /// here, before clamping.
    pub anchor_col: u16,
    /// The highlighted row (index into `entries`), or `None` when the pointer is off
    /// every action row. A keyboard open seeds the first enabled action; a mouse open
    /// leaves `None` so Enter cannot hit a row the pointer never chose.
    pub highlight: Option<usize>,
    /// Whether the keyboard owns the highlight: set by a Shift+F10 / Menu key open,
    /// cleared when the pointer enters a row. While set, pointer motion off the card
    /// leaves the highlight alone.
    pub keyboard: bool,
}

impl TabMenu {
    /// `controls tab-menu`: the open card's rows and cursor. Item text is the `chrome`
    /// verb's per-tab mirror ([`crate::session_chrome::menu_entry_mirror`]) verbatim, so
    /// both introspection surfaces describe the menu identically.
    pub(crate) fn controls_lines(&self) -> Vec<String> {
        let mut out = Vec::with_capacity(self.entries.len() + 1);
        out.push(format!(
            "tab-menu tab={} index={} rows={} selected={}",
            self.tab,
            self.index,
            self.entries.len(),
            self.highlight
                .map_or_else(|| "none".to_string(), |i| i.to_string()),
        ));
        for (i, entry) in self.entries.iter().enumerate() {
            out.push(format!(
                "tab-menu item index={i} selected={} text={:?}",
                Some(i) == self.highlight,
                crate::session_chrome::menu_entry_mirror(entry),
            ));
        }
        out
    }
}

/// Where the card landed, in frame cell coordinates (row 0 is the strip's first row).
/// Recorded by the painter and read by the hit-test so both agree, as with
/// `find_bar_hit`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) struct MenuRect {
    pub row: usize,
    pub col: usize,
    pub w: usize,
    pub h: usize,
}

impl MenuRect {
    /// Whether frame cell `(row, col)` is on the card, border included; a press on the
    /// border is swallowed, not passed to the terminal.
    #[must_use]
    pub(crate) fn contains(&self, row: usize, col: usize) -> bool {
        row >= self.row && row < self.row + self.h && col >= self.col && col < self.col + self.w
    }
}

/// Display cells one char occupies, per [`aterm_grapheme::char_width`] clamped to at
/// least one so a combining mark owns a cell (the strip's rule, restated because its
/// helper is private).
fn char_cells(c: char) -> usize {
    aterm_grapheme::char_width(c).max(1) as usize
}

/// Control chars never reach a cell (a header is built from user-authored
/// metadata and an OSC title — both attacker-adjacent strings).
fn safe_char(c: char) -> char {
    if c.is_control() { ' ' } else { c }
}

/// Display cells `s` occupies after projection.
#[must_use]
pub(crate) fn display_cells(s: &str) -> usize {
    s.chars().map(|c| char_cells(safe_char(c))).sum()
}

/// `s` cut to at most `max` DISPLAY cells, ellipsized when it did not fit. Cuts
/// on a whole char (never half a wide glyph) and leaves a cell for the `…`.
#[must_use]
pub(crate) fn ellipsize(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    if display_cells(s) <= max {
        return s.chars().map(safe_char).collect();
    }
    let budget = max.saturating_sub(1);
    let mut out = String::new();
    let mut used = 0usize;
    for c in s.chars().map(safe_char) {
        let w = char_cells(c);
        if used + w > budget {
            break;
        }
        out.push(c);
        used += w;
    }
    out.push('\u{2026}');
    out
}

/// The text one entry contributes to the width measurement (a separator
/// contributes nothing — it is drawn as a rule across whatever width wins).
fn entry_text(entry: &TabMenuEntry) -> &str {
    match entry {
        TabMenuEntry::Header(t) => t.as_str(),
        // The card's rows are aged as it opens (`App::tab_context_menu_model`);
        // unaged, its words alone.
        TabMenuEntry::Event { words, .. } => words,
        TabMenuEntry::Separator => "",
        TabMenuEntry::Action { label, .. } => label,
        TabMenuEntry::ConnectionAction { label, .. } => label,
        TabMenuEntry::Upgrade { label, .. } => label,
    }
}

/// Whether a click / Enter can activate this entry. Headers, separators and disabled
/// actions are inert; disabled actions stay visible so the menu keeps its shape.
#[must_use]
pub(crate) fn is_selectable(entry: &TabMenuEntry) -> bool {
    matches!(
        entry,
        TabMenuEntry::Action { enabled: true, .. }
            | TabMenuEntry::ConnectionAction { enabled: true, .. }
            | TabMenuEntry::Upgrade { .. }
    )
}

/// Content width the card wants: the widest entry, floored at [`MIN_CONTENT`]
/// and capped at [`MAX_CONTENT`].
#[must_use]
pub(crate) fn content_width(entries: &[TabMenuEntry]) -> usize {
    entries
        .iter()
        .map(|e| display_cells(entry_text(e)))
        .max()
        .unwrap_or(0)
        .clamp(MIN_CONTENT, MAX_CONTENT)
}

/// Place the card in frame cell coordinates.
///
/// * `anchor_col`: the strip column pressed; the card's left edge starts there.
/// * `strip_rows`: chrome rows the strip prepended; the card starts right below.
/// * `cols` / `frame_rows`: the composed frame's extent.
///
/// Returns `None` when the frame cannot host a card (narrower than a border pair, or
/// no room below the strip), and the caller does not open.
///
/// The card shifts left to fit the right edge before it shrinks, and shrinks only
/// when even flush-left overflows. Vertically it truncates rather than flipping
/// above the strip.
#[must_use]
pub(crate) fn place(
    entries: &[TabMenuEntry],
    anchor_col: u16,
    cols: usize,
    frame_rows: usize,
    strip_rows: usize,
) -> Option<MenuRect> {
    if entries.is_empty() {
        return None;
    }
    // Borders + padding are fixed overhead; a window that cannot host one
    // content cell hosts no menu.
    let chrome_w = 2 + 2 * H_PAD;
    if cols <= chrome_w || frame_rows <= strip_rows + 2 {
        return None;
    }
    let want_w = (content_width(entries) + chrome_w).min(cols);
    let row = strip_rows;
    let avail_rows = frame_rows - row;
    let h = (entries.len() + 2).min(avail_rows);
    let col = usize::from(anchor_col).min(cols.saturating_sub(want_w));
    Some(MenuRect {
        row,
        col,
        w: want_w,
        h,
    })
}

/// Entry rows the card at `rect` shows (height minus two border rows). A truncated
/// card shows a prefix of the model; see [`entry_at_row`] on why there is no scroller.
#[must_use]
pub(crate) fn visible_entries(rect: MenuRect) -> usize {
    rect.h.saturating_sub(2)
}

/// Which model entry frame `row` shows, or `None` for border rows and rows past the
/// truncation. Inverse of the painter's row walk.
///
/// No scrolling: the model is bounded (identity ≤ 4 lines, timeline ≤
/// [`crate::session_chrome::TIMELINE_TAIL`], four actions), so truncation only happens
/// in a tiny window, where fewer rows beat a scrollbar.
#[must_use]
pub(crate) fn entry_at_row(rect: MenuRect, row: usize) -> Option<usize> {
    if row <= rect.row || row + 1 >= rect.row + rect.h {
        return None;
    }
    let i = row - rect.row - 1;
    (i < visible_entries(rect)).then_some(i)
}

/// The entry a click at frame `(row, col)` activates: `Some(i)` only on a visible,
/// selectable row. Otherwise `None` (the caller still swallows presses on the card;
/// see `MenuRect::contains`).
#[must_use]
pub(crate) fn action_at(
    rect: MenuRect,
    entries: &[TabMenuEntry],
    row: usize,
    col: usize,
) -> Option<usize> {
    if !rect.contains(row, col) {
        return None;
    }
    let i = entry_at_row(rect, row)?;
    entries.get(i).filter(|e| is_selectable(e)).map(|_| i)
}

/// The first selectable entry, for a keyboard open.
#[must_use]
pub(crate) fn first_selectable(entries: &[TabMenuEntry], limit: usize) -> Option<usize> {
    entries.iter().take(limit).position(is_selectable)
}

/// Step the highlight one selectable row in `down`'s direction, wrapping at the
/// ends. `from = None` enters at the first (or last) selectable row. `limit` is
/// [`visible_entries`], so a truncated card never highlights a hidden row.
#[must_use]
pub(crate) fn step_selectable(
    entries: &[TabMenuEntry],
    from: Option<usize>,
    down: bool,
    limit: usize,
) -> Option<usize> {
    let rows: Vec<usize> = entries
        .iter()
        .take(limit)
        .enumerate()
        .filter(|(_, e)| is_selectable(e))
        .map(|(i, _)| i)
        .collect();
    if rows.is_empty() {
        return None;
    }
    let Some(from) = from else {
        return Some(if down { rows[0] } else { rows[rows.len() - 1] });
    };
    let at = rows.iter().position(|&i| i == from);
    Some(match (at, down) {
        (Some(p), true) => rows[(p + 1) % rows.len()],
        (Some(p), false) => rows[(p + rows.len() - 1) % rows.len()],
        (None, true) => rows[0],
        (None, false) => rows[rows.len() - 1],
    })
}

/// What one key press means to an open card (the Win32 menu bindings).
///
/// Shared by the winit route (`App::on_key_tab_menu_mode`) and the engine-neutral
/// seam (`App::tab_menu_input_event`, where `aterm ctl key …` arrives) so a
/// controller and a hand move the same highlight.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MenuNav {
    /// ↵ / Space — run the highlighted row (or dismiss when nothing is lit).
    Activate,
    /// Esc and every key with no menu meaning: dismiss and swallow, so a keystroke
    /// never both closes the card and reaches the shell (there are no mnemonics).
    Dismiss,
    /// ↓ — step the highlight forward over enabled rows, wrapping.
    Down,
    /// ↑ — step it backward.
    Up,
    /// Home — jump to the first enabled row.
    First,
    /// End — jump to the last enabled row.
    Last,
    /// A bare modifier or lock press: swallowed, card stays. Otherwise the Shift half
    /// of Shift+F10 would dismiss the card it re-opens (the same class
    /// `is_bare_modifier_key` guards the rename field from).
    Inert,
}

/// The menu meaning of an engine key. See [`MenuNav`].
///
/// Numpad keys fold onto their navigation twins, so a NumLock-off keypad steps.
#[must_use]
pub(crate) fn nav_for_key(key: &aterm_types::keyboard::Key) -> MenuNav {
    use aterm_types::keyboard::{Key, NamedKey as Nk};
    if aterm_types::keyboard::is_modifier_or_lock_key(key) {
        return MenuNav::Inert;
    }
    match key {
        // Space arrives as `NamedKey::Space` on some layouts and as the
        // one-char `Character(' ')` on others; both are the same press.
        Key::Named(Nk::Enter | Nk::NumpadEnter | Nk::Space) => MenuNav::Activate,
        Key::Character(' ') => MenuNav::Activate,
        Key::Named(Nk::ArrowDown | Nk::NumpadArrowDown) => MenuNav::Down,
        Key::Named(Nk::ArrowUp | Nk::NumpadArrowUp) => MenuNav::Up,
        Key::Named(Nk::Home | Nk::NumpadHome) => MenuNav::First,
        Key::Named(Nk::End | Nk::NumpadEnd) => MenuNav::Last,
        _ => MenuNav::Dismiss,
    }
}

/// The card's paint tones, derived from `chrome_band::band_colors` like the find bar
/// and config banner, one shade further from the terminal background because the
/// card floats over live output.
/// Shared with the Autopilot menu (`crate::autopilot_menu`), the other card
/// drawn over the pane.
pub(crate) struct MenuColors {
    pub(crate) band: BandColors,
    /// The card ground: the band tone pushed further from the terminal bg.
    pub(crate) card_bg: [u8; 3],
    /// Border / separator rule ink.
    pub(crate) rule: [u8; 3],
    /// The highlighted row's ground + ink.
    pub(crate) sel_bg: [u8; 3],
    pub(crate) sel_fg: [u8; 3],
    /// A disabled action's ink, dimmer than a header.
    pub(crate) disabled: [u8; 3],
}

pub(crate) fn menu_colors(palette: impl Into<chrome_band::ChromePalette>) -> MenuColors {
    let palette = palette.into();
    let theme = palette.theme;
    let band = chrome_band::band_colors(palette);
    let fg = [
        ((theme.fg >> 16) & 0xff) as u8,
        ((theme.fg >> 8) & 0xff) as u8,
        (theme.fg & 0xff) as u8,
    ];
    // One more step toward the ink than the band: the find bar sits ON the grid
    // edge and may share its tone; a popup over live text may not.
    let card_bg = chrome_band::mix3(band.bar_bg, fg, 0.08);
    // AA against the CARD, not against the band — the ground moved.
    let value = chrome_band::ensure_contrast(fg, card_bg, 4.5);
    let label = chrome_band::ensure_contrast(band.label, card_bg, 4.5);
    let rule = chrome_band::mix3(card_bg, value, 0.34);
    let sel_bg = chrome_band::mix3(card_bg, fg, 0.22);
    MenuColors {
        card_bg,
        rule,
        sel_fg: chrome_band::ensure_contrast(value, sel_bg, 4.5),
        sel_bg,
        // 3:1 (the WCAG non-text floor), not 4.5:1: a disabled row must read as
        // unavailable yet stay perceivable.
        disabled: chrome_band::ensure_contrast(label, card_bg, 3.0),
        band: BandColors {
            label,
            value,
            ..band
        },
    }
}

/// Write `s` into `row` from `col`, stopping before `end`, emitting lead +
/// continuation pairs for width-2 chars; a wide glyph that would straddle `end` is
/// dropped whole.
pub(crate) fn put_text(
    row: &mut [RenderCell],
    mut col: usize,
    end: usize,
    s: &str,
    fg: [u8; 3],
    bg: [u8; 3],
    bold: bool,
) {
    for c in s.chars().map(safe_char) {
        let w = char_cells(c);
        if col + w > end {
            break;
        }
        if let Some(slot) = row.get_mut(col) {
            *slot = chrome_band::cell(c, fg, bg, bold, false);
        }
        if w == 2
            && let Some(slot) = row.get_mut(col + 1)
        {
            let mut tail = chrome_band::cell(' ', fg, bg, bold, false);
            tail.wide = true;
            *slot = tail;
        }
        col += w;
    }
}

/// Paint the card: exactly `rect.h` rows of `rect.w` cells for the splice to blit at
/// `(rect.row, rect.col)`.
///
/// Box-drawing borders (`┌─┐│└┘├┤`, single-width in every width table used) keep the
/// card distinct from a program's own colour runs.
#[must_use]
pub(crate) fn paint(
    rect: MenuRect,
    entries: &[TabMenuEntry],
    highlight: Option<usize>,
    palette: impl Into<chrome_band::ChromePalette>,
) -> Vec<Vec<RenderCell>> {
    let c = menu_colors(palette.into());
    let w = rect.w;
    let inner = w.saturating_sub(2);
    let text_start = 1 + H_PAD;
    let text_end = w.saturating_sub(1 + H_PAD);
    let mut rows: Vec<Vec<RenderCell>> = Vec::with_capacity(rect.h);

    let edge = |left: char, fill: char, right: char, colors: &MenuColors| {
        let mut row = vec![chrome_band::cell(fill, colors.rule, colors.card_bg, false, false); w];
        if w >= 1 {
            row[0] = chrome_band::cell(left, colors.rule, colors.card_bg, false, false);
        }
        if w >= 2 {
            row[w - 1] = chrome_band::cell(right, colors.rule, colors.card_bg, false, false);
        }
        row
    };

    rows.push(edge('\u{250C}', '\u{2500}', '\u{2510}', &c));
    for i in 0..visible_entries(rect) {
        let Some(entry) = entries.get(i) else { break };
        if matches!(entry, TabMenuEntry::Separator) {
            rows.push(edge('\u{251C}', '\u{2500}', '\u{2524}', &c));
            continue;
        }
        let selected = highlight == Some(i) && is_selectable(entry);
        let bg = if selected { c.sel_bg } else { c.card_bg };
        let (fg, bold) = match entry {
            TabMenuEntry::Header(_) | TabMenuEntry::Event { .. } => (c.band.label, false),
            TabMenuEntry::Action { enabled: false, .. }
            | TabMenuEntry::ConnectionAction { enabled: false, .. } => (c.disabled, false),
            TabMenuEntry::Action { .. }
            | TabMenuEntry::ConnectionAction { .. }
            | TabMenuEntry::Upgrade { .. }
                if selected =>
            {
                (c.sel_fg, true)
            }
            TabMenuEntry::Action { .. }
            | TabMenuEntry::ConnectionAction { .. }
            | TabMenuEntry::Upgrade { .. } => (c.band.value, false),
            TabMenuEntry::Separator => unreachable!("handled above"),
        };
        let mut row = vec![chrome_band::cell(' ', fg, bg, false, false); w];
        // The vertical rules keep the CARD ground even on a highlighted row —
        // a selection wash that reaches the border makes the card look torn.
        row[0] = chrome_band::cell('\u{2502}', c.rule, c.card_bg, false, false);
        if w >= 2 {
            row[w - 1] = chrome_band::cell('\u{2502}', c.rule, c.card_bg, false, false);
        }
        let budget = inner.saturating_sub(2 * H_PAD);
        let text = ellipsize(entry_text(entry), budget);
        put_text(&mut row, text_start, text_end, &text, fg, bg, bold);
        rows.push(row);
    }
    // Only close the card when the frame actually left room for the bottom
    // edge; a truncated card ends on its last entry rather than eating one.
    if rows.len() < rect.h {
        rows.push(edge('\u{2514}', '\u{2500}', '\u{2518}', &c));
    }
    rows.truncate(rect.h);
    rows
}

/// A cell that renders as bare terminal ground, as the renderer draws columns a row
/// does not materialize.
///
/// The engine trims trailing blanks, so a blank row arrives from `cell_frame_into` as
/// an empty `Vec`. The splice pads rows up to the card's right edge with this before
/// writing, reproducing the pixels already there.
///
/// `default_bg` is the frame's OSC-11 background, or `COLOR_UNSET` to use the theme's.
/// On a split, a pane's own `default_bg_spans` value can differ; the pad is then off
/// only on cells the card overwrites, except at the very edge.
#[must_use]
pub(crate) fn ground_cell(theme: Theme, default_bg: u32) -> RenderCell {
    let unpack = |c: u32| {
        [
            ((c >> 16) & 0xff) as u8,
            ((c >> 8) & 0xff) as u8,
            (c & 0xff) as u8,
        ]
    };
    let bg = if default_bg == aterm_core::render::COLOR_UNSET {
        theme.bg
    } else {
        default_bg
    };
    chrome_band::cell(' ', unpack(theme.fg), unpack(bg), false, false)
}

/// Repaint fingerprint of an open menu; `0` when closed so the `RepaintKey` term is
/// idle. It moves on open, close and every highlight step, none of which dirty a
/// terminal cell, so the settled-screen early-out cannot swallow those frames.
#[must_use]
pub(crate) fn fingerprint(menu: Option<&TabMenu>, rect: Option<MenuRect>) -> u64 {
    use std::hash::{Hash, Hasher};
    let Some(menu) = menu else { return 0 };
    let mut h = std::collections::hash_map::DefaultHasher::new();
    menu.index.hash(&mut h);
    menu.anchor_col.hash(&mut h);
    menu.highlight.hash(&mut h);
    menu.entries.len().hash(&mut h);
    for e in &menu.entries {
        match e {
            TabMenuEntry::Header(t) => (0u8, t.as_str()).hash(&mut h),
            TabMenuEntry::Event { words, at_ms } => (5u8, words.as_str(), *at_ms).hash(&mut h),
            TabMenuEntry::Separator => 1u8.hash(&mut h),
            TabMenuEntry::Action { label, enabled, .. } => {
                (2u8, label.as_str(), *enabled).hash(&mut h);
            }
            // A connection row's identity is its PEER as well as its label —
            // two peers' `Disconnect` rows read alike and must not collapse.
            TabMenuEntry::ConnectionAction {
                label,
                peer_sid,
                verb,
                enabled,
            } => (
                3u8,
                label.as_str(),
                peer_sid.as_str(),
                *verb as u8,
                *enabled,
            )
                .hash(&mut h),
            // An upgrade row's identity is the session and the build it
            // names as well as its word.
            TabMenuEntry::Upgrade {
                label,
                sid,
                to,
                word,
            } => {
                (
                    4u8,
                    label.as_str(),
                    sid.as_str(),
                    to.as_str(),
                    word.as_str(),
                )
                    .hash(&mut h);
            }
        }
    }
    rect.hash(&mut h);
    // Never 0 while open — 0 is the closed sentinel.
    h.finish() | 1
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::menu::MenuAction;

    fn model() -> Vec<TabMenuEntry> {
        vec![
            TabMenuEntry::Header("build agent".into()),
            TabMenuEntry::Separator,
            TabMenuEntry::Header("spawned \u{00B7} 4m".into()),
            TabMenuEntry::Separator,
            TabMenuEntry::Action {
                label: "Rename Session\u{2026}".into(),
                action: MenuAction::RenameSession,
                enabled: true,
            },
            TabMenuEntry::Action {
                label: "Copy Session ID".into(),
                action: MenuAction::CopySessionId,
                enabled: true,
            },
            TabMenuEntry::Action {
                label: "Copy CWD".into(),
                action: MenuAction::CopyCwd,
                enabled: false,
            },
            TabMenuEntry::Action {
                label: "Close Tab".into(),
                action: MenuAction::CloseTab,
                enabled: true,
            },
        ]
    }

    #[test]
    fn card_hangs_under_the_strip_at_the_clicked_column() {
        let m = model();
        let rect = place(&m, 7, 100, 30, 1).expect("a 100x30 frame hosts a menu");
        assert_eq!(
            rect.row, 1,
            "the card starts on the first row BELOW the band"
        );
        assert_eq!(rect.col, 7, "and its left edge is the clicked column");
        assert_eq!(rect.h, m.len() + 2, "every entry plus two border rows");
        assert!(rect.w >= "Rename Session\u{2026}".chars().count() + 4);
    }

    #[test]
    fn a_card_near_the_right_edge_shifts_left_instead_of_hanging_off() {
        let m = model();
        let rect = place(&m, 96, 100, 30, 1).expect("still placeable");
        assert_eq!(
            rect.col + rect.w,
            100,
            "the card is flush with the right edge, whole"
        );
        assert!(rect.col < 96, "which means it moved left of the anchor");
    }

    #[test]
    fn a_short_frame_truncates_rather_than_flipping_above_the_band() {
        let m = model();
        let rect = place(&m, 0, 100, 6, 1).expect("placeable in a 6-row frame");
        assert_eq!(rect.row, 1);
        assert_eq!(rect.h, 5, "clamped to the rows below the band");
        assert_eq!(visible_entries(rect), 3);
        assert_eq!(entry_at_row(rect, 4), Some(2), "the last shown entry");
        assert_eq!(
            entry_at_row(rect, 5),
            None,
            "the bottom edge shows no entry"
        );
    }

    #[test]
    fn a_frame_with_no_room_declines_to_open() {
        let m = model();
        assert_eq!(place(&m, 0, 3, 30, 1), None, "too narrow for a border pair");
        assert_eq!(place(&m, 0, 100, 3, 2), None, "no rows below the band");
        assert_eq!(place(&[], 0, 100, 30, 1), None, "an empty model is no menu");
    }

    /// A click at a pixel-derived cell resolves through the painter's rect to the same
    /// model entry, carrying the action the dispatcher runs.
    #[test]
    fn hit_test_resolves_to_the_models_own_action() {
        let m = model();
        let rect = place(&m, 4, 100, 30, 1).expect("placeable");
        // Row 0 of the card is its top border; entry i lives at rect.row+1+i.
        let copy_id_row = rect.row + 1 + 5;
        let hit = action_at(rect, &m, copy_id_row, rect.col + 3).expect("an action row");
        assert_eq!(hit, 5);
        match &m[hit] {
            TabMenuEntry::Action { action, label, .. } => {
                assert_eq!(*action, MenuAction::CopySessionId);
                assert_eq!(label, "Copy Session ID");
            }
            other => panic!("expected an action, got {other:?}"),
        }
    }

    #[test]
    fn borders_headers_separators_and_disabled_rows_are_not_activatable() {
        let m = model();
        let rect = place(&m, 4, 100, 30, 1).expect("placeable");
        let at = |i: usize| action_at(rect, &m, rect.row + 1 + i, rect.col + 3);
        assert_eq!(at(0), None, "a header is inert");
        assert_eq!(at(1), None, "a separator is inert");
        assert_eq!(at(6), None, "Copy CWD is disabled here");
        assert_eq!(at(7), Some(7), "Close Tab is live");
        assert_eq!(
            action_at(rect, &m, rect.row, rect.col + 3),
            None,
            "the top border activates nothing"
        );
        assert_eq!(
            action_at(rect, &m, rect.row + 1, rect.col),
            None,
            "nor the left rule"
        );
        // …but every one of those points is still ON the card, so the caller
        // swallows the press instead of leaking it to the grid below.
        for (r, cc) in [
            (rect.row, rect.col + 3),
            (rect.row + 1, rect.col),
            (rect.row + 1 + 6, rect.col + 3),
        ] {
            assert!(rect.contains(r, cc), "({r},{cc}) is on the card");
        }
        assert!(
            !rect.contains(rect.row + rect.h, rect.col),
            "one past is off"
        );
    }

    #[test]
    fn keyboard_stepping_visits_only_enabled_actions_and_wraps() {
        let m = model();
        let limit = m.len();
        assert_eq!(first_selectable(&m, limit), Some(4), "Rename is the first");
        let mut at = first_selectable(&m, limit);
        at = step_selectable(&m, at, true, limit);
        assert_eq!(at, Some(5), "Copy Session ID");
        at = step_selectable(&m, at, true, limit);
        assert_eq!(at, Some(7), "Copy CWD is disabled — skipped");
        at = step_selectable(&m, at, true, limit);
        assert_eq!(at, Some(4), "and it wraps to the top");
        at = step_selectable(&m, at, false, limit);
        assert_eq!(at, Some(7), "backwards wraps to the bottom");
        assert_eq!(
            step_selectable(&m, None, true, 4),
            None,
            "a card truncated above the actions highlights nothing"
        );
    }

    #[test]
    fn paint_is_exactly_the_rect_and_marks_the_highlighted_row() {
        let m = model();
        let theme = Theme {
            fg: 0x00E0_E0E0,
            bg: 0x0018_1818,
            cursor: 0x00FF_FFFF,
            selection: 0x0044_4466,
        };
        let rect = place(&m, 2, 80, 24, 1).expect("placeable");
        let rows = paint(rect, &m, Some(7), theme);
        assert_eq!(rows.len(), rect.h);
        for row in &rows {
            assert_eq!(row.len(), rect.w, "every row is exactly the card width");
        }
        assert_eq!(rows[0][0].ch, '\u{250C}');
        assert_eq!(rows[0][rect.w - 1].ch, '\u{2510}');
        assert_eq!(rows[rect.h - 1][0].ch, '\u{2514}');
        // The separator rows are rules, the entry rows are text.
        assert_eq!(rows[1 + 1][0].ch, '\u{251C}', "entry 1 is a separator");
        let close = &rows[1 + 7];
        let text: String = close.iter().map(|c| c.ch).collect();
        assert!(text.contains("Close Tab"), "got {text:?}");
        let sel_bg = close[3].bg;
        let plain_bg = rows[1 + 5][3].bg;
        assert_ne!(sel_bg, plain_bg, "the highlighted row takes its own ground");
        assert!(close[3].bold, "and its ink is emphasised");
    }

    #[test]
    fn a_long_header_ellipsizes_instead_of_widening_the_card() {
        let mut m = model();
        m[0] = TabMenuEntry::Header("x".repeat(400));
        let rect = place(&m, 0, 200, 30, 1).expect("placeable");
        assert!(
            rect.w <= MAX_CONTENT + 2 + 2 * H_PAD,
            "the card is capped at {MAX_CONTENT} content cells, got {}",
            rect.w
        );
        let theme = Theme {
            fg: 0x00FF_FFFF,
            bg: 0x0000_0000,
            cursor: 0x00FF_FFFF,
            selection: 0x0033_3333,
        };
        let rows = paint(rect, &m, None, theme);
        let header: String = rows[1].iter().map(|c| c.ch).collect();
        assert!(header.contains('\u{2026}'), "got {header:?}");
    }

    #[test]
    fn a_wide_glyph_paints_a_real_lead_and_continuation() {
        let m = vec![TabMenuEntry::Header("\u{4F60}\u{597D} agent".into())];
        let theme = Theme {
            fg: 0x00FF_FFFF,
            bg: 0x0000_0000,
            cursor: 0x00FF_FFFF,
            selection: 0x0033_3333,
        };
        let rect = place(&m, 0, 60, 20, 1).expect("placeable");
        let rows = paint(rect, &m, None, theme);
        let row = &rows[1];
        assert_eq!(row[1 + H_PAD].ch, '\u{4F60}');
        assert!(!row[1 + H_PAD].wide, "the lead carries the glyph");
        assert!(row[2 + H_PAD].wide, "and its continuation is flagged");
        assert_eq!(row[2 + H_PAD].ch, ' ', "the continuation has no glyph");
    }

    #[test]
    fn fingerprint_is_zero_closed_nonzero_open_and_moves_with_the_highlight() {
        let rect = place(&model(), 0, 80, 24, 1);
        assert_eq!(fingerprint(None, rect), 0, "closed is the byte-identical 0");
        let mut menu = TabMenu {
            tab: crate::tab_model::TabId::from_stored(1),
            index: 0,
            entries: model(),
            anchor_col: 0,
            highlight: None,
            keyboard: false,
        };
        let open = fingerprint(Some(&menu), rect);
        assert_ne!(open, 0);
        menu.highlight = Some(4);
        let lit = fingerprint(Some(&menu), rect);
        assert_ne!(open, lit, "a highlight step must force the repaint");
        menu.highlight = None;
        assert_eq!(fingerprint(Some(&menu), rect), open, "and it is stable");
    }
}

/// The shared key→meaning table, which keeps the winit route and the control seam
/// in agreement.
#[cfg(test)]
mod nav_tests {
    use super::{MenuNav, nav_for_key};
    use aterm_types::keyboard::{Key, NamedKey as Nk};

    #[test]
    fn the_win32_menu_bindings_are_the_table() {
        for (k, want) in [
            (Key::Named(Nk::Enter), MenuNav::Activate),
            (Key::Named(Nk::NumpadEnter), MenuNav::Activate),
            (Key::Named(Nk::Space), MenuNav::Activate),
            (Key::Character(' '), MenuNav::Activate),
            (Key::Named(Nk::Escape), MenuNav::Dismiss),
            (Key::Named(Nk::ArrowDown), MenuNav::Down),
            (Key::Named(Nk::NumpadArrowDown), MenuNav::Down),
            (Key::Named(Nk::ArrowUp), MenuNav::Up),
            (Key::Named(Nk::Home), MenuNav::First),
            (Key::Named(Nk::End), MenuNav::Last),
            // No menu meaning ⇒ dismiss-and-swallow, never a fall-through.
            (Key::Character('a'), MenuNav::Dismiss),
            (Key::Named(Nk::Tab), MenuNav::Dismiss),
            (Key::Named(Nk::F5), MenuNav::Dismiss),
        ] {
            assert_eq!(nav_for_key(&k), want, "{k:?}");
        }
    }

    /// A bare modifier or lock press is inert: swallowed, card untouched, so the Shift
    /// half of ⇧F10 does not dismiss the card.
    #[test]
    fn bare_modifiers_and_locks_are_inert_not_a_dismiss() {
        for k in [
            Nk::ShiftLeft,
            Nk::ShiftRight,
            Nk::ControlLeft,
            Nk::AltLeft,
            Nk::SuperLeft,
            Nk::MetaLeft,
            Nk::HyperLeft,
            Nk::CapsLock,
            Nk::NumLock,
            Nk::ScrollLock,
        ] {
            assert_eq!(
                nav_for_key(&Key::Named(k)),
                MenuNav::Inert,
                "{k:?} must not dismiss the card"
            );
        }
    }
}
