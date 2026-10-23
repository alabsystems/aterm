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
    /// the pointer; actions resolve their payloads fresh at dispatch.
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
fn entry_text(entry: &TabMenuEntry) -> &str {
    match entry {
        TabMenuEntry::Header(t) => t.as_str(),
        TabMenuEntry::Event { words, .. } => words,
        TabMenuEntry::Separator => "",
        TabMenuEntry::Action { label, .. } => label,
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
            | TabMenuEntry::ConnectionAction { enabled: true, .. }
            | TabMenuEntry::Upgrade { .. }
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
    let theme = palette.theme;
    let band = chrome_band::band_colors(palette);
    let fg = [
        ((theme.fg >> 16) & 0xff) as u8,
        ((theme.fg >> 8) & 0xff) as u8,
        (theme.fg & 0xff) as u8,
    ];
    let card_bg = chrome_band::mix3(band.bar_bg, fg, 0.08);
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

/// Repaint fingerprint for the open tab menu (`0` when closed).
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
    rect.hash(&mut h);
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
