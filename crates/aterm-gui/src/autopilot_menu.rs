// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! In-grid popup menu for the Autopilot chip and composer-rule facts (`crate::fact_menu`).

use aterm_agent::supervise::Autopilot;
use aterm_core::terminal::RenderCell;
use aterm_render::Theme;

use crate::autopilot::AutopilotView;
use crate::autopilot_timers::TimerChoice;
use crate::claude_lights::{Fact, LandAtGlass};
use crate::fact_menu::FactAct;
use crate::tab_menu::{MenuRect, display_cells, ellipsize};
use crate::{App, WindowId};

/// Timed Cruise durations in minutes.
pub(crate) const LAND_IN_MINUTES: [u32; 3] = [60, 120, 240];

const H_PAD: usize = 1;
const MIN_CONTENT: usize = 24;
/// Default maximum content width in cells.
pub(crate) const MAX_CONTENT: usize = 44;
/// Maximum content width for cards containing a [`MenuRow::Plot`].
pub(crate) const TRAFFIC_MAX_CONTENT: usize = 60;
const GLYPH_CELLS: usize = 3;
const CHECK_CELLS: usize = 2;

/// Action performed when a menu row is chosen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MenuAct {
    Mode(Autopilot),
    Timer(TimerChoice),
    Settings,
    Fact(Fact, FactAct),
}

/// Owner of the open card (Autopilot chip or composer-rule fact).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MenuOwner {
    Autopilot,
    Fact(Fact),
}

impl MenuAct {
    /// Stable `controls` key for this action.
    #[must_use]
    pub(crate) fn key(self) -> String {
        match self {
            Self::Mode(mode) => format!("autopilot-menu/mode/{}", mode.word()),
            Self::Timer(TimerChoice::In(minutes)) => format!("autopilot-menu/land-in/{minutes}"),
            Self::Timer(TimerChoice::At) => "autopilot-menu/land-at".to_string(),
            Self::Timer(TimerChoice::Cancel) => "autopilot-menu/land-cancel".to_string(),
            Self::Settings => "autopilot-menu/settings".to_string(),
            Self::Fact(fact, act) => format!("fact-menu/{}/{}", fact.key(), act.key()),
        }
    }
}

/// One row of the menu.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum MenuRow {
    Header(String),
    Name(String),
    /// A line of words the card states (`◆  Opus 5.5`), and those words as
    /// a screen reader is told them, part of the card's name.
    Note {
        text: String,
        spoken: String,
    },
    /// Provenance note (`set 14:05 …`), with `floor` width preserved across rebuilds.
    SetBy {
        text: String,
        floor: usize,
    },
    Rule,
    /// Blank placeholder preserving row positions after a choice disappears.
    Gap(usize),
    Plot(Box<[PlotCell]>),
    Item {
        glyph: &'static str,
        label: String,
        spoken: String,
        act: MenuAct,
        enabled: bool,
        checked: bool,
    },
}

impl MenuRow {
    /// A note of `text`, spoken in its words ([`spoken_words`]).
    #[must_use]
    pub(crate) fn note(text: impl Into<String>) -> Self {
        let text = text.into();
        let spoken = spoken_words(&text);
        Self::Note { text, spoken }
    }

    /// A note of `text` a screen reader is told as `spoken`.
    #[must_use]
    pub(crate) fn note_spoken(text: impl Into<String>, spoken: impl Into<String>) -> Self {
        Self::Note {
            text: text.into(),
            spoken: spoken.into(),
        }
    }

    /// Whether a click or Return can choose this row.
    #[must_use]
    pub(crate) const fn selectable(&self) -> bool {
        matches!(self, Self::Item { enabled: true, .. })
    }

    fn text(&self) -> String {
        match self {
            Self::Header(text) | Self::Note { text, .. } | Self::SetBy { text, .. } => text.clone(),
            Self::Name(name) => quoted(name),
            Self::Rule | Self::Gap(_) => String::new(),
            Self::Plot(cells) => cells.iter().map(|cell| cell.ch).collect(),
            Self::Item { glyph, label, .. } => {
                let pad = GLYPH_CELLS.saturating_sub(display_cells(glyph));
                format!("{glyph}{}{label}", " ".repeat(pad))
            }
        }
    }
}

/// `text` as a screen reader is told it: its words, the glyphs that mark or
/// draw (`◆`, `⎇`, `▱▱▱`, an arrow) left out, and a `·` or `—` between two
/// parts a comma: `⎆  sol@example.test · max 20x` is `sol@example.test, max 20x`.
#[must_use]
pub(crate) fn spoken_words(text: &str) -> String {
    let glyph = |c: char| !c.is_ascii() && !c.is_alphanumeric();
    let mut out = String::with_capacity(text.len());
    let mut part = false;
    for token in text.split_whitespace() {
        if matches!(token, "\u{b7}" | "\u{2014}" | "\u{2013}") {
            part = !out.is_empty();
            continue;
        }
        let word = token.trim_matches(glyph);
        if word.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push_str(if part { ", " } else { " " });
        }
        part = false;
        out.push_str(word);
    }
    out
}

/// One cell of a [`MenuRow::Plot`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct PlotCell {
    pub(crate) ch: char,
    pub(crate) ink: PlotInk,
    pub(crate) busy: bool,
}

/// Ink role for a [`PlotCell`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum PlotInk {
    In,
    Out,
    Axis,
}

fn quoted(name: &str) -> String {
    format!("\u{201c}{name}\u{201d}")
}

fn quoted_in(name: &str, room: usize) -> String {
    if display_cells(&quoted(name)) <= room {
        return quoted(name);
    }
    quoted(&ellipsize(name, room.saturating_sub(2)))
}

/// Snapshot of session state used to build the menu rows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MenuFacts {
    pub(crate) title: String,
    pub(crate) name: Option<String>,
    pub(crate) room: usize,
    pub(crate) spoken: String,
    pub(crate) view: AutopilotView,
    pub(crate) carries: bool,
    pub(crate) land_at: Option<LandAtGlass>,
    pub(crate) armed_at_ms: Option<i64>,
    pub(crate) armed: Option<LandAtGlass>,
    pub(crate) set_by: Option<String>,
}

/// Format a landing time (`18:00` or `Sat 16:00`).
pub(crate) fn when(at: LandAtGlass) -> String {
    match at.other_day {
        Some(day) => format!("{} {}", day.short(), at.clock.as_str()),
        None => at.clock.as_str().to_string(),
    }
}

/// Format [`when`] for screen readers (`18:00` or `Saturday 16:00`).
pub(crate) fn spoken_when(at: LandAtGlass) -> String {
    match at.other_day {
        Some(day) => format!("{} {}", day.long(), at.clock.as_str()),
        None => at.clock.as_str().to_string(),
    }
}

/// Build the Autopilot menu rows from `facts`.
#[must_use]
pub(crate) fn rows(facts: &MenuFacts) -> Vec<MenuRow> {
    let on = facts.carries;
    let mut out = header_rows(&facts.title, facts.name.as_deref(), facts.room);
    if let Some(set_by) = &facts.set_by {
        out.extend(set_by_rows(set_by, facts.room));
    }
    if !on {
        let why = match facts.view {
            AutopilotView::AgentOff(agent) => crate::autopilot::agent_switch_off(agent),
            _ => "harness off in Settings",
        };
        out.push(MenuRow::note(why));
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

/// Build header rows for `title` and optional `name` within `room` cells.
#[must_use]
pub(crate) fn header_rows(title: &str, name: Option<&str>, room: usize) -> Vec<MenuRow> {
    match name {
        Some(name) => {
            let whole = format!("{title} {}", quoted(name));
            if display_cells(&whole) <= room {
                vec![MenuRow::Header(whole)]
            } else {
                let mut out = place_rows(title, room);
                out.push(MenuRow::Name(name.to_string()));
                out
            }
        }
        None => place_rows(title, room),
    }
}

fn place_rows(title: &str, room: usize) -> Vec<MenuRow> {
    match title.rsplit_once(", ") {
        Some((head, pane)) if display_cells(title) > room => vec![
            MenuRow::Header(format!("{head},")),
            MenuRow::Header(pane.to_string()),
        ],
        _ => vec![MenuRow::Header(title.to_string())],
    }
}

fn set_by_rows(note: &str, room: usize) -> Vec<MenuRow> {
    let row = |text: String| MenuRow::SetBy { text, floor: 0 };
    if display_cells(note) <= room {
        return vec![row(note.to_string())];
    }
    match note.rsplit_once(", ") {
        Some((head, tail)) if display_cells(head) < room && display_cells(tail) <= room => {
            vec![row(format!("{head},")), row(tail.to_string())]
        }
        _ => Vec::new(),
    }
}

/// Drop [`MenuRow::SetBy`] rows when keeping them would truncate the card or flip its side.
#[must_use]
pub(crate) fn fit_rows(
    rows: Vec<MenuRow>,
    anchor: (usize, usize),
    pane: (std::ops::Range<usize>, std::ops::Range<usize>),
    frame: (usize, usize),
    top: usize,
) -> Vec<MenuRow> {
    if !rows.iter().any(|row| matches!(row, MenuRow::SetBy { .. })) {
        return rows;
    }
    let without: Vec<MenuRow> = rows
        .iter()
        .filter(|row| !matches!(row, MenuRow::SetBy { .. }))
        .cloned()
        .collect();
    let side = |rect: MenuRect| {
        if rect.row + rect.h <= anchor.0 {
            0u8
        } else if rect.row > anchor.0 {
            2
        } else {
            1
        }
    };
    let with = place(&rows, anchor, pane.clone(), frame, top);
    let bare = place(&without, anchor, pane, frame, top);
    match (with, bare) {
        (Some(with), Some(bare)) if with.h == rows.len() + 2 && side(with) == side(bare) => rows,
        _ => without,
    }
}

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

fn row_width(row: &MenuRow) -> usize {
    match row {
        MenuRow::Gap(cells) => *cells,
        MenuRow::Plot(cells) => cells.len(),
        MenuRow::Item { .. } => display_cells(&row.text()) + CHECK_CELLS,
        MenuRow::SetBy { floor, .. } => display_cells(&row.text()).max(*floor),
        _ => display_cells(&row.text()),
    }
}

fn content_width(rows: &[MenuRow]) -> usize {
    rows.iter()
        .map(row_width)
        .max()
        .unwrap_or(0)
        .clamp(MIN_CONTENT, max_content(rows))
}

fn max_content(rows: &[MenuRow]) -> usize {
    if rows.iter().any(|row| matches!(row, MenuRow::Plot(_))) {
        TRAFFIC_MAX_CONTENT
    } else {
        MAX_CONTENT
    }
}

const CHROME_W: usize = 2 + 2 * H_PAD;

const BORDER_W: usize = 2;

/// Available content cells in a pane `pane_cols` wide.
#[must_use]
pub(crate) fn content_room(pane_cols: usize) -> usize {
    content_room_up_to(pane_cols, MAX_CONTENT)
}

/// Available content cells in `pane_cols`, capped at `widest`.
#[must_use]
pub(crate) fn content_room_up_to(pane_cols: usize, widest: usize) -> usize {
    pane_cols.saturating_sub(BORDER_W).min(widest)
}

fn side_pad(w: usize, rows: &[MenuRow]) -> usize {
    if w >= content_width(rows) + CHROME_W {
        H_PAD
    } else {
        0
    }
}

fn least_rows(rows: &[MenuRow]) -> usize {
    rows.iter()
        .take_while(|row| !matches!(row, MenuRow::Rule))
        .filter(|row| !matches!(row, MenuRow::SetBy { .. }))
        .count()
        + 2
}

fn same_place(shown: &MenuRow, fresh: &MenuRow) -> bool {
    match (shown, fresh) {
        (MenuRow::Header(_), MenuRow::Header(_))
        | (MenuRow::Name(_), MenuRow::Name(_))
        | (MenuRow::Note { .. }, MenuRow::Note { .. })
        | (MenuRow::Plot(_), MenuRow::Plot(_))
        | (MenuRow::SetBy { .. }, MenuRow::SetBy { .. })
        | (MenuRow::Rule, MenuRow::Rule) => true,
        (MenuRow::Item { act: a, .. }, MenuRow::Item { act: b, .. }) => a == b,
        _ => false,
    }
}

/// Map `fresh` onto `shown` row positions using [`MenuRow::Gap`] for removed rows.
#[must_use]
pub(crate) fn keep_places(shown: &[MenuRow], fresh: &[MenuRow]) -> Option<Vec<MenuRow>> {
    let mut next = fresh.iter().peekable();
    let laid = shown
        .iter()
        .map(
            |old| match next.next_if(|new| same_place(old, new)).cloned() {
                Some(MenuRow::SetBy { text, floor }) => MenuRow::SetBy {
                    text,
                    floor: floor.max(row_width(old)),
                },
                Some(new) => new,
                None => MenuRow::Gap(row_width(old)),
            },
        )
        .collect();
    next.peek().is_none().then_some(laid)
}

/// Place the menu card relative to `(chip_row, chip_col)` within `pane` and `frame`.
#[must_use]
pub(crate) fn place(
    rows: &[MenuRow],
    (chip_row, chip_col): (usize, usize),
    (pane_rows, pane_cols): (std::ops::Range<usize>, std::ops::Range<usize>),
    (cols, frame_rows): (usize, usize),
    top: usize,
) -> Option<MenuRect> {
    if rows.is_empty() || cols <= CHROME_W || chip_row >= frame_rows {
        return None;
    }
    let pane = pane_cols.start.min(cols)..pane_cols.end.min(cols);
    if pane.len() <= BORDER_W {
        return None;
    }
    let w = (content_width(rows) + CHROME_W).min(pane.len());
    let col = chip_col.min(pane.end - w).max(pane.start);
    let (lo, hi) = (pane_rows.start.max(top), pane_rows.end.min(frame_rows));
    let (lo, hi) = if (lo..hi).contains(&chip_row) && hi - lo >= least_rows(rows) {
        (lo, hi)
    } else {
        (top.min(chip_row), frame_rows)
    };
    let (above, below) = (chip_row - lo, hi - chip_row - 1);
    let want = rows.len() + 2;
    let (row, mut h) = if above >= want {
        (chip_row - want, want)
    } else if below >= want {
        (chip_row + 1, want)
    } else if hi - lo >= want {
        if above >= below {
            (lo, want)
        } else {
            (hi - want, want)
        }
    } else {
        (lo, hi - lo)
    };
    if h < want && h > 3 && matches!(rows.get(h - 3), Some(MenuRow::Rule)) {
        h -= 1;
    }
    (h >= 3).then_some(MenuRect { row, col, w, h })
}

/// Index into `rows` at `frame_row`, or `None` on a border or past truncation.
#[must_use]
pub(crate) const fn row_at(rect: MenuRect, frame_row: usize) -> Option<usize> {
    if frame_row <= rect.row || frame_row + 1 >= rect.row + rect.h {
        return None;
    }
    Some(frame_row - rect.row - 1)
}

/// Selectable row index at frame `(row, col)`.
#[must_use]
pub(crate) fn item_at(rect: MenuRect, rows: &[MenuRow], row: usize, col: usize) -> Option<usize> {
    if !rect.contains(row, col) {
        return None;
    }
    let i = row_at(rect, row)?;
    rows.get(i).filter(|r| r.selectable()).map(|_| i)
}

/// Advance highlight by one selectable row within the first `shown` rows.
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

/// Key action for an open menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MenuKey {
    Up,
    Down,
    Choose,
    Close,
    PassThrough,
    Inert,
}

/// Classify `key` for an open menu.
#[must_use]
pub(crate) const fn key_of(key: &aterm_types::keyboard::Key) -> MenuKey {
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

/// Render the menu card into `rect.h` rows of `rect.w` cells.
#[must_use]
pub(crate) fn paint(
    rect: MenuRect,
    rows: &[MenuRow],
    highlight: Option<usize>,
    theme: Theme,
) -> Vec<Vec<RenderCell>> {
    let c = crate::tab_menu::menu_colors(theme);
    let w = rect.w;
    let pad = side_pad(w, rows);
    let text_start = 1 + pad;
    let text_end = w.saturating_sub(1 + pad);
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
        if let MenuRow::Plot(plot) = row {
            out.push(paint_plot(w, (text_start, text_end), plot, &c));
            continue;
        }
        let selected = highlight == Some(i) && row.selectable();
        let bg = if selected { c.sel_bg } else { c.card_bg };
        let (fg, bold) = match row {
            MenuRow::Header(_) | MenuRow::Name(_) => (c.band.value, true),
            MenuRow::Note { .. } | MenuRow::SetBy { .. } | MenuRow::Item { enabled: false, .. } => {
                (c.disabled, false)
            }
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
        let text = match row {
            MenuRow::Name(name) => quoted_in(name, room),
            row => ellipsize(&row.text(), room),
        };
        crate::tab_menu::put_text(&mut cells, text_start, text_end, &text, fg, bg, bold);
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

fn paint_plot(
    w: usize,
    (text_start, text_end): (usize, usize),
    plot: &[PlotCell],
    c: &crate::tab_menu::MenuColors,
) -> Vec<RenderCell> {
    let cell = crate::chrome_band::cell;
    let busy_bg = crate::chrome_band::mix3(c.card_bg, c.band.accent, 0.10);
    let mut cells = vec![cell(' ', c.band.value, c.card_bg, false, false); w];
    cells[0] = cell('\u{2502}', c.rule, c.card_bg, false, false);
    if w >= 2 {
        cells[w - 1] = cell('\u{2502}', c.rule, c.card_bg, false, false);
    }
    for (slot, p) in cells
        .iter_mut()
        .take(text_end)
        .skip(text_start)
        .zip(plot.iter())
    {
        let fg = match p.ink {
            PlotInk::In => c.band.accent,
            PlotInk::Out => c.band.value,
            PlotInk::Axis => c.band.label,
        };
        let bg = if p.busy { busy_bg } else { c.card_bg };
        *slot = cell(p.ch, fg, bg, false, false);
    }
    cells
}

/// Recently closed menu card rect used to swallow the second press of a double-click.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SpentCard {
    pub(crate) wid: WindowId,
    pub(crate) rect: MenuRect,
    pub(crate) laid: Option<crate::app_mouse::GlassGrid>,
    pub(crate) at: std::time::Instant,
}

/// Open Autopilot or fact popup menu state.
#[derive(Clone, Debug)]
pub(crate) struct AutopilotMenu {
    pub(crate) owner: MenuOwner,
    pub(crate) wid: WindowId,
    pub(crate) session: u64,
    pub(crate) focus: Option<u64>,
    pub(crate) anchor: (usize, usize),
    pub(crate) rows: Vec<MenuRow>,
    pub(crate) spoken: String,
    /// Armed timer deadline (unix ms) targeted by the cancel row.
    pub(crate) cancels: Option<i64>,
    pub(crate) highlight: Option<usize>,
    pub(crate) rect: Option<MenuRect>,
    pub(crate) laid: Option<crate::app_mouse::GlassGrid>,
    pub(crate) generation: u64,
    pub(crate) rest: PointerRest,
}

/// Last pointer position and hovered choice recorded while the menu is open.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PointerRest {
    px: Option<(f64, f64)>,
    frame_row: Option<usize>,
    choice: Option<Choice>,
    generation: u64,
}

impl PointerRest {
    /// Initial pointer state when the menu opens.
    pub(crate) const fn at_open(px: Option<(f64, f64)>) -> Self {
        Self {
            px,
            frame_row: None,
            choice: None,
            generation: 0,
        }
    }
}

type Choice = (MenuAct, String, bool);

impl AutopilotMenu {
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

    fn note_pointer(&mut self, px: (f64, f64), frame_row: Option<usize>, hovered: Option<usize>) {
        if self.rest.px != Some(px) {
            self.rest_on(px, frame_row, hovered);
        }
    }

    fn rest_on(&mut self, px: (f64, f64), frame_row: Option<usize>, hovered: Option<usize>) {
        self.rest = PointerRest {
            px: Some(px),
            frame_row,
            choice: hovered.and_then(|i| self.choice_at(i)),
            generation: self.generation,
        };
    }

    /// Whether a press at `(frame_row, i)` lands without rows having shifted under the pointer.
    fn press_lands(&self, frame_row: usize, i: usize) -> bool {
        let rest = &self.rest;
        rest.generation == self.generation
            || (rest.frame_row == Some(frame_row)
                && rest.choice.is_some()
                && rest.choice == self.choice_at(i))
    }

    /// Rebuild menu rows in place, returning whether visible rows changed.
    pub(crate) fn refresh_rows(
        &mut self,
        fresh: Vec<MenuRow>,
        spoken: String,
        cancels: Option<i64>,
    ) -> bool {
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

    /// Format `controls autopilot-menu` or `controls fact-menu` lines.
    #[must_use]
    pub(crate) fn controls_lines(&self, sid: &str) -> Vec<String> {
        let (target, fact) = match self.owner {
            MenuOwner::Autopilot => ("autopilot-menu", String::new()),
            MenuOwner::Fact(fact) => ("fact-menu", format!(" fact={}", fact.key())),
        };
        let mut out = vec![format!(
            "{target} open=true session={sid}{fact} rows={} selected={}",
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
                    "{target} item index={i} key={} enabled={enabled} checked={checked} \
                     selected={} text={:?}",
                    act.key(),
                    self.highlight == Some(i),
                    row.text()
                )),
                MenuRow::Rule => out.push(format!("{target} rule index={i}")),
                MenuRow::Gap(_) => out.push(format!("{target} gap index={i}")),
                MenuRow::Header(text)
                | MenuRow::Note { text, .. }
                | MenuRow::SetBy { text, .. } => {
                    out.push(format!("{target} label index={i} text={text:?}"));
                }
                MenuRow::Name(_) => {
                    out.push(format!("{target} label index={i} text={:?}", row.text()));
                }
                MenuRow::Plot(_) => {
                    out.push(format!("{target} plot index={i} text={:?}", row.text()));
                }
            }
        }
        out
    }

    /// Compact 22-bit accessibility epoch over session, spoken title, cancel deadline, and rows.
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
            Self::Name(t) => (5u8, t).hash(h),
            Self::Note { text, .. } => (1u8, text).hash(h),
            Self::Rule => 2u8.hash(h),
            Self::Gap(cells) => (4u8, cells).hash(h),
            Self::SetBy { text, floor } => (6u8, text, floor).hash(h),
            Self::Plot(cells) => (7u8, cells).hash(h),
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

/// Repaint fingerprint for `wid` (`0` when no menu is open in `wid`).
#[must_use]
pub(crate) fn fingerprint(menu: Option<&AutopilotMenu>, wid: WindowId) -> u64 {
    menu.filter(|menu| menu.wid == wid)
        .map_or(0, AutopilotMenu::fingerprint)
}

impl App {
    /// Session place label and optional title for menu headers.
    pub(crate) fn menu_place_and_name(&self, session: u64) -> (String, Option<String>) {
        let sid = self
            .pool
            .get(session)
            .map(|s| s.ctx.self_id.as_str().to_string())
            .unwrap_or_default();
        let place = self.session_place(&sid);
        let place = place.strip_prefix("in ").unwrap_or(&place).to_string();
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
        (place, name)
    }

    fn autopilot_menu_facts(&self, session: u64) -> MenuFacts {
        let (_, view, _) = self.session_autopilot(session);
        let (place, name) = self.menu_place_and_name(session);
        let spoken = match &name {
            Some(name) => format!("Autopilot, {place}, {name}"),
            None => format!("Autopilot, {place}"),
        };
        let set_by = self
            .pool
            .get(session)
            .is_some_and(|s| {
                s.ctx
                    .meta
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .autopilot
                    .is_some()
            })
            .then(|| self.autopilot_set_by_words(session))
            .flatten()
            .map(|words| {
                let today = crate::harness_glass::local_day_at(
                    crate::autopilot_timers::wall_ms().div_euclid(1000),
                );
                words.line(Some(today))
            });
        MenuFacts {
            title: format!("Autopilot \u{b7} {place}"),
            name,
            room: MAX_CONTENT,
            spoken,
            view,
            carries: view.mode().is_some(),
            land_at: self.harness_policy.get().policy.land_at.map(|at| {
                crate::autopilot_timers::offer_glass(
                    at,
                    crate::autopilot_timers::wall_ms(),
                    &crate::autopilot_timers::local_offset_at,
                )
            }),
            armed_at_ms: self.autopilot_timers.land_at_ms(session),
            armed: self.autopilot_timers.glass(session),
            set_by,
        }
    }

    fn autopilot_menu_fit(
        &self,
        wid: WindowId,
        session: u64,
        anchor: (usize, usize),
        rows: Vec<MenuRow>,
    ) -> Vec<MenuRow> {
        let frame = self.windows.get(&wid).map_or((0, 0), |ws| {
            (ws.input_scratch.cols, ws.input_scratch.cells.len())
        });
        let top = usize::from(self.chrome_rows(wid));
        fit_rows(
            rows,
            anchor,
            self.autopilot_menu_pane(wid, session),
            frame,
            top,
        )
    }

    /// Open `session`'s Autopilot menu in `wid` anchored at `anchor`.
    pub(crate) fn open_autopilot_menu(
        &mut self,
        wid: WindowId,
        session: u64,
        anchor: (usize, usize),
        keyboard: bool,
    ) {
        self.close_autopilot_menu();
        self.autopilot_menu_spent = None;
        self.note_land_at_offer();
        let facts = MenuFacts {
            room: content_room(self.autopilot_menu_pane(wid, session).1.len()),
            ..self.autopilot_menu_facts(session)
        };
        let rows = self.autopilot_menu_fit(wid, session, anchor, rows(&facts));
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
            owner: MenuOwner::Autopilot,
            wid,
            session,
            focus,
            anchor,
            rows,
            spoken: facts.spoken,
            cancels: self.autopilot_timers.land_at_ms(session),
            highlight,
            rect: None,
            laid: None,
            generation: 0,
            rest: PointerRest::at_open(px),
        });
        self.request_menu_frame(wid);
    }

    /// Rebuild the open Autopilot menu if it belongs to `local_id`.
    pub(crate) fn refresh_autopilot_menu(&mut self, local_id: u64) {
        if !self
            .autopilot_menu
            .as_ref()
            .is_some_and(|menu| menu.session == local_id && menu.owner == MenuOwner::Autopilot)
        {
            return;
        }
        self.note_land_at_offer();
        let Some((wid, anchor)) = self
            .autopilot_menu
            .as_ref()
            .map(|menu| (menu.wid, menu.anchor))
        else {
            return;
        };
        let facts = MenuFacts {
            room: content_room(self.autopilot_menu_pane(wid, local_id).1.len()),
            ..self.autopilot_menu_facts(local_id)
        };
        let fresh = self.autopilot_menu_fit(wid, local_id, anchor, rows(&facts));
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

    /// Close any open menu and request a redraw.
    pub(crate) fn close_autopilot_menu(&mut self) -> bool {
        let Some(menu) = self.autopilot_menu.take() else {
            return false;
        };
        self.request_menu_frame(menu.wid);
        true
    }

    /// Frame row and column ranges of `session`'s visible pane in `wid`.
    pub(crate) fn autopilot_menu_pane(
        &self,
        wid: WindowId,
        session: u64,
    ) -> (std::ops::Range<usize>, std::ops::Range<usize>) {
        let (rows, cols) = self.windows.get(&wid).map_or((0, 0), |ws| {
            (ws.input_scratch.cells.len(), ws.input_scratch.cols)
        });
        let chrome = usize::from(self.chrome_rows(wid));
        self.visible_session_rect(wid, session).map_or(
            (0..rows, 0..cols),
            |(row, col, height, width)| {
                let (row, col) = (
                    (chrome + usize::from(row)).min(rows),
                    usize::from(col).min(cols),
                );
                (
                    row..(row + usize::from(height)).min(rows),
                    col..(col + usize::from(width)).min(cols),
                )
            },
        )
    }

    /// Request a redraw on `wid`.
    pub(crate) fn request_menu_frame(&self, wid: WindowId) {
        if let Some(window) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref()) {
            window.request_redraw();
        }
    }

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

    fn shows_autopilot_chip(&self, wid: WindowId, session: u64, cell: (usize, usize)) -> bool {
        let (x, y) = self.frame_cell_centre(wid, cell);
        self.claude_chips_at(wid, x, y)
            == Some((session, Some(crate::claude_lights::Chip::Autopilot)))
    }

    /// Open the Autopilot menu if `(x, y)` hits an Autopilot chip in `wid`.
    pub(crate) fn open_autopilot_menu_at(&mut self, wid: WindowId, x: f64, y: f64) -> bool {
        let Some((session, Some(crate::claude_lights::Chip::Autopilot))) =
            self.claude_chips_at(wid, x, y)
        else {
            return false;
        };
        let Some((row, mut col)) = self.glass_cell_at(wid, x, y) else {
            return false;
        };
        while col > 0 && self.shows_autopilot_chip(wid, session, (row, col - 1)) {
            col -= 1;
        }
        self.open_autopilot_menu(wid, session, (row, col), false);
        true
    }

    /// Open the Autopilot menu for the front window's focused session (`open autopilot-menu`).
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

    /// Paint the open menu into `wid`'s frame scratch, or close it if its anchor or focus moved.
    pub(crate) fn splice_autopilot_menu(&mut self, wid: WindowId) {
        let rect = self.paint_autopilot_menu(wid);
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        if let Some(rect) = rect {
            ws.pet_chrome
                .note_overlay(rect.row, rect.col, rect.h, rect.w);
        }
        if ws.pet_chrome.commit()
            && let Some(window) = ws.os_window.as_ref()
        {
            window.request_redraw();
        }
    }

    fn paint_autopilot_menu(&mut self, wid: WindowId) -> Option<crate::tab_menu::MenuRect> {
        let menu = self.autopilot_menu.as_ref().filter(|m| m.wid == wid)?;
        let (session, anchor, focus, owner) = (menu.session, menu.anchor, menu.focus, menu.owner);
        let anchored = match owner {
            MenuOwner::Autopilot => self.shows_autopilot_chip(wid, session, anchor),
            MenuOwner::Fact(fact) => self.shows_fact_on_row(wid, session, fact, anchor.0),
        };
        if self.focused_session_id(wid) != focus || !anchored {
            self.close_autopilot_menu();
            return None;
        }
        let top = usize::from(self.chrome_rows(wid));
        let theme = self.overlay_card_theme(wid);
        let ((cw, ch), pad) = (self.win_cell_size(wid), self.win_pad(wid));
        let grid_top = self.win_pad_top(wid) + self.win_head(wid);
        let pane = self.autopilot_menu_pane(wid, session);
        let menu = self.autopilot_menu.as_ref()?;
        let ws = self.windows.get(&wid)?;
        let rect = place(
            &menu.rows,
            anchor,
            pane,
            (ws.input_scratch.cols, ws.input_scratch.cells.len()),
            top,
        );
        let Some(rect) = rect else {
            self.close_autopilot_menu();
            return None;
        };
        let laid = self.glass_grid(wid);
        let arrived = (menu.rect != Some(rect) || menu.laid != laid)
            && menu
                .rest
                .px
                .and_then(|(x, y)| self.frame_cell_at(wid, x, y))
                .is_some_and(|(row, col)| rect.contains(row, col));
        let painted = paint(rect, &menu.rows, menu.highlight, theme);
        let ws = self.windows.get_mut(&wid)?;
        let ground = crate::tab_menu::ground_cell(theme, ws.input_scratch.default_bg);
        let right = rect.col + rect.w;
        for (r, row) in painted.iter().enumerate() {
            let frame_row = rect.row + r;
            let Some(cells) = ws.input_scratch.cells.get_mut(frame_row) else {
                break;
            };
            if cells.len() < right {
                cells.resize(right, ground);
            }
            for (c, cell) in row.iter().enumerate() {
                if let Some(slot) = cells.get_mut(rect.col + c) {
                    *slot = *cell;
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
        crate::app_render::scrub_overlay_card(
            &mut ws.input_scratch,
            rect,
            (cw, ch),
            (pad, grid_top),
        );
        // Paint card marks after scrubbing overlay glyphs so closing the card strips them.
        let on_frame = ws.input_scratch.cells.len().saturating_sub(rect.row);
        for (r, row) in painted.iter().enumerate().take(on_frame) {
            ws.claude_card
                .paint_marks(&mut ws.input_scratch, rect.row + r, rect.col, row);
        }
        ws.input_scratch.snapshot_seq = ws.input_scratch.snapshot_seq.wrapping_add(1);
        if let Some(menu) = self.autopilot_menu.as_mut() {
            menu.rect = Some(rect);
            menu.laid = laid;
            if arrived {
                menu.generation = menu.generation.wrapping_add(1);
            }
        }
        Some(rect)
    }

    /// Execute selectable row `i` and close the menu.
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
        if let MenuAct::Fact(fact, crate::fact_menu::FactAct::Type(slash)) = act {
            let (anchor, keyboard) = (menu.anchor, menu.highlight.is_some());
            self.choose_fact_type(wid, session, fact, slash, anchor, keyboard);
            return;
        }
        self.close_autopilot_menu();
        match act {
            MenuAct::Mode(mode) => {
                if self
                    .session_harness_posture(session)
                    .default_mode()
                    .is_some()
                {
                    self.set_session_autopilot(
                        session,
                        Some(mode),
                        crate::autopilot::Surface::Menu,
                    );
                }
            }
            MenuAct::Timer(choice) => {
                self.choose_autopilot_timer_naming(
                    session,
                    choice,
                    cancels,
                    crate::autopilot::Surface::Menu,
                );
            }
            MenuAct::Settings => {
                self.open_harness_settings(
                    wid,
                    crate::harness_screen::HarnessFocus::Session(session),
                );
            }
            MenuAct::Fact(_, act) => self.choose_fact_act(wid, session, act),
        }
    }

    /// Handle a window key event while the menu is open in `wid`.
    pub(crate) fn autopilot_menu_key(
        &mut self,
        wid: WindowId,
        ev: &aterm_window::event::KeyEvent,
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

    /// Apply `meaning` to the open menu in `wid`.
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

    /// Handle a controller input event while the menu is open in `wid`.
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

    /// Handle a mouse press in `wid` while the menu is open.
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
        let (laid, cell) = (menu.laid, self.laid_cell_at(wid, menu.laid, x, y));
        let on_card = menu
            .rect
            .zip(cell)
            .filter(|(rect, (row, col))| rect.contains(*row, *col));
        match on_card {
            Some((rect, (row, col))) => {
                if left && let Some(i) = item_at(rect, &menu.rows, row, col) {
                    if menu.press_lands(row, i) {
                        self.choose_autopilot_menu_row(i);
                        if self.autopilot_menu.as_ref().is_none_or(|m| m.wid != wid) {
                            self.autopilot_menu_spent = Some(SpentCard {
                                wid,
                                rect,
                                laid,
                                at: std::time::Instant::now(),
                            });
                        }
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

    /// Swallow the second press of a double-click on the rect a menu choice just closed.
    pub(crate) fn spends_menu_follow_up(&mut self, wid: WindowId) -> bool {
        let Some(spent) = self.autopilot_menu_spent.take() else {
            return false;
        };
        let (x, y) = self
            .windows
            .get(&wid)
            .map_or((0.0, 0.0), |ws| ws.last_cursor_px);
        spent.wid == wid
            && spent.at.elapsed().as_millis() <= crate::multi_click_ms()
            && spent
                .laid
                .and_then(|laid| laid.cell_at(x, y))
                .is_some_and(|(row, col)| spent.rect.contains(row, col))
    }

    /// Update hover highlight and pointer rest state on mouse motion in `wid`.
    pub(crate) fn autopilot_menu_motion(&mut self, wid: WindowId, x: f64, y: f64) -> bool {
        let Some((rect, laid)) = self
            .autopilot_menu
            .as_ref()
            .filter(|m| m.wid == wid)
            .map(|m| (m.rect, m.laid))
        else {
            return false;
        };
        let cell = self.laid_cell_at(wid, laid, x, y);
        let Some(menu) = self.autopilot_menu.as_mut() else {
            return false;
        };
        let Some((rect, (row, col))) = rect
            .zip(cell)
            .filter(|(rect, (r, c))| rect.contains(*r, *c))
        else {
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

    /// `controls autopilot-menu` output for the frontmost window.
    #[must_use]
    pub(crate) fn autopilot_menu_controls(&self) -> Vec<String> {
        match self
            .autopilot_menu
            .as_ref()
            .filter(|m| Some(m.wid) == self.frontmost_window && m.owner == MenuOwner::Autopilot)
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

    /// Build the accessibility [`crate::a11y_tree::GridMenu`] for `wid`.
    #[cfg(a11y_tree)]
    #[must_use]
    pub(crate) fn autopilot_grid_menu(&self, wid: WindowId) -> Option<crate::a11y_tree::GridMenu> {
        use crate::a11y_tree::{GridMenu, GridMenuItem};
        let menu = self.autopilot_menu.as_ref().filter(|m| m.wid == wid)?;
        let rect = menu.rect?;
        let top = rect.row.checked_sub(usize::from(self.chrome_rows(wid)))?;
        let mut items = Vec::new();
        let mut selected = None;
        let mut notes: Vec<&str> = Vec::new();
        let mut set_by: Vec<&str> = Vec::new();
        for (i, row) in menu.rows.iter().enumerate().take(shown_rows(rect)) {
            match row {
                MenuRow::Note { spoken, .. } => notes.push(spoken.as_str()),
                MenuRow::SetBy { text, .. } => set_by.push(text.as_str()),
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
                MenuRow::Header(_)
                | MenuRow::Name(_)
                | MenuRow::Rule
                | MenuRow::Gap(_)
                | MenuRow::Plot(_) => {}
            }
        }
        let mut label = menu.spoken.clone();
        if !set_by.is_empty() {
            label.push_str(", ");
            label.push_str(&set_by.join(" "));
        }
        for note in notes.into_iter().filter(|note| !note.is_empty()) {
            label.push_str(", ");
            label.push_str(note);
        }
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

    /// Activate `item` from a screen reader if `epoch` matches the painted menu.
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

#[cfg(a11y_tree)]
const fn shown_rows(rect: MenuRect) -> usize {
    rect.h.saturating_sub(2)
}

#[cfg(test)]
#[path = "autopilot_menu_tests.rs"]
mod tests;
