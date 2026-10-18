// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE AUTOPILOT MENU, set from the chip
//! (`docs/DESIGN-harness-autopilot-2026-10-15.md` §9.3.3): Cruise, Land now,
//! Off, the timed Cruises, the cancel and `⚙ Harness settings…`, as cells
//! over the pane — a popup, never a chrome row, so no program is re-gridded
//! — opened by a right-click on the Autopilot chip (a ctrl-click on macOS),
//! or `↓`, shift+F10 or the Menu key while `ctrl+shift+tab` has the chip
//! selected.
//!
//! ```text
//!  Autopilot · tab 3 “fix the footer”
//!  ∞  Cruise                              ●
//!  ⤓  Land now
//!  ○  Off
//!  ──────────────────────────────────────
//!  ∞⤓ Cruise 1 hour, then land
//!  ∞⤓ Cruise 2 hours, then land
//!  ∞⤓ Cruise 4 hours, then land
//!  ∞⤓ Cruise until 18:00, then land          only when [harness] land_at is set
//!  ✕  Cancel landing at 18:00                only while a timer is armed
//!  ──────────────────────────────────────
//!  ⚙  Harness settings…
//! ```
//!
//! **Shape.** The model ([`rows`]), its geometry ([`place`]), hit test
//! ([`item_at`]), keyboard steps ([`step`]) and paint ([`paint`]) are pure
//! functions over the rows, generalized from the in-grid tab menu
//! (`crate::tab_menu`, whose rect, ink and text cutting they share). Not an
//! NSMenu on macOS: the chip is no NSView, and a menu's nested tracking loop
//! would stop frames while the agent's output keeps coming. Its
//! accessibility node is the grid tree's `a11y_tree::GridMenu`
//! ([`App::autopilot_grid_menu`]: a `Role::Menu` the window publishes where
//! the tree is built, its items in words, the highlighted row the focus).
//!
//! **Where.** It hangs above the chip, its left edge at the chip's column,
//! and opens downward only where fewer rows are above.
//!
//! **On top.** It is painted after the frame published its overlays, so its
//! splice takes its cells back from them (`app_render::scrub_overlay_card`):
//! a caret under it is hidden and a sprite across its edge (the resident
//! pet) is cut there; and its paint is a host write (`snapshot_seq`), so the
//! card is drawn whole, never only over the rows the program redrew.
//!
//! **Keys.** `↑`/`↓`/Return/Esc. Any other key closes it and passes through,
//! as the chip selection does. A click outside closes it, and is spent on
//! that, as the tab menu's is.
//!
//! **It closes by itself** on the rule a stale chip selection has: at the
//! first painted frame whose glass no longer shows its session's Autopilot
//! chip at the menu's anchor (a box covered the composer, the pane narrowed,
//! the chip did not fit), and on a focus or tab change — so a box that
//! replaces Claude's composer under an open menu is answered by whoever
//! answers boxes, never by the menu's Return.
//!
//! **Under the master switch (or no host)** the mode and timer rows are
//! shown disabled under the line `harness off in Settings`, and only `⚙
//! Harness settings…` acts. Under the session's agent's own switch they are
//! disabled under that switch by name, `Supervise Claude Code sessions is off`, as
//! the chip's title says it (§10.2.1).
//!
//! **It follows its session.** Where the session's word or timer moves under
//! an open menu (a wire `meta set`, the timer's fire, Settings), its rows are
//! built again from the store, so its `●` and its cancel never stand stale;
//! and the cancel is a compare-and-set that drops only the timer its row
//! named, never writing `cruise` over what someone else stored. The rows
//! built again keep the places the shown rows hold ([`keep_places`]: a row
//! that left leaves a blank [`MenuRow::Gap`]), so no row moves under a still
//! pointer; where a row arrives and the rest must move, a press on a choice
//! the rows moved under since the pointer last moved chooses nothing
//! ([`AutopilotMenu::press_lands`]).
//!
//! **Cost.** Nothing at rest: closed, the menu is one `Option` the frame's
//! repaint key and the key path test. Opening and closing it are one frame
//! each; the rows are built at the open, and again only when the session's
//! pair moves under it.

use aterm_agent::supervise::Autopilot;
use aterm_core::terminal::RenderCell;
use aterm_render::Theme;

use crate::autopilot::AutopilotView;
use crate::autopilot_timers::TimerChoice;
use crate::claude_lights::LandAtGlass;
use crate::tab_menu::{MenuRect, display_cells, ellipsize};
use crate::{App, WindowId};

/// The timed Cruises the menu and Settings offer, in minutes (§9.3.3).
pub(crate) const LAND_IN_MINUTES: [u32; 3] = [60, 120, 240];

/// Cells of breathing room inside each vertical border.
const H_PAD: usize = 1;
/// The narrowest and widest the card's content may be, in cells.
const MIN_CONTENT: usize = 24;
const MAX_CONTENT: usize = 44;
/// The glyph column: `∞⤓` and a space.
const GLYPH_CELLS: usize = 3;
/// The checked mark (`●`) and the space before it.
const CHECK_CELLS: usize = 2;

/// What a row does when chosen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MenuAct {
    /// Write the session's own word (the chip's write; a hand write drops a
    /// timer).
    Mode(Autopilot),
    /// A timed Cruise, or its cancel.
    Timer(TimerChoice),
    /// Open Settings ▸ Harness.
    Settings,
}

impl MenuAct {
    /// The row's stable key (`controls autopilot-menu`, §9.3.3).
    #[must_use]
    pub(crate) fn key(self) -> String {
        match self {
            Self::Mode(mode) => format!("autopilot-menu/mode/{}", mode.word()),
            Self::Timer(TimerChoice::In(minutes)) => format!("autopilot-menu/land-in/{minutes}"),
            Self::Timer(TimerChoice::At) => "autopilot-menu/land-at".to_string(),
            Self::Timer(TimerChoice::Cancel) => "autopilot-menu/land-cancel".to_string(),
            Self::Settings => "autopilot-menu/settings".to_string(),
        }
    }
}

/// One row of the menu.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum MenuRow {
    /// The menu's name: `Autopilot · tab 3 “fix the footer”`.
    Header(String),
    /// Why the rows below are off: `harness off in Settings`, or the agent's
    /// own switch by name (`Supervise Claude Code sessions is off`).
    Note(String),
    /// A rule between groups.
    Rule,
    /// The place of a choice that left while the menu was open (the timer
    /// fired, or was cancelled elsewhere): blank, inert, and as wide as the
    /// row it held (`.0`, its cells in [`content_width`]), so every row that
    /// remains stays on the frame row it was on ([`keep_places`]). Never in
    /// a menu built at the open.
    Gap(usize),
    /// A choice.
    Item {
        glyph: &'static str,
        label: String,
        /// What a screen reader says: words, never a glyph.
        spoken: String,
        act: MenuAct,
        enabled: bool,
        /// The mode the session runs under (`●`).
        checked: bool,
    },
}

impl MenuRow {
    /// Whether a click or Return can choose it.
    #[must_use]
    pub(crate) fn selectable(&self) -> bool {
        matches!(self, Self::Item { enabled: true, .. })
    }

    /// Its text as drawn, the glyph column padded (the check mark is drawn
    /// apart, flush right).
    fn text(&self) -> String {
        match self {
            Self::Header(text) | Self::Note(text) => text.clone(),
            Self::Rule | Self::Gap(_) => String::new(),
            Self::Item { glyph, label, .. } => {
                let pad = GLYPH_CELLS.saturating_sub(display_cells(glyph));
                format!("{glyph}{}{label}", " ".repeat(pad))
            }
        }
    }
}

/// What the menu is built from, read once at the open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MenuFacts {
    /// The menu's name.
    pub(crate) title: String,
    /// Its name as a screen reader says it, in words
    /// (`Autopilot, tab 3, fix the footer`).
    pub(crate) spoken: String,
    /// The session's effective Autopilot (the checked row).
    pub(crate) view: AutopilotView,
    /// Whether the harness carries a mode here: off under the master
    /// switch, an agent's own switch, or with no host.
    pub(crate) carries: bool,
    /// `[harness] land_at` as it arms if chosen now
    /// ([`crate::autopilot_timers::offer_glass`]), offered as "Cruise until
    /// 18:00, then land" — "Cruise until Sat 18:00, then land" once today's
    /// 18:00 has passed.
    pub(crate) land_at: Option<LandAtGlass>,
    /// The session's armed instant, whether or not a local face is available.
    pub(crate) armed_at_ms: Option<i64>,
    /// Its optional local face, used only to name the cancel row.
    pub(crate) armed: Option<LandAtGlass>,
}

/// A clock time as the title and the page write it: `18:00`, or `Sat 16:00`
/// on another day.
pub(crate) fn when(at: LandAtGlass) -> String {
    match at.other_day {
        Some(day) => format!("{} {}", day.short(), at.clock.as_str()),
        None => at.clock.as_str().to_string(),
    }
}

/// [`when`] as a screen reader says it: `18:00`, or `Saturday 16:00`.
pub(crate) fn spoken_when(at: LandAtGlass) -> String {
    match at.other_day {
        Some(day) => format!("{} {}", day.long(), at.clock.as_str()),
        None => at.clock.as_str().to_string(),
    }
}

/// THE MODEL (§9.3.3): the header, the three modes (the effective one
/// checked), the timed Cruises (1 h, 2 h, 4 h; until `[harness] land_at`
/// where it is set; the cancel while a timer is armed) and Settings. Where
/// the harness carries no mode here, the mode and timer rows are disabled
/// under `harness off in Settings` (or, under the agent's own switch, that
/// switch by name), and only Settings acts. Pure.
#[must_use]
pub(crate) fn rows(facts: &MenuFacts) -> Vec<MenuRow> {
    let on = facts.carries;
    let mut out = vec![MenuRow::Header(facts.title.clone())];
    if !on {
        // Why, as the chip's title says it: an agent's own switch by name.
        let why = match facts.view {
            AutopilotView::AgentOff(agent) => crate::autopilot::agent_switch_off(agent),
            _ => "harness off in Settings",
        };
        out.push(MenuRow::Note(why.to_string()));
    }
    for (mode, glyph, label) in [
        (Autopilot::Cruise, "\u{221e}", "Cruise"),
        (Autopilot::Land, "\u{2913}", "Land now"),
        (Autopilot::Off, "\u{25cb}", "Off"),
    ] {
        out.push(MenuRow::Item {
            glyph,
            label: label.to_string(),
            spoken: label.to_string(),
            act: MenuAct::Mode(mode),
            enabled: on,
            checked: on && facts.view.mode() == Some(mode),
        });
    }
    out.push(MenuRow::Rule);
    for minutes in LAND_IN_MINUTES {
        let hours = minutes / 60;
        let span = if hours == 1 {
            "1 hour".to_string()
        } else {
            format!("{hours} hours")
        };
        out.push(MenuRow::Item {
            glyph: "\u{221e}\u{2913}",
            label: format!("Cruise {span}, then land"),
            spoken: format!("Cruise for {span}, then land"),
            act: MenuAct::Timer(TimerChoice::In(minutes)),
            enabled: on,
            checked: false,
        });
    }
    if let Some(at) = facts.land_at {
        out.push(MenuRow::Item {
            glyph: "\u{221e}\u{2913}",
            label: format!("Cruise until {}, then land", when(at)),
            spoken: format!("Cruise until {}, then land", spoken_when(at)),
            act: MenuAct::Timer(TimerChoice::At),
            enabled: on,
            checked: false,
        });
    }
    if facts.armed_at_ms.is_some() {
        out.push(MenuRow::Item {
            glyph: "\u{2715}",
            label: facts.armed.map_or_else(
                || "Cancel landing".to_string(),
                |armed| format!("Cancel landing at {}", when(armed)),
            ),
            spoken: facts.armed.map_or_else(
                || "Cancel landing".to_string(),
                |armed| format!("Cancel landing at {}", spoken_when(armed)),
            ),
            act: MenuAct::Timer(TimerChoice::Cancel),
            enabled: on,
            checked: false,
        });
    }
    out.push(MenuRow::Rule);
    out.push(MenuRow::Item {
        glyph: "\u{2699}",
        label: "Harness settings\u{2026}".to_string(),
        spoken: "Harness settings".to_string(),
        act: MenuAct::Settings,
        enabled: true,
        checked: false,
    });
    out
}

/// Whether `a` offers the choice `b` offered, in the same words: a lit row
/// stays lit across a rebuild only where Return would still do what it said.
fn same_choice(a: &MenuRow, b: &MenuRow) -> bool {
    match (a, b) {
        (
            MenuRow::Item {
                act: act_a,
                label: label_a,
                enabled: enabled_a,
                ..
            },
            MenuRow::Item {
                act: act_b,
                label: label_b,
                enabled: enabled_b,
                ..
            },
        ) => act_a == act_b && label_a == label_b && enabled_a == enabled_b,
        _ => false,
    }
}

/// The cells one row asks of the card: its text, and room for the check
/// mark on a choice; a gap keeps what its row asked.
fn row_width(row: &MenuRow) -> usize {
    match row {
        MenuRow::Gap(cells) => *cells,
        MenuRow::Item { .. } => display_cells(&row.text()) + CHECK_CELLS,
        _ => display_cells(&row.text()),
    }
}

/// The card's content width: the widest row with room for the check mark,
/// floored and capped.
fn content_width(rows: &[MenuRow]) -> usize {
    rows.iter()
        .map(row_width)
        .max()
        .unwrap_or(0)
        .clamp(MIN_CONTENT, MAX_CONTENT)
}

/// Whether `fresh` may stand where `shown` stood: the same kind of row, and
/// for a choice the same act (its words, check and state may have moved).
fn same_place(shown: &MenuRow, fresh: &MenuRow) -> bool {
    match (shown, fresh) {
        (MenuRow::Header(_), MenuRow::Header(_))
        | (MenuRow::Note(_), MenuRow::Note(_))
        | (MenuRow::Rule, MenuRow::Rule) => true,
        (MenuRow::Item { act: a, .. }, MenuRow::Item { act: b, .. }) => a == b,
        _ => false,
    }
}

/// THE ROWS STAY WHERE THEY WERE (lifecycle-1): `fresh`, the rows built again
/// under an open menu, laid into the places the `shown` rows hold — a row that
/// left leaves a [`MenuRow::Gap`] as wide as it was — so a choice that remains
/// stays on the frame row the pointer found it on, and the card keeps its
/// rect. `None` where `fresh` holds a row with no place among `shown` (one
/// arrived: the card must grow, and is laid out afresh). Pure.
#[must_use]
pub(crate) fn keep_places(shown: &[MenuRow], fresh: &[MenuRow]) -> Option<Vec<MenuRow>> {
    let mut next = fresh.iter().peekable();
    let laid = shown
        .iter()
        .map(|old| {
            next.next_if(|new| same_place(old, new))
                .cloned()
                .unwrap_or_else(|| MenuRow::Gap(row_width(old)))
        })
        .collect();
    next.peek().is_none().then_some(laid)
}

/// THE GEOMETRY: the card hangs ABOVE the chip at frame cell `(chip_row,
/// chip_col)`, its left edge at the chip's column (shifted left only to
/// stay whole), never over the first `top` rows (the tab strip); it opens
/// downward only where fewer rows are above, and where neither side holds it
/// whole it takes the larger and shows a prefix. `None` where the frame
/// cannot host a card at all.
#[must_use]
pub(crate) fn place(
    rows: &[MenuRow],
    chip_row: usize,
    chip_col: usize,
    cols: usize,
    frame_rows: usize,
    top: usize,
) -> Option<MenuRect> {
    let chrome_w = 2 + 2 * H_PAD;
    if rows.is_empty() || cols <= chrome_w || chip_row >= frame_rows {
        return None;
    }
    let w = (content_width(rows) + chrome_w).min(cols);
    let col = chip_col.min(cols - w);
    let want = rows.len() + 2;
    let above = chip_row.saturating_sub(top);
    let below = frame_rows - chip_row - 1;
    let (row, h) = if above >= want || above >= below {
        let h = want.min(above);
        (chip_row - h, h)
    } else {
        (chip_row + 1, want.min(below))
    };
    // A border pair and one row, or no card.
    (h >= 3).then_some(MenuRect { row, col, w, h })
}

/// The row frame `row` shows, or `None` for a border or past a truncation.
#[must_use]
pub(crate) fn row_at(rect: MenuRect, frame_row: usize) -> Option<usize> {
    if frame_row <= rect.row || frame_row + 1 >= rect.row + rect.h {
        return None;
    }
    Some(frame_row - rect.row - 1)
}

/// The row a click at frame `(row, col)` chooses: a shown, selectable row.
#[must_use]
pub(crate) fn item_at(rect: MenuRect, rows: &[MenuRow], row: usize, col: usize) -> Option<usize> {
    if !rect.contains(row, col) {
        return None;
    }
    let i = row_at(rect, row)?;
    rows.get(i).filter(|r| r.selectable()).map(|_| i)
}

/// Step the highlight one selectable row (`down`, else up), wrapping; from
/// none, the first (or last). Only rows a truncated card shows (`shown`).
#[must_use]
pub(crate) fn step(
    rows: &[MenuRow],
    from: Option<usize>,
    down: bool,
    shown: usize,
) -> Option<usize> {
    let live: Vec<usize> = rows
        .iter()
        .take(shown)
        .enumerate()
        .filter(|(_, r)| r.selectable())
        .map(|(i, _)| i)
        .collect();
    let (first, last) = (*live.first()?, *live.last()?);
    let Some(at) = from.and_then(|f| live.iter().position(|&i| i == f)) else {
        return Some(if down { first } else { last });
    };
    Some(if down {
        live[(at + 1) % live.len()]
    } else {
        live[(at + live.len() - 1) % live.len()]
    })
}

/// What a key means to the open menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MenuKey {
    Up,
    Down,
    Choose,
    Close,
    /// Any other key: the menu closes and the key goes on.
    PassThrough,
    /// A bare modifier: nothing.
    Inert,
}

/// The menu's keys (§9.3.3): `↑`/`↓`/Return/Esc; a bare modifier is
/// nothing; everything else closes the menu and passes through.
#[must_use]
pub(crate) fn key_of(key: &aterm_types::keyboard::Key) -> MenuKey {
    use aterm_types::keyboard::{Key, NamedKey as Nk};
    if aterm_types::keyboard::is_modifier_or_lock_key(key) {
        return MenuKey::Inert;
    }
    match key {
        Key::Named(Nk::ArrowUp | Nk::NumpadArrowUp) => MenuKey::Up,
        Key::Named(Nk::ArrowDown | Nk::NumpadArrowDown) => MenuKey::Down,
        Key::Named(Nk::Enter | Nk::NumpadEnter) => MenuKey::Choose,
        Key::Named(Nk::Escape) => MenuKey::Close,
        _ => MenuKey::PassThrough,
    }
}

/// THE PAINT: exactly `rect.h` rows of `rect.w` cells, the tab menu's card
/// (its ground, rules, ink and selection), a disabled row in the disabled
/// ink, the checked row's `●` flush right.
#[must_use]
pub(crate) fn paint(
    rect: MenuRect,
    rows: &[MenuRow],
    highlight: Option<usize>,
    theme: Theme,
) -> Vec<Vec<RenderCell>> {
    let c = crate::tab_menu::menu_colors(theme);
    let w = rect.w;
    let text_start = 1 + H_PAD;
    let text_end = w.saturating_sub(1 + H_PAD);
    let cell = crate::chrome_band::cell;
    let edge = |left: char, fill: char, right: char| {
        let mut row = vec![cell(fill, c.rule, c.card_bg, false, false); w];
        if let Some(first) = row.first_mut() {
            *first = cell(left, c.rule, c.card_bg, false, false);
        }
        if w >= 2 {
            row[w - 1] = cell(right, c.rule, c.card_bg, false, false);
        }
        row
    };
    let mut out = Vec::with_capacity(rect.h);
    out.push(edge('\u{250C}', '\u{2500}', '\u{2510}'));
    for (i, row) in rows.iter().enumerate().take(rect.h.saturating_sub(2)) {
        if matches!(row, MenuRow::Rule) {
            out.push(edge('\u{251C}', '\u{2500}', '\u{2524}'));
            continue;
        }
        let selected = highlight == Some(i) && row.selectable();
        let bg = if selected { c.sel_bg } else { c.card_bg };
        let (fg, bold) = match row {
            MenuRow::Header(_) => (c.band.value, true),
            MenuRow::Note(_) | MenuRow::Item { enabled: false, .. } => (c.disabled, false),
            MenuRow::Item { .. } if selected => (c.sel_fg, true),
            _ => (c.band.value, false),
        };
        let mut cells = vec![cell(' ', fg, bg, false, false); w];
        cells[0] = cell('\u{2502}', c.rule, c.card_bg, false, false);
        if w >= 2 {
            cells[w - 1] = cell('\u{2502}', c.rule, c.card_bg, false, false);
        }
        let checked = matches!(row, MenuRow::Item { checked: true, .. });
        let room = text_end
            .saturating_sub(text_start)
            .saturating_sub(if checked { CHECK_CELLS } else { 0 });
        crate::tab_menu::put_text(
            &mut cells,
            text_start,
            text_end,
            &ellipsize(&row.text(), room),
            fg,
            bg,
            bold,
        );
        if checked && text_end >= 1 {
            cells[text_end - 1] = cell('\u{25cf}', fg, bg, bold, false);
        }
        out.push(cells);
    }
    if out.len() < rect.h {
        out.push(edge('\u{2514}', '\u{2500}', '\u{2518}'));
    }
    out.truncate(rect.h);
    out
}

/// One open Autopilot menu (one at a time, in one window).
#[derive(Clone, Debug)]
pub(crate) struct AutopilotMenu {
    /// The window it is open in.
    pub(crate) wid: WindowId,
    /// The session whose chip opened it: every choice is for it.
    pub(crate) session: u64,
    /// The window's focused session when it opened: a focus or tab change
    /// moves it, and the menu closes.
    pub(crate) focus: Option<u64>,
    /// The chip's first cell, frame coordinates: the anchor, and what each
    /// painted frame checks is still the session's Autopilot chip.
    pub(crate) anchor: (usize, usize),
    /// The rows, built at the open and again whenever the session's pair
    /// moves under the open menu ([`App::refresh_autopilot_menu`]).
    pub(crate) rows: Vec<MenuRow>,
    /// The menu's name in words, read with the rows ([`MenuFacts::spoken`]):
    /// what its accessibility node is called.
    pub(crate) spoken: String,
    /// The instant its cancel row names, unix ms (the index's, read with the
    /// rows): the cancel drops that timer and no other.
    pub(crate) cancels: Option<i64>,
    /// The highlighted row: the first choice for a keyboard open, none for
    /// a pointer's.
    pub(crate) highlight: Option<usize>,
    /// Where the last frame painted it, for the hit test; `None` before the
    /// first frame.
    pub(crate) rect: Option<MenuRect>,
    /// How many times the rows moved under the open menu (0 at the open).
    pub(crate) generation: u64,
    /// Where the pointer last moved to while the menu was open, and what it
    /// rested on there ([`Self::note_pointer`]); at the open, the pixel it
    /// already held, with nothing read.
    pub(crate) rest: PointerRest,
}

/// Where the pointer last MOVED to while the menu was open: the window pixel
/// (`None` where the window held no position), the frame row there and the
/// choice the card showed on it (`None` for both until the pointer moved
/// over a painted card; a choice `None` over a border, the header, a rule, a
/// gap or off the card), and the rows' [`AutopilotMenu::generation`] it was
/// read against. Keyed on the pixel, not on the event: a motion event at the
/// pixel the pointer already rests on — AppKit queues one ahead of every
/// press and every wheel event — is no motion and reads nothing.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PointerRest {
    px: Option<(f64, f64)>,
    frame_row: Option<usize>,
    choice: Option<Choice>,
    generation: u64,
}

impl PointerRest {
    /// The rest at an open: the pixel the pointer holds, nothing read, the
    /// rows' first generation.
    pub(crate) fn at_open(px: Option<(f64, f64)>) -> Self {
        Self {
            px,
            frame_row: None,
            choice: None,
            generation: 0,
        }
    }
}

/// A choice as the person saw it: what it does, its words, whether it was on
/// — [`same_choice`]'s fields.
type Choice = (MenuAct, String, bool);

impl AutopilotMenu {
    /// Row `i` as a [`Choice`], where it is one.
    fn choice_at(&self, i: usize) -> Option<Choice> {
        match self.rows.get(i)? {
            MenuRow::Item {
                act,
                label,
                enabled,
                ..
            } => Some((*act, label.clone(), *enabled)),
            _ => None,
        }
    }

    /// The pointer is at window pixel `px`, on frame row `frame_row`
    /// (`None` off every frame cell), over row `hovered` of the painted card
    /// where that is a choice. Where it MOVED there — another pixel than the
    /// one it rested on — what it sees there is noted against the rows'
    /// generation now; a motion event at the same pixel is no motion and
    /// reads nothing, whatever the rows did meanwhile.
    fn note_pointer(&mut self, px: (f64, f64), frame_row: Option<usize>, hovered: Option<usize>) {
        if self.rest.px != Some(px) {
            self.rest_on(px, frame_row, hovered);
        }
    }

    /// Note what the pointer at `px` sees now, moved or not: a refused press
    /// shows the person the row now under it, and the next press may take it.
    fn rest_on(&mut self, px: (f64, f64), frame_row: Option<usize>, hovered: Option<usize>) {
        self.rest = PointerRest {
            px: Some(px),
            frame_row,
            choice: hovered.and_then(|i| self.choice_at(i)),
            generation: self.generation,
        };
    }

    /// THE PRESS GOES WHERE THE POINTER LOOKED (lifecycle-1): whether a
    /// press on frame row `frame_row`, over row `i`, chooses it. Yes where
    /// the rows have not moved since the pointer last moved (or since the
    /// open); else only where the pointer last moved onto that same frame
    /// row and the card showed there the very choice it shows now. A press
    /// the rows moved under is refused, and the row now under it lit
    /// ([`App::autopilot_menu_press`]), as the reader's click from moved rows
    /// is refused by its epoch — a pointer still since the open, which read
    /// nothing, included.
    fn press_lands(&self, frame_row: usize, i: usize) -> bool {
        let rest = &self.rest;
        rest.generation == self.generation
            || (rest.frame_row == Some(frame_row)
                && rest.choice.is_some()
                && rest.choice == self.choice_at(i))
    }

    /// Refresh the owned menu snapshot, as the App's meta/timer wake does.
    /// Whether its visible rows moved and therefore need another painted frame.
    /// The rows that remain keep their places ([`keep_places`]) — and the
    /// card its rect, where it asks no other width — so nothing moves under a
    /// still pointer; only where a row arrived is the card laid out afresh,
    /// and the hit test waits for the frame that paints it.
    fn refresh_rows(&mut self, fresh: Vec<MenuRow>, spoken: String, cancels: Option<i64>) -> bool {
        self.cancels = cancels;
        let kept = keep_places(&self.rows, &fresh);
        let stays = kept.is_some();
        let laid = kept.unwrap_or(fresh);
        if self.rows == laid {
            return false;
        }
        self.spoken = spoken;
        let lit = self.highlight.and_then(|i| self.rows.get(i));
        self.highlight = lit.and_then(|lit| laid.iter().position(|row| same_choice(row, lit)));
        if !stays || content_width(&laid) != content_width(&self.rows) {
            self.rect = None;
        }
        self.rows = laid;
        self.generation = self.generation.wrapping_add(1);
        true
    }

    /// `controls autopilot-menu`: the open card's rows, their stable keys,
    /// what each shows and whether it is on, checked and highlighted.
    #[must_use]
    pub(crate) fn controls_lines(&self, sid: &str) -> Vec<String> {
        let mut out = vec![format!(
            "autopilot-menu open=true session={sid} rows={} selected={}",
            self.rows.len(),
            self.highlight
                .map_or_else(|| "none".to_string(), |i| i.to_string())
        )];
        for (i, row) in self.rows.iter().enumerate() {
            match row {
                MenuRow::Item {
                    act,
                    enabled,
                    checked,
                    ..
                } => out.push(format!(
                    "autopilot-menu item index={i} key={} enabled={enabled} checked={checked} \
                     selected={} text={:?}",
                    act.key(),
                    self.highlight == Some(i),
                    row.text()
                )),
                MenuRow::Rule => out.push(format!("autopilot-menu rule index={i}")),
                MenuRow::Gap(_) => out.push(format!("autopilot-menu gap index={i}")),
                MenuRow::Header(text) | MenuRow::Note(text) => {
                    out.push(format!("autopilot-menu label index={i} text={text:?}"));
                }
            }
        }
        out
    }

    /// THE ROWS' ACCESSIBILITY EPOCH (`a11y_tree::GridMenu::epoch`): its
    /// session, its name, the exact instant its cancel names and its rows
    /// (each row's words, choice, state and check) folded. A timer replacement
    /// is an identity input even where its visible minute face stays equal.
    /// This is a compact 22-bit hash, not a collision-free generation.
    /// The highlight is not in it: `↓` moves the tree's focus
    /// and keeps every id. Computed where the tree is built and where a
    /// reader's click arrives, never on the frame's own path.
    #[cfg(a11y_tree)]
    #[must_use]
    fn a11y_epoch(&self) -> u32 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.session.hash(&mut h);
        self.spoken.hash(&mut h);
        self.cancels.hash(&mut h);
        self.rows.hash(&mut h);
        crate::a11y_tree::menu_epoch_from(h.finish())
    }

    /// Repaint fingerprint: never 0 (closed is 0).
    #[must_use]
    fn fingerprint(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.session.hash(&mut h);
        self.anchor.hash(&mut h);
        self.highlight.hash(&mut h);
        self.rows.hash(&mut h);
        h.finish() | 1
    }
}

impl std::hash::Hash for MenuRow {
    fn hash<H: std::hash::Hasher>(&self, h: &mut H) {
        match self {
            Self::Header(t) => (0u8, t).hash(h),
            Self::Note(t) => (1u8, t).hash(h),
            Self::Rule => 2u8.hash(h),
            Self::Gap(cells) => (4u8, cells).hash(h),
            Self::Item {
                label,
                act,
                enabled,
                checked,
                ..
            } => (3u8, label, act.key(), enabled, checked).hash(h),
        }
    }
}

/// The open menu's repaint term for window `wid`: 0 with none open there,
/// so a frame at rest pays one `Option` test. It moves on the open, the
/// close and every highlight step, none of which dirty a terminal cell.
#[must_use]
pub(crate) fn fingerprint(menu: Option<&AutopilotMenu>, wid: WindowId) -> u64 {
    menu.filter(|menu| menu.wid == wid)
        .map_or(0, AutopilotMenu::fingerprint)
}

impl App {
    /// The facts session `session`'s menu is built from, read once.
    fn autopilot_menu_facts(&self, session: u64) -> MenuFacts {
        let (_, view, _) = self.session_autopilot(session);
        let sid = self
            .pool
            .get(session)
            .map(|s| s.ctx.self_id.as_str().to_string())
            .unwrap_or_default();
        // Its pane named where its tab holds two agents (`tab 1, left pane`),
        // as its SESSIONS row names it (§10.3.3, item 9).
        let place = self.session_place(&sid);
        let place = place.strip_prefix("in ").unwrap_or(&place).to_string();
        // The page's own name for the session: the person's title, else the
        // terminal's live one without its spinner frame (the menu's name is
        // built once, at the open) — read from the pane's terminal, never the
        // store's copy, which only the front pane's window title writes.
        let name = self.pool.get(session).and_then(|s| {
            let person = s
                .ctx
                .meta
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .presentation_value("title");
            let stored = self
                .store
                .read()
                .unwrap_or_else(|p| p.into_inner())
                .by_local(session)
                .map(|h| h.title.clone())
                .unwrap_or_default();
            self.harness_session_name(session, person, &stored)
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty())
        });
        let (title, spoken) = match name {
            Some(name) => (
                format!("Autopilot \u{b7} {place} \u{201c}{name}\u{201d}"),
                format!("Autopilot, {place}, {name}"),
            ),
            None => (
                format!("Autopilot \u{b7} {place}"),
                format!("Autopilot, {place}"),
            ),
        };
        MenuFacts {
            title,
            spoken,
            view,
            carries: view.mode().is_some(),
            // One wall read at the open, only with `land_at` set.
            land_at: self.harness_policy.get().policy.land_at.map(|at| {
                crate::autopilot_timers::offer_glass(
                    at,
                    crate::autopilot_timers::wall_ms(),
                    &crate::autopilot_timers::local_offset_at,
                )
            }),
            armed_at_ms: self.autopilot_timers.land_at_ms(session),
            armed: self.autopilot_timers.glass(session),
        }
    }

    /// OPEN session `session`'s menu in window `wid`, hung from its
    /// Autopilot chip's first cell `anchor` (frame row, column). A keyboard
    /// open lights the first choice; a pointer's lights none. Any menu open
    /// elsewhere closes. One frame.
    pub(crate) fn open_autopilot_menu(
        &mut self,
        wid: WindowId,
        session: u64,
        anchor: (usize, usize),
        keyboard: bool,
    ) {
        self.close_autopilot_menu();
        // When the `land_at` row's words turn, noted before they are read.
        self.note_land_at_offer();
        let facts = self.autopilot_menu_facts(session);
        let rows = rows(&facts);
        let highlight = keyboard
            .then(|| step(&rows, None, true, rows.len()))
            .flatten();
        let focus = self.focused_session_id(wid);
        let px = self
            .windows
            .get(&wid)
            .filter(|ws| ws.pointer_position_known)
            .map(|ws| ws.last_cursor_px);
        self.autopilot_menu = Some(AutopilotMenu {
            wid,
            session,
            focus,
            anchor,
            rows,
            spoken: facts.spoken,
            cancels: self.autopilot_timers.land_at_ms(session),
            highlight,
            rect: None,
            generation: 0,
            rest: PointerRest::at_open(px),
        });
        self.request_menu_frame(wid);
    }

    /// THE OPEN MENU FOLLOWS ITS SESSION'S PAIR: session `local_id`'s word or
    /// timer moved under it (a wire `meta set`, the timer's fire, Settings,
    /// a re-derived face), so its rows are built again from what is stored
    /// now — the checked mode, the cancel row and the instant it names — and
    /// never offer what no longer is. The highlight stays on a choice still
    /// offered in the same words; otherwise none is lit, and Return closes
    /// the menu without acting. The hit test waits for the frame that paints
    /// the new rows. Nothing (one `Option` test) unless a menu is open on
    /// `local_id`; nothing more where its rows did not move.
    pub(crate) fn refresh_autopilot_menu(&mut self, local_id: u64) {
        if !self
            .autopilot_menu
            .as_ref()
            .is_some_and(|menu| menu.session == local_id)
        {
            return;
        }
        self.note_land_at_offer();
        let facts = self.autopilot_menu_facts(local_id);
        let fresh = rows(&facts);
        let cancels = self.autopilot_timers.land_at_ms(local_id);
        let Some(menu) = self.autopilot_menu.as_mut() else {
            return;
        };
        if !menu.refresh_rows(fresh, facts.spoken, cancels) {
            return;
        }
        let wid = menu.wid;
        self.request_menu_frame(wid);
    }

    /// Close the open menu, wherever it is. Whether one was open (its
    /// window owes the frame that erases it).
    pub(crate) fn close_autopilot_menu(&mut self) -> bool {
        let Some(menu) = self.autopilot_menu.take() else {
            return false;
        };
        self.request_menu_frame(menu.wid);
        true
    }

    /// Ask `wid`'s window, where it has one, for a frame.
    fn request_menu_frame(&self, wid: WindowId) {
        if let Some(window) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref()) {
            window.request_redraw();
        }
    }

    /// The frame cell `(row, col)`'s centre, window pixels: the inverse of
    /// `App::frame_cell_at`.
    fn frame_cell_centre(&self, wid: WindowId, (row, col): (usize, usize)) -> (f64, f64) {
        let (cw, ch) = self.win_cell_size(wid);
        let (cw, ch) = (cw.max(1), ch.max(1));
        let pad = self.win_pad(wid);
        let top = self.win_pad_top(wid) + self.win_head(wid);
        let (ox, oy) = self.frame_origin(wid);
        (
            ox as f64 + (pad + col * cw) as f64 + cw as f64 / 2.0,
            oy as f64 + (top + row * ch) as f64 + ch as f64 / 2.0,
        )
    }

    /// Whether the frame cell shows `session`'s Autopilot chip on this frame's
    /// glass (`App::claude_chips_at`, which a band claiming the row hides).
    fn shows_autopilot_chip(&self, wid: WindowId, session: u64, cell: (usize, usize)) -> bool {
        let (x, y) = self.frame_cell_centre(wid, cell);
        self.claude_chips_at(wid, x, y)
            == Some((session, Some(crate::claude_lights::Chip::Autopilot)))
    }

    /// A press on `session`'s Autopilot chip at window pixel `(x, y)` that
    /// asks for the menu (a right-click, a ctrl-click on macOS): opened,
    /// hung from the chip's first cell. Whether it opened.
    pub(crate) fn open_autopilot_menu_at(&mut self, wid: WindowId, x: f64, y: f64) -> bool {
        let Some((session, Some(crate::claude_lights::Chip::Autopilot))) =
            self.claude_chips_at(wid, x, y)
        else {
            return false;
        };
        let Some((row, mut col)) = self.frame_cell_at(wid, x, y) else {
            return false;
        };
        while col > 0 && self.shows_autopilot_chip(wid, session, (row, col - 1)) {
            col -= 1;
        }
        self.open_autopilot_menu(wid, session, (row, col), false);
        true
    }

    /// `open autopilot-menu`: the front window's focused session's menu, hung
    /// from its Autopilot chip as this frame's glass shows it (the first
    /// cell, scanned once — a wire call, never a frame's), its first choice
    /// lit as for a keyboard open.
    pub(crate) fn open_front_autopilot_menu(&mut self) -> Result<(), String> {
        let wid = self
            .frontmost_window
            .ok_or_else(|| "no front window to open the Autopilot menu on".to_string())?;
        let session = self
            .focused_session_id(wid)
            .ok_or_else(|| "the front window has no focused session".to_string())?;
        let (rows, cols) = self.windows.get(&wid).map_or((0, 0), |ws| {
            (ws.input_scratch.cells.len(), ws.input_scratch.cols)
        });
        let anchor = (0..rows)
            .flat_map(|row| (0..cols).map(move |col| (row, col)))
            .find(|&cell| self.shows_autopilot_chip(wid, session, cell))
            .ok_or_else(|| "the focused session shows no Autopilot chip".to_string())?;
        self.open_autopilot_menu(wid, session, anchor, true);
        Ok(())
    }

    /// THE SPLICE, once a frame after the footer's (whose hits it reads):
    /// paint the open menu of window `wid` over its frame cells and record
    /// where, for the hit test — or CLOSE it where this frame's glass no
    /// longer shows its session's Autopilot chip at the anchor, the window's
    /// focus moved (a pane or a tab change) since it opened, or no card fits.
    /// Nothing with no menu open.
    ///
    /// The menu's card is the last of the host's chrome over the grid the
    /// resident pet must not sit on, after the footer's write: its cells are
    /// noted, and the pass ends here (`crate::pet_chrome`) — a pass whose
    /// cells moved under a pet that read the old ones asks for the frame that
    /// shows them to it.
    pub(crate) fn splice_autopilot_menu(&mut self, wid: WindowId) {
        let rect = self.paint_autopilot_menu(wid);
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        if let Some(rect) = rect {
            ws.pet_chrome.note(rect.row, rect.col, rect.h, rect.w);
        }
        if ws.pet_chrome.commit()
            && let Some(window) = ws.os_window.as_ref()
        {
            window.request_redraw();
        }
    }

    /// [`Self::splice_autopilot_menu`]'s paint: the card's frame rect where
    /// one was painted.
    fn paint_autopilot_menu(&mut self, wid: WindowId) -> Option<crate::tab_menu::MenuRect> {
        let menu = self.autopilot_menu.as_ref().filter(|m| m.wid == wid)?;
        let (session, anchor, focus) = (menu.session, menu.anchor, menu.focus);
        if self.focused_session_id(wid) != focus || !self.shows_autopilot_chip(wid, session, anchor)
        {
            self.close_autopilot_menu();
            return None;
        }
        let top = usize::from(self.chrome_rows(wid));
        let theme = self.overlay_card_theme(wid);
        let ((cw, ch), pad) = (self.win_cell_size(wid), self.win_pad(wid));
        let menu = self.autopilot_menu.as_ref()?;
        let ws = self.windows.get(&wid)?;
        let rect = place(
            &menu.rows,
            anchor.0,
            anchor.1,
            ws.input_scratch.cols,
            ws.input_scratch.cells.len(),
            top,
        );
        let Some(rect) = rect else {
            self.close_autopilot_menu();
            return None;
        };
        let painted = paint(rect, &menu.rows, menu.highlight, theme);
        let ws = self.windows.get_mut(&wid)?;
        let ground = crate::tab_menu::ground_cell(theme, ws.input_scratch.default_bg);
        let right = rect.col + rect.w;
        for (r, row) in painted.into_iter().enumerate() {
            let frame_row = rect.row + r;
            let Some(cells) = ws.input_scratch.cells.get_mut(frame_row) else {
                break;
            };
            if cells.len() < right {
                cells.resize(right, ground);
            }
            for (c, cell) in row.into_iter().enumerate() {
                if let Some(slot) = cells.get_mut(rect.col + c) {
                    *slot = cell;
                }
            }
            let span = rect.col..right;
            if let Some(cl) = ws.input_scratch.clusters.get_mut(frame_row) {
                cl.retain(|(col, _)| !span.contains(col));
            }
            if let Some(cb) = ws.input_scratch.combining.get_mut(frame_row) {
                cb.retain(|(col, _)| !span.contains(col));
            }
            if let Some(im) = ws.input_scratch.images.get_mut(frame_row) {
                im.retain(|(col, _)| !span.contains(col));
            }
        }
        crate::claude_card::cut_out(&mut ws.claude_card, &mut ws.input_scratch, rect, cw, pad);
        // THE CARD IS ON TOP: the caret under it, the resident pet and every
        // other overlay yield its cells (`scrub_overlay_card`) — the pet's
        // seat avoids them from the next frame (`crate::pet_chrome`), this
        // cut covers the frames before it moves.
        crate::app_render::scrub_overlay_card(&mut ws.input_scratch, rect, (cw, ch), pad);
        // A host write, as the tab menu's is: every reuse lane refills, and
        // the renderer compares every row — not only the rows whose engine
        // revision moved, which painted the card in strips over rows the
        // program left alone.
        ws.input_scratch.snapshot_seq = ws.input_scratch.snapshot_seq.wrapping_add(1);
        if let Some(menu) = self.autopilot_menu.as_mut() {
            menu.rect = Some(rect);
        }
        Some(rect)
    }

    /// Choose row `i` of the open menu: close it, then act for its session
    /// — a cancel for the very timer its row named.
    pub(crate) fn choose_autopilot_menu_row(&mut self, i: usize) {
        let Some(menu) = self.autopilot_menu.as_ref() else {
            return;
        };
        let (wid, session, cancels) = (menu.wid, menu.session, menu.cancels);
        let Some(MenuRow::Item {
            act, enabled: true, ..
        }) = menu.rows.get(i).cloned()
        else {
            return;
        };
        self.close_autopilot_menu();
        match act {
            MenuAct::Mode(mode) => {
                // A menu's enabled row is a snapshot. Pointer, keyboard and
                // controller choices all recheck the policy in force now.
                if self
                    .session_harness_posture(session)
                    .default_mode()
                    .is_some()
                {
                    self.set_session_autopilot(session, Some(mode));
                }
            }
            MenuAct::Timer(choice) => {
                self.choose_autopilot_timer_naming(session, choice, cancels);
            }
            MenuAct::Settings => {
                self.open_harness_settings(
                    wid,
                    crate::harness_screen::HarnessFocus::Session(session),
                );
            }
        }
    }

    /// A key while the menu is open in window `wid` (§9.3.3): `↑`/`↓` move
    /// the highlight, Return chooses it, Esc closes; a bare modifier is
    /// nothing; any other key closes the menu and goes on. Whether the key
    /// was the menu's.
    pub(crate) fn autopilot_menu_key(
        &mut self,
        wid: WindowId,
        ev: &winit::event::KeyEvent,
    ) -> bool {
        let meaning = if crate::keymap::press_is_inert(ev) {
            MenuKey::Inert
        } else {
            aterm_winit_keymap::map_logical_key(&ev.logical_key)
                .as_ref()
                .map_or(MenuKey::PassThrough, key_of)
        };
        self.autopilot_menu_nav(wid, meaning)
    }

    /// One [`MenuKey`] for the menu open in window `wid`: the sole mutator
    /// behind the key path. Whether the key was the menu's.
    pub(crate) fn autopilot_menu_nav(&mut self, wid: WindowId, meaning: MenuKey) -> bool {
        let Some(menu) = self.autopilot_menu.as_mut().filter(|m| m.wid == wid) else {
            return false;
        };
        let shown = menu.rect.map_or(menu.rows.len(), |r| r.h.saturating_sub(2));
        match meaning {
            MenuKey::Inert => true,
            MenuKey::Up | MenuKey::Down => {
                let down = meaning == MenuKey::Down;
                menu.highlight = step(&menu.rows, menu.highlight, down, shown);
                self.request_menu_frame(wid);
                true
            }
            MenuKey::Choose => {
                match menu.highlight {
                    Some(i) => self.choose_autopilot_menu_row(i),
                    None => {
                        self.close_autopilot_menu();
                    }
                }
                true
            }
            MenuKey::Close => {
                self.close_autopilot_menu();
                true
            }
            MenuKey::PassThrough => {
                self.close_autopilot_menu();
                false
            }
        }
    }

    /// An engine-neutral input event (a controller's `aterm ctl key …`, `App::input`)
    /// while the menu is open in window `wid`: its keys as [`Self::autopilot_menu_nav`]
    /// reads them; a key release is not the menu's; any other input closes it and
    /// goes on. Whether the menu took it.
    pub(crate) fn autopilot_menu_input_event(
        &mut self,
        wid: WindowId,
        ev: &crate::input::InputEvent,
    ) -> bool {
        use crate::input::InputEvent;
        use aterm_types::keyboard::KeyEventType;
        if !self.autopilot_menu.as_ref().is_some_and(|m| m.wid == wid) {
            return false;
        }
        match ev {
            InputEvent::Key {
                event_type: KeyEventType::Release,
                ..
            } => false,
            InputEvent::Key { key, .. } => self.autopilot_menu_nav(wid, key_of(key)),
            _ => {
                self.close_autopilot_menu();
                false
            }
        }
    }

    /// A mouse press while the menu is open in window `wid`, at window pixel
    /// `(x, y)`: a left press on a choice chooses it; a press on the card
    /// elsewhere is swallowed and the card stays; a press anywhere else
    /// closes it and is spent on that (the tab menu's rule: the gesture that
    /// closes a menu never also acts on what is underneath). A press on a
    /// choice the rows moved under since the pointer last moved — the timer
    /// fired, or was armed or cancelled elsewhere, and another choice now
    /// stands where the pointer rests — chooses nothing: the choice under it
    /// is lit, and the next press takes it ([`AutopilotMenu::press_lands`]).
    /// Whether the menu took the press — always, while one is open there.
    pub(crate) fn autopilot_menu_press(
        &mut self,
        wid: WindowId,
        left: bool,
        x: f64,
        y: f64,
    ) -> bool {
        let Some(menu) = self.autopilot_menu.as_ref().filter(|m| m.wid == wid) else {
            return false;
        };
        let cell = self.frame_cell_at(wid, x, y);
        let on_card = menu
            .rect
            .zip(cell)
            .filter(|(rect, (row, col))| rect.contains(*row, *col));
        match on_card {
            Some((rect, (row, col))) => {
                if left && let Some(i) = item_at(rect, &menu.rows, row, col) {
                    if menu.press_lands(row, i) {
                        self.choose_autopilot_menu_row(i);
                    } else if let Some(menu) = self.autopilot_menu.as_mut() {
                        menu.highlight = Some(i);
                        menu.rest_on((x, y), Some(row), Some(i));
                        self.request_menu_frame(wid);
                    }
                }
            }
            None => {
                self.close_autopilot_menu();
            }
        }
        true
    }

    /// The pointer moved over window `wid` while the menu is open there: the
    /// highlight follows a choice under it (and stays where the pointer is off
    /// every choice). Where it is on the painted card or off it is noted
    /// ([`AutopilotMenu::note_pointer`]): a card that then grows under a
    /// pointer resting beside it takes no press it did not see. Whether the
    /// menu took the motion — over the card.
    pub(crate) fn autopilot_menu_motion(&mut self, wid: WindowId, x: f64, y: f64) -> bool {
        let Some(rect) = self
            .autopilot_menu
            .as_ref()
            .filter(|m| m.wid == wid)
            .and_then(|m| m.rect)
        else {
            return false;
        };
        let cell = self.frame_cell_at(wid, x, y);
        let Some(menu) = self.autopilot_menu.as_mut() else {
            return false;
        };
        let Some((row, col)) = cell.filter(|(r, c)| rect.contains(*r, *c)) else {
            menu.note_pointer((x, y), cell.map(|(r, _)| r), None);
            return false;
        };
        let hovered = item_at(rect, &menu.rows, row, col);
        menu.note_pointer((x, y), Some(row), hovered);
        if hovered.is_some() && hovered != menu.highlight {
            menu.highlight = hovered;
            self.request_menu_frame(wid);
        }
        true
    }

    /// `controls autopilot-menu`: the open menu of the front window, or
    /// `autopilot-menu open=false`.
    #[must_use]
    pub(crate) fn autopilot_menu_controls(&self) -> Vec<String> {
        match self
            .autopilot_menu
            .as_ref()
            .filter(|m| Some(m.wid) == self.frontmost_window)
        {
            Some(menu) => {
                let sid = self
                    .pool
                    .get(menu.session)
                    .map(|s| s.ctx.self_id.as_str().to_string())
                    .unwrap_or_default();
                menu.controls_lines(&sid)
            }
            None => vec!["autopilot-menu open=false".to_string()],
        }
    }

    /// THE ACCESSIBILITY NODE of the menu open in window `wid` (§9.2.1,
    /// §9.3.3), once a frame has painted it: the grid tree's
    /// [`crate::a11y_tree::GridMenu`] over its cells, named in words
    /// (`Autopilot, tab 3, fix the footer`, and `harness off in Settings`
    /// where that line stands), one item per choice the card shows — a mode
    /// row a radio with its `●` as its checked state, a timer or Settings row
    /// a plain item, a disabled row disabled — and the highlighted row the
    /// tree's focus, since the keys are the menu's while it is open. Rows in
    /// the grid's own numbering (below the tab strip), as the chips' nodes
    /// are. `None` with no menu painted there: the tree publishes none.
    #[cfg(a11y_tree)]
    #[must_use]
    pub(crate) fn autopilot_grid_menu(&self, wid: WindowId) -> Option<crate::a11y_tree::GridMenu> {
        use crate::a11y_tree::{GridMenu, GridMenuItem};
        let menu = self.autopilot_menu.as_ref().filter(|m| m.wid == wid)?;
        let rect = menu.rect?;
        let top = rect.row.checked_sub(usize::from(self.chrome_rows(wid)))?;
        let mut items = Vec::new();
        let mut selected = None;
        let mut note = None;
        for (i, row) in menu.rows.iter().enumerate().take(shown_rows(rect)) {
            match row {
                MenuRow::Note(text) => note = Some(text.as_str()),
                MenuRow::Item {
                    spoken,
                    act,
                    enabled,
                    checked,
                    ..
                } => {
                    if menu.highlight == Some(i) {
                        selected = Some(items.len());
                    }
                    items.push(GridMenuItem {
                        label: spoken.clone(),
                        checked: matches!(act, MenuAct::Mode(_)).then_some(*checked),
                        disabled: !enabled,
                        row: top + 1 + i,
                    });
                }
                MenuRow::Header(_) | MenuRow::Rule | MenuRow::Gap(_) => {}
            }
        }
        let label = match note {
            Some(note) => format!("{}, {note}", menu.spoken),
            None => menu.spoken.clone(),
        };
        Some(GridMenu {
            label,
            epoch: menu.a11y_epoch(),
            items,
            selected,
            row: top,
            col: rect.col,
            width: rect.w,
            height: rect.h,
        })
    }

    /// A SCREEN READER CLICKED item `item` of the menu the tree published for
    /// window `wid` at `epoch` ([`Self::autopilot_grid_menu`]'s numbering: the
    /// card's shown choices, top down): what a click on that row does
    /// ([`Self::choose_autopilot_menu_row`]) — a disabled row does nothing.
    /// Only while the open menu's rows are the ones that tree was built from
    /// (its epoch, [`AutopilotMenu::a11y_epoch`]) and a frame has painted
    /// them. Rebuilt rows and the exact cancel instant feed the compact
    /// identity ([`AutopilotMenu::a11y_epoch`]); a request with a different
    /// epoch is refused rather than matched by position against the current
    /// rows. The 22-bit hash can collide; it is not a unique generation.
    /// A request for a menu that is shut, or open in another window, is
    /// nothing.
    #[cfg(a11y_tree)]
    pub(crate) fn choose_autopilot_menu_item_for_reader(
        &mut self,
        wid: WindowId,
        epoch: u32,
        item: usize,
    ) {
        let Some(row) = self
            .autopilot_menu
            .as_ref()
            .filter(|m| m.wid == wid && m.a11y_epoch() == epoch)
            .and_then(|m| {
                let rect = m.rect?;
                m.rows
                    .iter()
                    .enumerate()
                    .take(shown_rows(rect))
                    .filter(|(_, row)| matches!(row, MenuRow::Item { .. }))
                    .nth(item)
                    .map(|(i, _)| i)
            })
        else {
            return;
        };
        self.choose_autopilot_menu_row(row);
    }
}

/// How many of the menu's rows a card painted at `rect` shows: its rows
/// between the borders (a card too tall for the frame shows a prefix).
#[cfg(a11y_tree)]
fn shown_rows(rect: MenuRect) -> usize {
    rect.h.saturating_sub(2)
}

#[cfg(test)]
#[path = "autopilot_menu_tests.rs"]
mod tests;
