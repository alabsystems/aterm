// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! In-grid tab context menu layout, keyboard navigation, and cell painter.

use aterm_core::terminal::RenderCell;
use aterm_render::Theme;

use crate::chrome_band::{self, BandColors};
use crate::session_chrome::TabMenuEntry;
use crate::tab_model::TabId;

/// Cells of horizontal breathing room inside each vertical border.
const H_PAD: usize = 1;
/// Narrowest the card may be, in content cells: a menu whose rows read
/// `Copy…`/`Close…` is worse than one that runs a little wide.
const MIN_CONTENT: usize = 16;
/// Widest the card may be, in content cells. Headers (whose description can reach
/// [`crate::session_chrome::DESCRIPTION_DISPLAY_MAX`] graphemes) ellipsize to this.
const MAX_CONTENT: usize = 44;

/// Open in-grid tab context menu state for one window.
pub(crate) struct TabMenu {
    /// The stable identity of the right-clicked tab — the dispatch subject.
    pub tab: TabId,
    /// The tab's position when the menu popped: the paint anchor only.
    pub index: usize,
    /// The composed model, snapshotted at pop time so the card never reflows under
    /// the pointer; actions validate their captured targets at dispatch.
    pub entries: Vec<TabMenuEntry>,
    /// The strip column the press landed on: the card's left edge, before clamping.
    pub anchor_col: u16,
    /// The highlighted row (index into `entries`), or `None` when the pointer is off
    /// every action row.
    pub highlight: Option<usize>,
    /// Whether the keyboard owns the highlight: set by a Shift+F10 / Menu key open,
    /// cleared when the pointer enters a row.
    pub keyboard: bool,
}

impl TabMenu {
    /// Format the open menu's rows and highlight for `controls tab-menu`.
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

/// Menu card bounding rectangle in frame cell coordinates.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) struct MenuRect {
    pub row: usize,
    pub col: usize,
    pub w: usize,
    pub h: usize,
}

impl MenuRect {
    /// Whether frame cell `(row, col)` lies within the card (including its border).
    #[must_use]
    pub(crate) const fn contains(&self, row: usize, col: usize) -> bool {
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

/// Display width of `s` in cells after control-character sanitization.
#[must_use]
pub(crate) fn display_cells(s: &str) -> usize {
    s.chars().map(|c| char_cells(safe_char(c))).sum()
}

/// Truncate `s` to at most `max` display cells with a trailing `…` when clipped.
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
pub(crate) fn entry_text(entry: &TabMenuEntry) -> &str {
    match entry {
        TabMenuEntry::Header(t) => t.as_str(),
        TabMenuEntry::Event { words, .. } | TabMenuEntry::RewindTo { words, .. } => words,
        TabMenuEntry::Separator => "",
        TabMenuEntry::Action { label, .. } => label,
        TabMenuEntry::SessionAction { label, .. } => label,
        TabMenuEntry::ConnectionAction { label, .. } => label,
        TabMenuEntry::Upgrade { label, .. } => label,
    }
}

/// Whether `entry` is an enabled action row that can be highlighted or activated.
#[must_use]
pub(crate) const fn is_selectable(entry: &TabMenuEntry) -> bool {
    matches!(
        entry,
        TabMenuEntry::Action { enabled: true, .. }
            | TabMenuEntry::SessionAction {
                enabled: true,
                origin: Some(_),
                ..
            }
            | TabMenuEntry::ConnectionAction { enabled: true, .. }
            | TabMenuEntry::Upgrade { .. }
            | TabMenuEntry::RewindTo {
                origin: Some(_),
                ..
            }
    )
}

/// Clamped interior content width in cells (`MIN_CONTENT..=MAX_CONTENT`) for `entries`.
#[must_use]
pub(crate) fn content_width(entries: &[TabMenuEntry]) -> usize {
    entries
        .iter()
        .map(|e| display_cells(entry_text(e)))
        .max()
        .unwrap_or(0)
        .clamp(MIN_CONTENT, MAX_CONTENT)
}

/// Compute the clamped [`MenuRect`] for `entries` anchored at `anchor_col` below `strip_rows`.
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

/// The rows a client reads of a menu laid out at `rect`: each one visible but the rules, with its
/// index in `entries`.
#[cfg(a11y_tree)]
pub(crate) fn reader_rows(
    entries: &[TabMenuEntry],
    rect: MenuRect,
) -> impl Iterator<Item = (usize, &TabMenuEntry)> {
    entries
        .iter()
        .enumerate()
        .take(visible_entries(rect))
        .filter(|(_, entry)| !matches!(entry, TabMenuEntry::Separator))
}

/// Number of entry rows visible inside `rect` (height minus two border rows).
#[must_use]
pub(crate) const fn visible_entries(rect: MenuRect) -> usize {
    rect.h.saturating_sub(2)
}

/// Entry index displayed at frame `row`, or `None` on border or out-of-range rows.
#[must_use]
pub(crate) fn entry_at_row(rect: MenuRect, row: usize) -> Option<usize> {
    if row <= rect.row || row + 1 >= rect.row + rect.h {
        return None;
    }
    let i = row - rect.row - 1;
    (i < visible_entries(rect)).then_some(i)
}

/// Selectable entry index at frame `(row, col)`, if any.
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

/// First selectable entry index within `limit` visible rows.
#[must_use]
pub(crate) fn first_selectable(entries: &[TabMenuEntry], limit: usize) -> Option<usize> {
    entries.iter().take(limit).position(is_selectable)
}

/// Advance the highlight one selectable row down or up within `limit` rows, wrapping around.
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

/// Keyboard navigation action for an open tab menu.
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

/// Map an engine key press to its [`MenuNav`] action.
#[must_use]
pub(crate) const fn nav_for_key(key: &aterm_types::keyboard::Key) -> MenuNav {
    use aterm_types::keyboard::{Key, NamedKey as Nk};
    if aterm_types::keyboard::is_modifier_or_lock_key(key) {
        return MenuNav::Inert;
    }
    match key {
        Key::Named(Nk::Enter | Nk::NumpadEnter | Nk::Space) => MenuNav::Activate,
        Key::Character(' ') => MenuNav::Activate,
        Key::Named(Nk::ArrowDown | Nk::NumpadArrowDown) => MenuNav::Down,
        Key::Named(Nk::ArrowUp | Nk::NumpadArrowUp) => MenuNav::Up,
        Key::Named(Nk::Home | Nk::NumpadHome) => MenuNav::First,
        Key::Named(Nk::End | Nk::NumpadEnd) => MenuNav::Last,
        _ => MenuNav::Dismiss,
    }
}

/// Derived RGB palette for the tab menu card.
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
    let fg = rgb(palette.theme.fg);
    let card_bg = chrome_band::mix3(chrome_band::band_colors(palette).bar_bg, fg, 0.08);
    menu_colors_on(palette, card_bg)
}

const fn rgb(c: u32) -> [u8; 3] {
    [
        ((c >> 16) & 0xff) as u8,
        ((c >> 8) & 0xff) as u8,
        (c & 0xff) as u8,
    ]
}

/// The card's colours on the pane's own ground: a card that wears the harness glass stands on
/// the ground its glass inks are solved on (`docs/DESIGN-harness-glass-rainbow-2026-11-05.md`
/// §4.1), never the band's grey, on which Day's letters floored to 4.5:1 turn olive and brown.
pub(crate) fn glass_menu_colors(palette: impl Into<chrome_band::ChromePalette>) -> MenuColors {
    let palette = palette.into();
    let ground = rgb(palette.theme.bg);
    menu_colors_on(palette, ground)
}

/// The card's colours on `card_bg`: every ink floored against it.
fn menu_colors_on(palette: chrome_band::ChromePalette, card_bg: [u8; 3]) -> MenuColors {
    let fg = rgb(palette.theme.fg);
    let band = chrome_band::band_colors(palette);
    let value = chrome_band::ensure_contrast(fg, card_bg, 4.5);
    let label = chrome_band::ensure_contrast(band.label, card_bg, 4.5);
    let rule = chrome_band::mix3(card_bg, value, 0.34);
    let sel_bg = chrome_band::mix3(card_bg, fg, 0.22);
    MenuColors {
        card_bg,
        rule,
        sel_fg: chrome_band::ensure_contrast(value, sel_bg, 4.5),
        sel_bg,
        disabled: chrome_band::ensure_contrast(label, card_bg, 3.0),
        band: BandColors {
            label,
            value,
            ..band
        },
    }
}

/// Write `s` into `row` starting at `col` up to `end`, emitting continuation cells for wide chars.
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

/// Paint `menu` into a `rect.h` × `rect.w` cell grid.
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
            | TabMenuEntry::SessionAction { enabled: false, .. }
            | TabMenuEntry::SessionAction { origin: None, .. }
            | TabMenuEntry::ConnectionAction { enabled: false, .. }
            | TabMenuEntry::RewindTo { origin: None, .. } => (c.disabled, false),
            TabMenuEntry::Action { .. }
            | TabMenuEntry::SessionAction { .. }
            | TabMenuEntry::ConnectionAction { .. }
            | TabMenuEntry::Upgrade { .. }
            | TabMenuEntry::RewindTo { .. }
                if selected =>
            {
                (c.sel_fg, true)
            }
            TabMenuEntry::Action { .. }
            | TabMenuEntry::SessionAction { .. }
            | TabMenuEntry::ConnectionAction { .. }
            | TabMenuEntry::Upgrade { .. }
            | TabMenuEntry::RewindTo { .. } => (c.band.value, false),
            TabMenuEntry::Separator => unreachable!("handled above"),
        };
        let mut row = vec![chrome_band::cell(' ', fg, bg, false, false); w];
        row[0] = chrome_band::cell('\u{2502}', c.rule, c.card_bg, false, false);
        if w >= 2 {
            row[w - 1] = chrome_band::cell('\u{2502}', c.rule, c.card_bg, false, false);
        }
        let budget = inner.saturating_sub(2 * H_PAD);
        let text = ellipsize(entry_text(entry), budget);
        put_text(&mut row, text_start, text_end, &text, fg, bg, bold);
        rows.push(row);
    }
    if rows.len() < rect.h {
        rows.push(edge('\u{2514}', '\u{2500}', '\u{2518}', &c));
    }
    rows.truncate(rect.h);
    rows
}

/// Blank cell rendered in the terminal's default background for padding short frame rows.
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

/// Structural repaint key for an open [`TabMenu`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TabMenuRepaintKey {
    pub(crate) index: usize,
    pub(crate) anchor_col: u16,
    pub(crate) highlight: Option<usize>,
    pub(crate) rect: Option<MenuRect>,
    pub(crate) entries_len: usize,
    pub(crate) entries_hash: u64,
}

/// Structural repaint key for the open tab menu (`None` when closed).
#[must_use]
pub(crate) fn fingerprint(
    menu: Option<&TabMenu>,
    rect: Option<MenuRect>,
) -> Option<TabMenuRepaintKey> {
    use std::hash::{Hash, Hasher};
    let menu = menu?;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for e in &menu.entries {
        match e {
            TabMenuEntry::Header(t) => (0u8, t.as_str()).hash(&mut h),
            TabMenuEntry::Event { words, at_ms } => (5u8, words.as_str(), *at_ms).hash(&mut h),
            TabMenuEntry::RewindTo {
                words,
                at_ms,
                origin,
            } => {
                (6u8, words.as_str(), *at_ms, *origin).hash(&mut h);
            }
            TabMenuEntry::Separator => 1u8.hash(&mut h),
            TabMenuEntry::Action { label, enabled, .. } => {
                (2u8, label.as_str(), *enabled).hash(&mut h);
            }
            TabMenuEntry::SessionAction {
                label,
                enabled,
                origin,
                ..
            } => {
                (7u8, label.as_str(), *enabled, *origin).hash(&mut h);
            }
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
    Some(TabMenuRepaintKey {
        index: menu.index,
        anchor_col: menu.anchor_col,
        highlight: menu.highlight,
        rect,
        entries_len: menu.entries.len(),
        entries_hash: h.finish(),
    })
}

#[cfg(test)]
#[path = "tab_menu_tests.rs"]
mod tests;

/// The shared key→meaning table, which keeps the winit route and the control seam
/// in agreement.
#[cfg(test)]
#[path = "tab_menu_nav_tests.rs"]
mod nav_tests;
