// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The IN-GRID TAB CONTEXT MENU: the popup a right-click on an in-grid tab chip
//! drops, painted as terminal cells over the frame the way every other piece of
//! this app's own chrome is (the strip, the find bar, the config banner).
//!
//! # Why this exists at all
//!
//! [`crate::session_chrome::compose_tab_menu`] has shipped a complete,
//! unit-proved context-menu MODEL for every terminal tab since session-metadata
//! stage 2 — identity headers, the timeline tail, then `Rename Session…` /
//! `Copy Session ID` / `Copy CWD` / `Close Tab`. macOS renders it as a real
//! `NSMenu` off the strip's `TabView`; off macOS the model was composed, stored
//! on the toolbar handle, serialised by the `chrome` verb — and never shown to a
//! human. A right-click on a Windows tab did NOTHING. This module is the missing
//! renderer, so the same one model finally reaches the same glass on both
//! platforms.
//!
//! # Why cells and not `TrackPopupMenu`
//!
//! The obvious Windows answer is a real `HMENU` + `TrackPopupMenu`: native look,
//! native keyboard and accessibility, no painting at all. It was rejected, and
//! the reason is mechanical rather than aesthetic:
//!
//! * `TrackPopupMenu` runs its OWN modal message loop. Every call site we have
//!   is INSIDE winit's event callback (`on_mouse_input` / `on_key`), and the
//!   vendored runner takes the handler out of a `Cell` for the duration of a
//!   dispatch — `send_event` then routes `RedrawRequested` to
//!   `call_event_handler` with NO buffering, whose `.expect("either event
//!   handler is re-entrant (likely) …")` is a hard panic
//!   (`vendor/winit/src/platform_impl/windows/event_loop/runner.rs`). A menu
//!   pops a top-level window over ours; the `WM_PAINT` that follows would kill
//!   the app. The existing `drag_window()` precedent is not a counter-example:
//!   winit POSTS `WM_NCLBUTTONDOWN` and returns, so DefWindowProc's modal loop
//!   starts after our handler has already unwound. There is no equally cheap
//!   deferral point for a popup that must appear at the pointer, on the press.
//! * Nothing in `crates/` speaks `HMENU` today, so this would also be a brand-new
//!   FFI surface (`CreatePopupMenu`/`AppendMenuW`/`TrackPopupMenu`/`DestroyMenu`
//!   plus `ClientToScreen` and an owner-window handle) for one gesture.
//! * The menu's own content argues against it too: the top half is a session
//!   IDENTITY card (emoji icon, an authored description, a relative-age timeline
//!   tail). As `MFT_STRING | MFS_DISABLED` rows in a system menu that reads as a
//!   broken menu; as a themed card it reads as what it is.
//! * Everything the user is right-clicking — the band, the chip, its `✕`, the
//!   `+` — is already OUR pixels in OUR theme (`theme is the source of identity`,
//!   `native_appearance.rs`). A system-tinted popup hanging off a themed chip is
//!   the one place the seam would show.
//!
//! What that costs us is honest and worth stating: no OS-level keyboard menu
//! semantics for free (we implement Esc/arrows/Enter ourselves, below), and no
//! UI-Automation menu role (the popup is grid cells; a screen reader sees the
//! cells, not a `menu`). Accessibility parity would need the AccessKit tree to
//! grow a node for this surface, which is deliberately left for the a11y lane
//! rather than half-done here.
//!
//! # Shape
//!
//! PURE by construction — geometry, hit-testing, keyboard stepping and the cell
//! paint are all free functions over the model, so the hit-test ↔ paint ↔
//! dispatch chain is unit-proved headlessly on every platform. The impure half
//! (opening from a press, blitting into `input_scratch`, posting the chosen
//! action) lives in `app_mouse.rs` / `app_render.rs` / `app_tabs.rs`.

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
/// Widest the card may be, in CONTENT cells. The identity header carries a
/// user-authored description capped at 160 GRAPHEMES upstream
/// ([`crate::session_chrome::DESCRIPTION_DISPLAY_MAX`]); a menu that grows to
/// fit it would be a paragraph with a border. Headers ellipsize to this.
const MAX_CONTENT: usize = 44;

/// One OPEN tab context menu on one window.
///
/// `tab` is the STABLE [`TabId`] captured when the menu popped — never the
/// index — for exactly the reason `Wake::TabMenuAction` documents: the menu can
/// outlive a background exit or a control-socket `tab move`, and acting on
/// "whatever sits in slot 3 now" would copy from, or close, a different live
/// session. `index` is kept ALONGSIDE it purely as the paint anchor (which chip
/// the card hangs under) and is allowed to go stale; every ACTION re-resolves
/// through `tab`.
pub(crate) struct TabMenu {
    /// The stable identity of the right-clicked tab — the dispatch subject.
    pub tab: TabId,
    /// The tab's position when the menu popped: the paint anchor only.
    pub index: usize,
    /// The composed model, snapshotted at pop time. Snapshotted rather than
    /// recomposed per frame because the card must not reflow (or re-order!)
    /// under the pointer while a human is aiming at a row; the ACTIONS resolve
    /// their payloads fresh at dispatch, so a long-open menu still copies the
    /// current truth.
    pub entries: Vec<TabMenuEntry>,
    /// The strip COLUMN the press landed on — the card's left edge wants to sit
    /// here, before clamping.
    pub anchor_col: u16,
    /// The highlighted row (an index into `entries`), or `None` when the
    /// pointer is off every action row. Seeded to the first enabled action when
    /// the menu was opened from the KEYBOARD, and left `None` for a mouse open
    /// (a menu that pops with a row pre-lit under a pointer that has not moved
    /// yet invites a mis-click on Enter).
    pub highlight: Option<usize>,
    /// Whether the highlight currently belongs to the KEYBOARD — set when
    /// Shift+F10 / the Menu key opened the card (seeding the rule above), and
    /// cleared the moment the pointer enters a row. While it is set, pointer
    /// motion OFF the card leaves the highlight alone: a hand brushing the mouse
    /// must not silently unselect the row a keyboard user is about to press ↵ on.
    pub keyboard: bool,
}

/// Where the card actually landed, in FRAME cell coordinates (the same axis the
/// tab-strip splice works in: row 0 is the strip's first row). Recorded by the
/// painter and read by the hit-test, so the two can never disagree about where
/// the menu is — the rule `find_bar_hit` already follows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) struct MenuRect {
    pub row: usize,
    pub col: usize,
    pub w: usize,
    pub h: usize,
}

impl MenuRect {
    /// Whether frame cell `(row, col)` is anywhere on the card — border
    /// included. The card is modal-ish, so a press on its BORDER must be
    /// swallowed, not fall through to the terminal underneath.
    #[must_use]
    pub(crate) fn contains(&self, row: usize, col: usize) -> bool {
        row >= self.row
            && row < self.row + self.h
            && col >= self.col
            && col < self.col + self.w
    }
}

/// Display cells one projected char occupies. Same table the terminal grid and
/// the strip classify with ([`aterm_grapheme::char_width`]), clamped to one so a
/// combining mark still owns a cell — the strip's rule, restated here because
/// its helper is private and a second width policy is how two chrome surfaces
/// drift apart.
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
        TabMenuEntry::Separator => "",
        TabMenuEntry::Action { label, .. } => label,
    }
}

/// Whether this entry is a row a click / Enter can ACTIVATE. Headers and
/// separators are inert; a DISABLED action is inert too — it stays visible (a
/// stable menu shape is learnable, `compose_tab_menu`'s stated rule) but is not
/// selectable, exactly like a greyed `NSMenuItem`.
#[must_use]
pub(crate) fn is_selectable(entry: &TabMenuEntry) -> bool {
    matches!(entry, TabMenuEntry::Action { enabled: true, .. })
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

/// Place the card in FRAME cell coordinates.
///
/// * `anchor_col` — the strip column the press landed on; the card's left edge
///   starts there, so the menu hangs off the chip the user aimed at.
/// * `strip_rows` — how many chrome rows the strip prepended; the card starts
///   immediately below the band (Windows drops a tab menu under the tab, and it
///   keeps the card off the band it was launched from).
/// * `cols` / `frame_rows` — the composed frame's extent.
///
/// Returns `None` when the frame simply cannot host a card (a window narrower
/// than a border pair, or with no room below the strip) — the caller then
/// declines to open rather than painting a sliver.
///
/// The clamping rule is "prefer to stay whole": the card shifts LEFT to fit the
/// right edge before it is allowed to shrink, and it shrinks only when even the
/// flush-left placement overflows. Vertically it TRUNCATES rather than flipping
/// above the strip — there is nothing above the strip but the strip.
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

/// How many ENTRY rows the card at `rect` actually shows (its height minus the
/// two border rows). A card the frame truncated shows a prefix of the model —
/// the identity headers come first, so a short window keeps the identity and
/// loses the tail, which is the wrong end to lose; see the note in
/// [`entry_at_row`] for why it is still preferable to a scroller.
#[must_use]
pub(crate) fn visible_entries(rect: MenuRect) -> usize {
    rect.h.saturating_sub(2)
}

/// Which model entry frame `row` shows, or `None` for the border rows / a row
/// past the truncation. Pure inverse of the painter's row walk.
///
/// There is deliberately NO scrolling: a context menu that scrolls is a list
/// box. The model is bounded by construction (identity ≤ 4 lines, timeline ≤
/// [`crate::session_chrome::TIMELINE_TAIL`], four actions), so the only way to
/// truncate is a genuinely tiny window — where the right answer is fewer rows,
/// not a scrollbar in a popup.
#[must_use]
pub(crate) fn entry_at_row(rect: MenuRect, row: usize) -> Option<usize> {
    if row <= rect.row || row + 1 >= rect.row + rect.h {
        return None;
    }
    let i = row - rect.row - 1;
    (i < visible_entries(rect)).then_some(i)
}

/// The entry a click at frame `(row, col)` activates: `Some(i)` only when the
/// point is on a VISIBLE, SELECTABLE row of this card. Off the card, on a
/// border, on a header/separator, or on a disabled action ⇒ `None` (the caller
/// still swallows the press — see `MenuRect::contains`).
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
    entries
        .iter()
        .take(limit)
        .position(is_selectable)
}

/// Step the highlight one selectable row in `down`'s direction, WRAPPING at the
/// ends (the menu convention everywhere, and the only sane behaviour for a
/// four-item list). `from = None` enters at the first (or last) selectable row.
/// `limit` is [`visible_entries`] — a truncated card never highlights a row it
/// is not showing.
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

/// What one key press MEANS to an open card — the Win32 menu bindings, as a
/// decision no route owns privately.
///
/// It exists because the card is driven from TWO places that must never
/// disagree: the physical winit route (`App::on_key_tab_menu_mode`) and the
/// engine-neutral convergence seam (`App::tab_menu_input_event`, where
/// `aterm ctl key …` arrives). A controller and a hand pressing the same key
/// have to move the same highlight, or the introspection mirror stops being a
/// mirror.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MenuNav {
    /// ↵ / Space — run the highlighted row (or dismiss when nothing is lit).
    Activate,
    /// Esc, and every key with no menu meaning: dismiss AND swallow. A menu is a
    /// mode, so an unhandled keystroke must not both close the card and land in
    /// the shell underneath. (Win32 spends those keys on mnemonics; we have no
    /// mnemonic model, so the safe reading of an unknown key is "dismiss".)
    Dismiss,
    /// ↓ — step the highlight forward over enabled rows, wrapping.
    Down,
    /// ↑ — step it backward.
    Up,
    /// Home — jump to the first enabled row.
    First,
    /// End — jump to the last enabled row.
    Last,
    /// A bare MODIFIER or LOCK press: swallowed and otherwise IGNORED, card
    /// stays. No Win32 menu closes because you rested a finger on Shift, and
    /// reaching Shift+F10 to re-pop the card would otherwise dismiss it on the
    /// Shift half of the very chord that opened it. The same key class
    /// `is_bare_modifier_key` protects the rename field from, for the same
    /// reason.
    Inert,
}

/// The menu meaning of an ENGINE key. See [`MenuNav`].
///
/// Numpad spellings are folded onto their navigation twins: a card driven from
/// the number pad with NumLock off must step, not dismiss.
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

/// The card's paint tones. Derived from the SAME `chrome_band::band_colors` the
/// find bar / config banner / native backing rows use, so the popup is
/// recognisably the same chrome family — but one shade further from the terminal
/// background, because unlike those bands this card FLOATS over live output and
/// has to read as lifted off it rather than spliced into it.
struct MenuColors {
    band: BandColors,
    /// The card ground: the band tone pushed further from the terminal bg.
    card_bg: [u8; 3],
    /// Border / separator rule ink.
    rule: [u8; 3],
    /// The highlighted row's ground + ink.
    sel_bg: [u8; 3],
    sel_fg: [u8; 3],
    /// A DISABLED action's ink: dimmer than a header, because a header is
    /// information and a greyed action is a promise the app is not keeping
    /// right now.
    disabled: [u8; 3],
}

fn menu_colors(theme: Theme) -> MenuColors {
    let band = chrome_band::band_colors(theme);
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
        // Held to 3:1 rather than 4.5:1 ON PURPOSE — a disabled row must read as
        // unavailable, and AA-strength ink does not. 3:1 is the WCAG
        // non-text/large floor, so it stays perceivable rather than vanishing.
        disabled: chrome_band::ensure_contrast(label, card_bg, 3.0),
        band: BandColors { label, value, ..band },
    }
}

/// Write `s` into `row` starting at `col`, stopping before `end`, emitting a
/// real lead + continuation pair for width-2 chars (the strip's contract — a
/// wide glyph that would straddle `end` is dropped whole rather than painting
/// half a glyph over the border).
fn put_text(
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
        if w == 2 && let Some(slot) = row.get_mut(col + 1) {
            let mut tail = chrome_band::cell(' ', fg, bg, bold, false);
            tail.wide = true;
            *slot = tail;
        }
        col += w;
    }
}

/// Paint the card: exactly `rect.h` rows of exactly `rect.w` cells, ready for
/// the splice to blit at `(rect.row, rect.col)`.
///
/// Box-drawing rules rather than a bare tinted block: this card floats over
/// arbitrary terminal output, and a borderless tint is indistinguishable from a
/// program's own colour run. `┌─┐│└┘├┤` are single-width in every width table we
/// classify with, so the border costs exactly one cell a side.
#[must_use]
pub(crate) fn paint(
    rect: MenuRect,
    entries: &[TabMenuEntry],
    highlight: Option<usize>,
    theme: Theme,
) -> Vec<Vec<RenderCell>> {
    let c = menu_colors(theme);
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
            TabMenuEntry::Header(_) => (c.band.label, false),
            TabMenuEntry::Action { enabled: false, .. } => (c.disabled, false),
            TabMenuEntry::Action { .. } if selected => (c.sel_fg, true),
            TabMenuEntry::Action { .. } => (c.band.value, false),
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

/// A cell that renders as bare terminal ground — what the renderer already
/// draws for a column a row does not materialize.
///
/// This is NOT decoration: the engine TRIMS trailing blanks, so a blank
/// terminal row arrives from `cell_frame_into` as a ZERO-LENGTH `Vec` and the
/// renderer fills it from the frame's default background. A popup landing on a
/// blank row (i.e. most of a fresh shell's screen) therefore has no cells to
/// write into. The splice pads the row up to the card's right edge with this
/// before writing, which reproduces exactly the pixels that were already there.
///
/// `default_bg` is the frame's live OSC-11 background, or `COLOR_UNSET` when
/// the program has not set one — in which case the theme's own background is
/// what the renderer would have used. On a SPLIT the per-pane
/// `default_bg_spans` could differ from this value (the frame carries ONE
/// such background, panes notwithstanding); the pad is
/// then off by the pane's own OSC 11, on cells the card is about to overwrite
/// anyway except at the very edge. Reading the span set here would couple this
/// splice to the composite's pane partition for a one-column effect.
///
/// scope-waiver: the phrase describes the scope of the CALLER'S `default_bg`
/// argument — the one window-wide scalar this fn is handed — and does so in
/// order to say it is the WRONG scope on a split, which is the opposite of
/// claiming window-wide authority for anything declared here. `ground_cell` is
/// a pure `(Theme, u32) -> RenderCell` mapping: it owns no state, enforces no
/// budget, and no instance count of it can be wrong.
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

/// The repaint fingerprint of an open menu — `0` when nothing is open, so the
/// `RepaintKey` term is byte-identical to the pre-menu path while closed (the
/// idle invariant every other `*_fp` term in that key holds to).
///
/// It must move on OPEN, on CLOSE, and on every highlight step: none of those
/// dirty a terminal cell, so without this the settled-screen early-out would
/// swallow the frame that draws the card and the frame that erases it.
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
            TabMenuEntry::Separator => 1u8.hash(&mut h),
            TabMenuEntry::Action {
                label, enabled, ..
            } => (2u8, *label, *enabled).hash(&mut h),
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
                label: "Rename Session\u{2026}",
                action: MenuAction::RenameSession,
                enabled: true,
            },
            TabMenuEntry::Action {
                label: "Copy Session ID",
                action: MenuAction::CopySessionId,
                enabled: true,
            },
            TabMenuEntry::Action {
                label: "Copy CWD",
                action: MenuAction::CopyCwd,
                enabled: false,
            },
            TabMenuEntry::Action {
                label: "Close Tab",
                action: MenuAction::CloseTab,
                enabled: true,
            },
        ]
    }

    #[test]
    fn card_hangs_under_the_strip_at_the_clicked_column() {
        let m = model();
        let rect = place(&m, 7, 100, 30, 1).expect("a 100x30 frame hosts a menu");
        assert_eq!(rect.row, 1, "the card starts on the first row BELOW the band");
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
        assert_eq!(entry_at_row(rect, 5), None, "the bottom edge shows no entry");
    }

    #[test]
    fn a_frame_with_no_room_declines_to_open() {
        let m = model();
        assert_eq!(place(&m, 0, 3, 30, 1), None, "too narrow for a border pair");
        assert_eq!(place(&m, 0, 100, 3, 2), None, "no rows below the band");
        assert_eq!(place(&[], 0, 100, 30, 1), None, "an empty model is no menu");
    }

    /// The whole point of the bundle: a click at a pixel-derived cell resolves
    /// through the SAME rect the painter used to the SAME model entry, and that
    /// entry carries the action the dispatcher will run.
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
                assert_eq!(*label, "Copy Session ID");
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
        assert!(!rect.contains(rect.row + rect.h, rect.col), "one past is off");
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

/// C5 REMEDIATION — the shared key→meaning table. These are pure and run on
/// every platform, because the table is what keeps the winit route and the
/// control seam from drifting: if this file's answer changes, BOTH routes
/// change together or neither does.
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

    /// A bare MODIFIER or LOCK press is INERT: swallowed, card untouched. No
    /// Win32 menu closes because a finger rested on Shift — and reaching for
    /// ⇧F10 to re-pop the card must not dismiss it on the ⇧ half of the very
    /// chord that opens it.
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
