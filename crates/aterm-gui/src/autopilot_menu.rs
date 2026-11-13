// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! In-grid popup card of the composer-rule facts' menus (`crate::fact_menu`) and of the
//! permission chip's box (`crate::permission_box`): the row model, its placement, painter,
//! pointer, keys and reader tree. A row is an item a choice closes the card on, or a slider of
//! snap stops ([`MenuRow::Stops`]) whose pick leaves the card open.

use aterm_core::terminal::RenderCell;
use aterm_effects::glass_inks::{GlassInks, field};
use aterm_effects::harness_card::CardPalette;
use aterm_render::Theme;

use crate::claude_lights::Fact;
use crate::fact_menu::FactAct;
use crate::menu_viewport::MenuViewport;
use crate::tab_menu::{MenuRect, display_cells, ellipsize};
use crate::{App, WindowId};

const H_PAD: usize = 1;
const MIN_CONTENT: usize = 24;
/// Default maximum content width in cells.
pub(crate) const MAX_CONTENT: usize = 44;
/// Maximum content width for cards containing a [`MenuRow::Plot`].
pub(crate) const TRAFFIC_MAX_CONTENT: usize = 60;
const GLYPH_CELLS: usize = 3;
const CHECK_CELLS: usize = 2;
/// The cells between an item's label and its trailing words.
const TRAIL_GAP_CELLS: usize = 2;
/// The cells a [`MenuRow::Fact`]'s name column takes in the cell card, the value beginning at
/// the next: a card's fact names are at most 8 cells, so one blank at least parts the two, and
/// a builder wraps a value to the room less this.
pub(crate) const FACT_LABEL_CELLS: usize = 9;
/// The cells a slider's stop takes before its label: its mark and a space.
const STOP_MARK_CELLS: usize = 2;
/// The fewest cells of track between two stops of a slider with room for them: a space, three
/// of track, a space.
const STOP_TRACK_CELLS: usize = 5;
/// A slider's stop the thumb stands on, and one it does not.
const STOP_THUMB: char = '\u{25cf}';
const STOP_MARK: char = '\u{25cb}';

/// Action performed when a menu row is chosen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MenuAct {
    Fact(Fact, FactAct),
}

/// Owner of the open card: the composer-rule fact it hangs from, or the permission chip, whose
/// box has no row to choose (`crate::permission_box`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MenuOwner {
    Fact(Fact),
    Permission,
}

impl MenuAct {
    /// Stable `controls` key for this action.
    #[must_use]
    pub(crate) fn key(self) -> String {
        match self {
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
    /// a screen reader is told them, part of the card's name, in `tone`'s ink.
    Note {
        text: String,
        spoken: String,
        tone: NoteTone,
    },
    Rule,
    /// A group heading: the title of the rows under it, to the next heading. Not a choice; a
    /// screen reader is told the title as part of the card's name.
    Section(String),
    /// A named fact: its name (`Token`) and its value (`none in the vault`) in `tone`'s ink,
    /// and the pair as a screen reader is told it (`spoken`, part of the card's name; empty on
    /// a row that continues the fact above it, whose `label` is empty too). Not a choice. The
    /// builder wraps a long value over rows: a row never wraps itself.
    Fact {
        label: String,
        value: String,
        spoken: String,
        tone: NoteTone,
    },
    /// Blank placeholder preserving row positions after a choice disappears.
    Gap(usize),
    /// A drawn row (a chart, a bar, a strip) with the caption a screen reader is told for it.
    Plot {
        cells: Box<[PlotCell]>,
        spoken: String,
    },
    Item {
        glyph: &'static str,
        label: String,
        spoken: String,
        act: MenuAct,
        enabled: bool,
        checked: bool,
        /// Words set apart from the label at the row's far end (`≤88% left`): what the choice
        /// stands at, not its name. Empty for none.
        trail: String,
    },
    /// A slider of snap stops: one selectable row whose pick does not close the card. The row
    /// holds what a slider is, never how it is drawn: its stops in order, the stop its thumb
    /// stands on (`None`: the value is none of them) and the stop the keys or the pointer rest
    /// on. The rest is the card's own state, kept across a rebuild of its rows and left out of
    /// the row's reader identity (its `Hash`).
    Stops {
        /// What the slider sets, as a screen reader names it (`Effort`).
        name: String,
        stops: Box<[Stop]>,
        at: Option<usize>,
        focus: Option<usize>,
    },
}

/// One snap stop of a [`MenuRow::Stops`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Stop {
    /// The stop's word on the track (`medium`), which a screen reader is told as the value.
    pub(crate) label: String,
    /// What picking it does.
    pub(crate) act: MenuAct,
    pub(crate) enabled: bool,
}

/// The cells each of `stops` takes, mark and label, in a slider `room` cells wide, from the
/// row's first text cell: the first stop at the left end, the last at the right, the track
/// shared evenly between them. The painter and the pointer both read it, so a press lands on
/// the stop drawn under it. In a row too narrow for every label the later spans are cut at
/// `room`.
#[must_use]
pub(crate) fn stop_spans(stops: &[Stop], room: usize) -> Vec<std::ops::Range<usize>> {
    let widths: Vec<usize> = stops
        .iter()
        .map(|stop| STOP_MARK_CELLS + display_cells(&stop.label))
        .collect();
    let gaps = stops.len().saturating_sub(1);
    let track = room.saturating_sub(widths.iter().sum());
    let mut at = 0;
    widths
        .iter()
        .enumerate()
        .map(|(i, width)| {
            if i > 0 {
                // An uneven track gives its odd cells to the leftmost gaps; a row with no room
                // keeps one cell between two stops.
                at += (track / gaps + usize::from(i <= track % gaps)).max(1);
            }
            let span = at.min(room)..(at + width).min(room);
            at += width;
            span
        })
        .collect()
}

/// The stop of a slider a press at `cell`, counted from the row's first text cell, snaps to:
/// the one whose span ([`stop_spans`]) holds it, else the nearer of the two it lies between.
#[must_use]
pub(crate) fn stop_near(spans: &[std::ops::Range<usize>], cell: usize) -> Option<usize> {
    let last = spans.len().checked_sub(1)?;
    Some(
        spans
            .windows(2)
            .position(|pair| 2 * cell < pair[0].end + pair[1].start)
            .unwrap_or(last),
    )
}

/// The enabled stop next to `from` on a slider, toward its `up` end; from no stop, the first
/// enabled one from the other end. `None` where there is none that way: a slider's ends do not
/// wrap.
#[must_use]
pub(crate) fn stop_step(stops: &[Stop], from: Option<usize>, up: bool) -> Option<usize> {
    let live = |i: &usize| stops[*i].enabled;
    match (from, up) {
        (Some(from), true) => (from + 1..stops.len()).find(live),
        (Some(from), false) => (0..from.min(stops.len())).rev().find(live),
        (None, true) => (0..stops.len()).find(live),
        (None, false) => (0..stops.len()).rev().find(live),
    }
}

impl MenuRow {
    /// A note of `text`, spoken in its words ([`spoken_words`]).
    #[must_use]
    pub(crate) fn note(text: impl Into<String>) -> Self {
        let text = text.into();
        let spoken = spoken_words(&text);
        Self::Note {
            text,
            spoken,
            tone: NoteTone::Label,
        }
    }

    /// A note of `text` a screen reader is told as `spoken`.
    #[must_use]
    pub(crate) fn note_spoken(text: impl Into<String>, spoken: impl Into<String>) -> Self {
        Self::Note {
            text: text.into(),
            spoken: spoken.into(),
            tone: NoteTone::Label,
        }
    }

    /// A drawn row of `cells` a screen reader is told as `spoken`.
    #[must_use]
    pub(crate) fn plot(cells: impl Into<Box<[PlotCell]>>, spoken: impl Into<String>) -> Self {
        Self::Plot {
            cells: cells.into(),
            spoken: spoken.into(),
        }
    }

    /// A slider named `name` of `stops`, its thumb on `at`.
    #[must_use]
    pub(crate) fn stops(
        name: impl Into<String>,
        stops: impl Into<Box<[Stop]>>,
        at: Option<usize>,
    ) -> Self {
        Self::Stops {
            name: name.into(),
            stops: stops.into(),
            at,
            focus: None,
        }
    }

    /// Whether a click or Return can choose this row.
    #[must_use]
    pub(crate) fn selectable(&self) -> bool {
        match self {
            Self::Item { enabled, .. } => *enabled,
            Self::Stops { stops, .. } => stops.iter().any(|stop| stop.enabled),
            _ => false,
        }
    }

    /// The stop a slider's choice picks: the one the keys or the pointer rest on, else the one
    /// its thumb stands on.
    #[must_use]
    pub(crate) fn rested_stop(&self) -> Option<usize> {
        match self {
            Self::Stops { at, focus, .. } => focus.or(*at),
            _ => None,
        }
    }

    /// The row's words as its cells show them, unpainted.
    pub(crate) fn text(&self) -> String {
        match self {
            Self::Header(text) | Self::Note { text, .. } => text.clone(),
            Self::Name(name) => quoted(name),
            Self::Section(title) => title.clone(),
            Self::Fact { label, value, .. } if value.is_empty() => label.clone(),
            Self::Fact { label, value, .. } => {
                let pad = fact_value_at(label).saturating_sub(display_cells(label));
                format!("{label}{}{value}", " ".repeat(pad))
            }
            Self::Rule | Self::Gap(_) => String::new(),
            Self::Plot { cells, .. } => cells.iter().map(|cell| cell.ch).collect(),
            Self::Item {
                glyph,
                label,
                trail,
                ..
            } => {
                let pad = GLYPH_CELLS.saturating_sub(display_cells(glyph));
                let gap = if trail.is_empty() { 0 } else { TRAIL_GAP_CELLS };
                format!(
                    "{glyph}{}{label}{}{trail}",
                    " ".repeat(pad),
                    " ".repeat(gap)
                )
            }
            Self::Stops { stops, at, .. } => stops
                .iter()
                .enumerate()
                .map(|(i, stop)| {
                    let mark = if *at == Some(i) {
                        STOP_THUMB
                    } else {
                        STOP_MARK
                    };
                    format!("{mark} {}", stop.label)
                })
                .collect::<Vec<_>>()
                .join(" \u{2500}\u{2500}\u{2500} "),
        }
    }
}

/// `text` as a screen reader is told it: its words, the glyphs that mark or
/// draw (`◆`, `⎇`, `▱▱▱`, an arrow) left out, and a `·` or `—` between two
/// parts a comma: `⎆  sol@example.test · max 20x` is `sol@example.test, max 20x`.
/// A bound's sign is a word, `≥` said `at least` and `≤` `at most`: `weekly ≤62% left` is
/// `weekly at most 62% left`, never the bound as an exact figure.
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
        // A bound's sign is a word: `≤30% left` is `at most 30% left`, `≥$40` `at least $40`.
        let bound = token.trim_start_matches(|c| glyph(c) && !matches!(c, '\u{2264}' | '\u{2265}'));
        if bound.starts_with('\u{2265}') {
            out.push_str("at least ");
        } else if bound.starts_with('\u{2264}') {
            out.push_str("at most ");
        }
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

/// Ink role for a [`PlotCell`] (`docs/DESIGN-harness-glass-rainbow-2026-11-05.md` §5.1): each a
/// token of the glass the card wears ([`GlassInks`]), a hued one at its column's palette position
/// over the card's own width.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum PlotInk {
    /// The traffic card's inbound band: the card's accent.
    In,
    /// The traffic card's outbound band: the card's value ink.
    Out,
    /// The day axis, a legend's or a row's words, `▕`: the glass's quiet ink.
    Axis,
    /// A measured level (`▁…█`): the glass's mark at the cell's column, 3:1 or more.
    Read,
    /// `→` and `↺`: the glass's lit ink.
    Mark,
    /// `┄`: the glass's track, 3:1 or more.
    Track,
    /// `╳` and `$`: the glass's warn ink, bold, on its wash ([`WARN_WASH`]).
    Warn,
    /// A name inside a chart: the glass's value ink.
    Value,
}

/// The ink a [`MenuRow::Note`]'s words, or a [`MenuRow::Fact`]'s value, take.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) enum NoteTone {
    /// The card's label ink, as every note before the outlook.
    #[default]
    Label,
    /// The value ink (the outlook's verdict).
    Value,
    /// The warn ink floored to 4.5:1 on the card (a projected stop, its `⧗` leading).
    Warn,
}

impl NoteTone {
    /// The tone's word on a `controls` line.
    const fn key(self) -> &'static str {
        match self {
            Self::Label => "label",
            Self::Value => "value",
            Self::Warn => "warn",
        }
    }
}

/// The cell of a fact's row, from its first text cell, its value begins at: the name column's
/// end ([`FACT_LABEL_CELLS`]), or one blank past a name that outruns the column.
fn fact_value_at(label: &str) -> usize {
    match display_cells(label) {
        0 => FACT_LABEL_CELLS,
        cells => FACT_LABEL_CELLS.max(cells + 1),
    }
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

fn same_choice(a: &MenuRow, b: &MenuRow) -> bool {
    match (a, b) {
        (
            MenuRow::Item {
                act: act_a,
                label: label_a,
                enabled: enabled_a,
                trail: trail_a,
                ..
            },
            MenuRow::Item {
                act: act_b,
                label: label_b,
                enabled: enabled_b,
                trail: trail_b,
                ..
            },
        ) => act_a == act_b && label_a == label_b && enabled_a == enabled_b && trail_a == trail_b,
        (MenuRow::Stops { stops: a, .. }, MenuRow::Stops { stops: b, .. }) => a == b,
        _ => false,
    }
}

/// The cells `row` takes: its text's ([`MenuRow::text`]), counted without building it, since a
/// text's cells are the sum of its characters' ([`display_cells`]); a card's width is read every
/// frame it is painted ([`side_pad`]), and so allocates nothing.
pub(crate) fn row_width(row: &MenuRow) -> usize {
    match row {
        MenuRow::Gap(cells) => *cells,
        MenuRow::Plot { cells, .. } => cells.len(),
        MenuRow::Item {
            glyph,
            label,
            trail,
            ..
        } => {
            let trail = match display_cells(trail) {
                0 => 0,
                cells => TRAIL_GAP_CELLS + cells,
            };
            display_cells(glyph).max(GLYPH_CELLS) + display_cells(label) + trail + CHECK_CELLS
        }
        MenuRow::Header(text) | MenuRow::Note { text, .. } => display_cells(text),
        // The heading's blank and one cell of its rule at least.
        MenuRow::Section(title) => display_cells(title) + 2,
        MenuRow::Fact { label, value, .. } if value.is_empty() => display_cells(label),
        MenuRow::Fact { label, value, .. } => fact_value_at(label) + display_cells(value),
        MenuRow::Name(name) => {
            display_cells("\u{201c}") + display_cells(name) + display_cells("\u{201d}")
        }
        MenuRow::Stops { stops, .. } => {
            stops
                .iter()
                .map(|stop| STOP_MARK_CELLS + display_cells(&stop.label))
                .sum::<usize>()
                + stops.len().saturating_sub(1) * STOP_TRACK_CELLS
        }
        MenuRow::Rule => 0,
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
    if rows.iter().any(|row| matches!(row, MenuRow::Plot { .. })) {
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

/// The fewest rows a card of `rows` opens in, its border included: its head, the rows before
/// its first rule or group heading, so a card of sections needs room for what it says first and
/// scrolls the rest.
fn least_rows(rows: &[MenuRow]) -> usize {
    rows.iter()
        .take_while(|row| !matches!(row, MenuRow::Rule | MenuRow::Section(_)))
        .count()
        + 2
}

fn same_place(shown: &MenuRow, fresh: &MenuRow) -> bool {
    match (shown, fresh) {
        (MenuRow::Header(_), MenuRow::Header(_))
        | (MenuRow::Name(_), MenuRow::Name(_))
        | (MenuRow::Note { .. }, MenuRow::Note { .. })
        | (MenuRow::Plot { .. }, MenuRow::Plot { .. })
        | (MenuRow::Rule, MenuRow::Rule) => true,
        (MenuRow::Section(a), MenuRow::Section(b)) => a == b,
        (MenuRow::Fact { label: a, .. }, MenuRow::Fact { label: b, .. }) => a == b,
        (MenuRow::Item { act: a, .. }, MenuRow::Item { act: b, .. }) => a == b,
        (MenuRow::Stops { stops: a, .. }, MenuRow::Stops { stops: b, .. }) => a
            .iter()
            .map(|stop| stop.act)
            .eq(b.iter().map(|stop| stop.act)),
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
pub(crate) fn row_at(
    rect: MenuRect,
    viewport: MenuViewport,
    total: usize,
    frame_row: usize,
) -> Option<usize> {
    if frame_row <= rect.row || frame_row + 1 >= rect.row + rect.h {
        return None;
    }
    viewport.absolute(total, rect.h.saturating_sub(2), frame_row - rect.row - 1)
}

/// Selectable row index at frame `(row, col)`.
#[must_use]
pub(crate) fn item_at(
    rect: MenuRect,
    rows: &[MenuRow],
    viewport: MenuViewport,
    row: usize,
    col: usize,
) -> Option<usize> {
    if !rect.contains(row, col) {
        return None;
    }
    let i = row_at(rect, viewport, rows.len(), row)?;
    rows.get(i).filter(|r| r.selectable()).map(|_| i)
}

/// A pointer's place on the card: a selectable row, and on a slider the stop under the column.
pub(crate) type Hit = (usize, Option<usize>);

/// The selectable row at frame `(row, col)` ([`item_at`]) and, on a slider, the stop the column
/// snaps to ([`stop_near`] of the row's [`stop_spans`]).
#[must_use]
pub(crate) fn hit_at(
    rect: MenuRect,
    rows: &[MenuRow],
    viewport: MenuViewport,
    row: usize,
    col: usize,
) -> Option<Hit> {
    let i = item_at(rect, rows, viewport, row, col)?;
    let MenuRow::Stops { stops, .. } = &rows[i] else {
        return Some((i, None));
    };
    let (text_start, text_end) = text_span(rect.w, rows);
    let cell = col.saturating_sub(rect.col + text_start);
    let spans = stop_spans(stops, text_end.saturating_sub(text_start));
    Some((i, stop_near(&spans, cell)))
}

/// Advance highlight through every selectable logical row, wrapping at either end.
#[must_use]
pub(crate) fn step(rows: &[MenuRow], from: Option<usize>, down: bool) -> Option<usize> {
    let live: Vec<usize> = rows
        .iter()
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

fn page(
    rows: &[MenuRow],
    from: Option<usize>,
    down: bool,
    viewport: MenuViewport,
    shown: usize,
) -> Option<usize> {
    if rows.is_empty() {
        return None;
    }
    let at = from
        .filter(|at| *at < rows.len())
        .unwrap_or(viewport.range(rows.len(), shown).start);
    let target = if down {
        at.saturating_add(shown.max(1))
            .min(rows.len().saturating_sub(1))
    } else {
        at.saturating_sub(shown.max(1))
    };
    let enabled = |i: &usize| rows[*i].selectable();
    if down {
        (target..rows.len())
            .find(enabled)
            .or_else(|| (0..=target).rev().find(enabled))
    } else {
        (0..=target)
            .rev()
            .find(enabled)
            .or_else(|| (target..rows.len()).find(enabled))
    }
}

/// Key action for an open menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MenuKey {
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    /// Toward a slider's low end, and toward its high end ([`MenuRow::Stops`]).
    Left,
    Right,
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
        Key::Named(Nk::Home | Nk::NumpadHome) => MenuKey::Home,
        Key::Named(Nk::End | Nk::NumpadEnd) => MenuKey::End,
        Key::Named(Nk::PageUp | Nk::NumpadPageUp) => MenuKey::PageUp,
        Key::Named(Nk::PageDown | Nk::NumpadPageDown) => MenuKey::PageDown,
        Key::Named(Nk::ArrowLeft | Nk::NumpadArrowLeft) => MenuKey::Left,
        Key::Named(Nk::ArrowRight | Nk::NumpadArrowRight) => MenuKey::Right,
        Key::Named(Nk::Enter | Nk::NumpadEnter | Nk::Space) | Key::Character(' ') => {
            MenuKey::Choose
        }
        Key::Named(Nk::Escape) => MenuKey::Close,
        _ => MenuKey::PassThrough,
    }
}

/// The glass a card wears ([`paint`]): the inks and the card's painted height.
struct CardGlass {
    inks: GlassInks,
    /// The rows the card paints, its border's included.
    rows: usize,
    /// Its rules are drawn inside the border, so the arc runs whole round the card.
    arc: bool,
}

impl CardGlass {
    /// The border's ink at `row`, `col` of a card `w` cells wide.
    fn border(&self, row: usize, col: usize, w: usize) -> [u8; 3] {
        unpack(self.inks.border(row, col, self.rows, w))
    }
}

const fn pack(c: [u8; 3]) -> u32 {
    u32::from_be_bytes([0, c[0], c[1], c[2]])
}

const fn unpack(c: u32) -> [u8; 3] {
    [(c >> 16) as u8, (c >> 8) as u8, c as u8]
}

/// The card's colours and the glass's inks on them ([`paint`]): a fact card the glass opens
/// stands on the pane's own ground, the ground the inks are solved on; with no palette, on the
/// band's grey.
fn card_inks(
    theme: Theme,
    palette: Option<CardPalette>,
) -> (crate::tab_menu::MenuColors, GlassInks) {
    let c = match palette {
        Some(_) => crate::tab_menu::glass_menu_colors(theme),
        None => crate::tab_menu::menu_colors(theme),
    };
    let inks = GlassInks::of(
        palette,
        pack(c.card_bg),
        theme.fg & 0x00FF_FFFF,
        pack(c.rule),
    );
    (c, inks)
}

/// The card columns a row's text (and a drawn row's cells) take in a card `w` cells wide:
/// inside the border and the side pad.
fn text_span(w: usize, rows: &[MenuRow]) -> (usize, usize) {
    let pad = side_pad(w, rows);
    (1 + pad, w.saturating_sub(1 + pad))
}

/// Render the menu card into `rect.h` rows of `rect.w` cells, wearing `palette`'s glass
/// (`docs/DESIGN-harness-glass-rainbow-2026-11-05.md` §1.4, §4.1): a fact card the glass opens
/// wears its pane's Autopilot palette on the pane's own ground ([`crate::tab_menu::glass_menu_colors`],
/// the ground the glass's inks are solved on), its border the static arc
/// ([`GlassInks::border`]), its rules inside the border, its drawn rows the glass's tokens with
/// each column's hue and a warn cell on a wash of the warn ink ([`WARN_WASH`]); with no palette
/// (a pane with no harness theme) the card stands on the band's grey, every token is neutral
/// and the border is the card's rule ink.
#[must_use]
pub(crate) fn paint(
    rect: MenuRect,
    rows: &[MenuRow],
    viewport: MenuViewport,
    highlight: Option<usize>,
    theme: Theme,
    palette: Option<CardPalette>,
) -> Vec<Vec<RenderCell>> {
    let (c, inks) = card_inks(theme, palette);
    let w = rect.w;
    let (text_start, text_end) = text_span(w, rows);
    let cell = crate::chrome_band::cell;
    let visible = viewport.range(rows.len(), rect.h.saturating_sub(2));
    let glass = CardGlass {
        inks,
        rows: (visible.len() + 2).min(rect.h),
        arc: palette.is_some(),
    };
    let edge = |r: usize, left: char, fill: char, right: char| {
        (0..w)
            .map(|col| {
                let ch = match col {
                    0 => left,
                    col if col + 1 == w => right,
                    _ => fill,
                };
                cell(ch, glass.border(r, col, w), c.card_bg, false, false)
            })
            .collect::<Vec<_>>()
    };
    let sides = |r: usize, row: &mut [RenderCell]| {
        row[0] = cell('\u{2502}', glass.border(r, 0, w), c.card_bg, false, false);
        if w >= 2 {
            row[w - 1] = cell(
                '\u{2502}',
                glass.border(r, w - 1, w),
                c.card_bg,
                false,
                false,
            );
        }
    };
    let mut out = Vec::with_capacity(rect.h);
    out.push(edge(0, '\u{250C}', '\u{2500}', '\u{2510}'));
    if visible.start > 0 && w > 2 {
        out[0][w - 2] = cell('↑', c.band.value, c.card_bg, true, false);
    }
    for (i, row) in rows
        .iter()
        .enumerate()
        .take(visible.end)
        .skip(visible.start)
    {
        let r = out.len();
        if matches!(row, MenuRow::Rule) {
            if glass.arc {
                // Inside the border, so the arc runs whole round the card.
                let mut cells = vec![cell(' ', c.rule, c.card_bg, false, false); w];
                sides(r, &mut cells);
                for slot in cells.iter_mut().take(text_end).skip(text_start) {
                    *slot = cell('\u{2500}', c.rule, c.card_bg, false, false);
                }
                out.push(cells);
            } else {
                out.push(edge(r, '\u{251C}', '\u{2500}', '\u{2524}'));
            }
            continue;
        }
        if let MenuRow::Plot { cells: plot, .. } = row {
            let mut cells = paint_plot(w, (text_start, text_end), plot, &c, &glass.inks);
            sides(r, &mut cells);
            out.push(cells);
            continue;
        }
        if let MenuRow::Section(title) = row {
            // Inside the border on every card, as a glass card's rule is: the heading's words
            // begin where every row's do.
            let mut cells = vec![cell(' ', c.rule, c.card_bg, false, false); w];
            sides(r, &mut cells);
            let title = ellipsize(title, text_end.saturating_sub(text_start));
            let rule_from = text_start + display_cells(&title) + 1;
            let (fg, bg) = (c.band.value, c.card_bg);
            crate::tab_menu::put_text(&mut cells, text_start, text_end, &title, fg, bg, true);
            for slot in cells.iter_mut().take(text_end).skip(rule_from) {
                *slot = cell('\u{2500}', c.rule, c.card_bg, false, false);
            }
            out.push(cells);
            continue;
        }
        if let MenuRow::Fact {
            label, value, tone, ..
        } = row
        {
            let bg = c.card_bg;
            let mut cells = vec![cell(' ', c.band.label, bg, false, false); w];
            sides(r, &mut cells);
            crate::tab_menu::put_text(
                &mut cells,
                text_start,
                text_end,
                &ellipsize(label, text_end.saturating_sub(text_start)),
                c.band.label,
                bg,
                false,
            );
            let (fg, bold) = match tone {
                NoteTone::Label => (c.band.label, false),
                NoteTone::Value => (c.band.value, false),
                NoteTone::Warn => (unpack(glass.inks.warn()), true),
            };
            let from = (text_start + fact_value_at(label)).min(text_end);
            let value = ellipsize(value, text_end - from);
            crate::tab_menu::put_text(&mut cells, from, text_end, &value, fg, bg, bold);
            out.push(cells);
            continue;
        }
        let selected = highlight == Some(i) && row.selectable();
        if let MenuRow::Stops { stops, at, .. } = row {
            let mut cells = vec![cell(' ', c.band.value, c.card_bg, false, false); w];
            sides(r, &mut cells);
            let rested = row.rested_stop().filter(|_| selected);
            paint_stops(&mut cells, (text_start, text_end), (stops, *at, rested), &c);
            out.push(cells);
            continue;
        }
        let bg = if selected { c.sel_bg } else { c.card_bg };
        let (fg, bold) = match row {
            MenuRow::Header(_) | MenuRow::Name(_) => (c.band.value, true),
            MenuRow::Note {
                tone: NoteTone::Value,
                ..
            } => (c.band.value, false),
            MenuRow::Note {
                tone: NoteTone::Warn,
                ..
            } => (unpack(glass.inks.warn()), true),
            MenuRow::Note { .. } | MenuRow::Item { enabled: false, .. } => (c.disabled, false),
            MenuRow::Item { .. } if selected => (c.sel_fg, true),
            _ => (c.band.value, false),
        };
        let mut cells = vec![cell(' ', fg, bg, false, false); w];
        sides(out.len(), &mut cells);
        let checked = matches!(row, MenuRow::Item { checked: true, .. });
        // An item's trailing words end where a check would begin, on a checked row or not, so
        // a card's trails share one right edge; the label has the room before them.
        let trail = match row {
            MenuRow::Item { trail, .. } if !trail.is_empty() => Some(trail.as_str()),
            _ => None,
        };
        let held = if checked || trail.is_some() {
            CHECK_CELLS
        } else {
            0
        };
        let trail_end = text_end.saturating_sub(held).max(text_start);
        let trail_from = trail.map_or(trail_end, |trail| {
            trail_end
                .saturating_sub(display_cells(trail))
                .max(text_start)
        });
        let room = match trail {
            Some(_) => trail_from
                .saturating_sub(text_start)
                .saturating_sub(TRAIL_GAP_CELLS),
            None => trail_end - text_start,
        };
        let text = match row {
            MenuRow::Name(name) => quoted_in(name, room),
            MenuRow::Item { glyph, label, .. } => {
                let pad = GLYPH_CELLS.saturating_sub(display_cells(glyph));
                ellipsize(&format!("{glyph}{}{label}", " ".repeat(pad)), room)
            }
            row => ellipsize(&row.text(), room),
        };
        crate::tab_menu::put_text(&mut cells, text_start, text_end, &text, fg, bg, bold);
        if let Some(trail) = trail {
            let trail = ellipsize(trail, trail_end - trail_from);
            crate::tab_menu::put_text(&mut cells, trail_from, trail_end, &trail, fg, bg, bold);
        }
        if checked && text_end >= 1 {
            cells[text_end - 1] = cell('\u{25cf}', fg, bg, bold, false);
        }
        out.push(cells);
    }
    if out.len() < rect.h {
        out.push(edge(out.len(), '\u{2514}', '\u{2500}', '\u{2518}'));
    }
    if visible.end < rows.len()
        && w > 2
        && let Some(bottom) = out.last_mut()
    {
        bottom[w - 2] = cell('↓', c.band.value, c.card_bg, true, false);
    }
    out.truncate(rect.h);
    out
}

/// A slider's cells inside the border (its sides are the caller's), each stop on its span
/// ([`stop_spans`]): its mark, the thumb's where the thumb stands, and its label; the track
/// between two stops in the card's rule ink; the stop the keys or the pointer rest on (`rested`,
/// on the card's selected row) on the selection's ground; a stop that cannot be picked in the
/// disabled ink.
fn paint_stops(
    cells: &mut [RenderCell],
    (text_start, text_end): (usize, usize),
    (stops, at, rested): (&[Stop], Option<usize>, Option<usize>),
    c: &crate::tab_menu::MenuColors,
) {
    let cell = crate::chrome_band::cell;
    let spans = stop_spans(stops, text_end.saturating_sub(text_start));
    for (i, (stop, span)) in stops.iter().zip(&spans).enumerate() {
        if let Some(next) = spans.get(i + 1) {
            // A space either side of the track where the gap has room for it.
            let pad = usize::from(next.start - span.end >= 3);
            for col in span.end + pad..next.start - pad {
                cells[text_start + col] = cell('\u{2500}', c.rule, c.card_bg, false, false);
            }
        }
        let thumb = at == Some(i);
        let (fg, bg) = match (stop.enabled, rested == Some(i)) {
            (false, _) => (c.disabled, c.card_bg),
            (true, true) => (c.sel_fg, c.sel_bg),
            (true, false) => (c.band.value, c.card_bg),
        };
        let mark = if thumb { STOP_THUMB } else { STOP_MARK };
        let text = format!("{mark} {}", stop.label);
        for col in span.clone() {
            cells[text_start + col] = cell(' ', fg, bg, false, false);
        }
        crate::tab_menu::put_text(
            cells,
            text_start + span.start,
            text_start + span.end,
            &ellipsize(&text, span.len()),
            fg,
            bg,
            thumb,
        );
    }
}

/// How far a warn cell's ground is mixed toward the warn ink on a drawn row: where the warn ink
/// has a hue of its own (Cruise, Land), a stop and an out line stand on a wash of it, so they
/// read apart from the projected line's letters even where a letter takes the warn's hue (Land's
/// sunset at its orange); the warn ink is floored along its hue to 4.5:1 on the wash.
pub(crate) const WARN_WASH: f32 = 0.16;

/// A warn cell's ground and ink on a drawn row: a wash of the warn ink ([`WARN_WASH`]) where it
/// has a hue of its own, the warn ink floored to 4.5:1 on it; `None` where it has none.
fn warn_wash(c: &crate::tab_menu::MenuColors, glass: &GlassInks) -> Option<([u8; 3], [u8; 3])> {
    (glass.warn() != glass.lit()).then(|| {
        let wash = crate::chrome_band::mix3(c.card_bg, unpack(glass.warn()), WARN_WASH);
        let ink = aterm_effects::color_math::floor_contrast_keeping_hue(
            glass.warn(),
            pack(wash),
            aterm_effects::cli_marks::NAME_CONTRAST,
        );
        (wash, unpack(ink))
    })
}

/// A drawn row's cells inside the border (its sides are the caller's): each cell in its ink,
/// a hued one at its column's palette position over the card's own `w` cells, and a warn cell
/// on its wash ([`warn_wash`]).
fn paint_plot(
    w: usize,
    (text_start, text_end): (usize, usize),
    plot: &[PlotCell],
    c: &crate::tab_menu::MenuColors,
    glass: &GlassInks,
) -> Vec<RenderCell> {
    let cell = crate::chrome_band::cell;
    let busy_bg = crate::chrome_band::mix3(c.card_bg, c.band.accent, 0.10);
    let warn_on = warn_wash(c, glass);
    let mut cells = vec![cell(' ', c.band.value, c.card_bg, false, false); w];
    for (col, p) in (text_start..text_end).zip(plot.iter()) {
        let at = field(col, w);
        let (fg, bold) = match p.ink {
            PlotInk::In => (c.band.accent, false),
            PlotInk::Out => (c.band.value, false),
            PlotInk::Axis => (unpack(glass.quiet()), false),
            PlotInk::Read => (unpack(glass.mark(at)), false),
            PlotInk::Mark => (unpack(glass.lit()), false),
            PlotInk::Track => (unpack(glass.track()), false),
            PlotInk::Warn => (warn_on.map_or(unpack(glass.warn()), |(_, ink)| ink), true),
            PlotInk::Value => (unpack(glass.value()), false),
        };
        let bg = match (p.busy, warn_on) {
            (true, _) => busy_bg,
            (false, Some((wash, _))) if p.ink == PlotInk::Warn => wash,
            (false, _) => c.card_bg,
        };
        cells[col] = cell(p.ch, fg, bg, bold, false);
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

/// Open fact popup menu state.
#[derive(Clone, Debug)]
pub(crate) struct AutopilotMenu {
    pub(crate) owner: MenuOwner,
    pub(crate) wid: WindowId,
    pub(crate) session: u64,
    pub(crate) focus: Option<u64>,
    pub(crate) anchor: (usize, usize),
    pub(crate) rows: Vec<MenuRow>,
    pub(crate) spoken: String,
    pub(crate) highlight: Option<usize>,
    pub(crate) viewport: MenuViewport,
    /// Last painted capacity, retained while a changed viewport awaits its next frame.
    pub(crate) page_rows: usize,
    /// Partial wheel gestures belong to this popup, never to the covered pane.
    pub(crate) wheel_residual: f64,
    pub(crate) wheel_residual_x: f64,
    pub(crate) rect: Option<MenuRect>,
    pub(crate) laid: Option<crate::app_mouse::GlassGrid>,
    pub(crate) generation: u64,
    pub(crate) rest: PointerRest,
    /// What the permission chip's box says and the cells of content its words were wrapped to
    /// ([`crate::permission_box`]); `None` on a fact's card.
    pub(crate) told: Option<(crate::permission_box::Told, usize)>,
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
    pub(crate) fn reveal_highlight(&mut self) {
        if self
            .viewport
            .reveal(self.rows.len(), self.page_rows, self.highlight)
        {
            // Until painted, the new logical rows have no corresponding pixel hit targets.
            self.rect = None;
            self.generation = self.generation.wrapping_add(1);
        }
    }

    /// The choice at `hit`: an item's own, or the one of a slider's stop under the pointer.
    fn choice_at(&self, (i, stop): Hit) -> Option<Choice> {
        match self.rows.get(i)? {
            MenuRow::Item {
                act,
                label,
                enabled,
                ..
            } => Some((*act, label.clone(), *enabled)),
            MenuRow::Stops { stops, .. } => {
                let stop = stops.get(stop?)?;
                Some((stop.act, stop.label.clone(), stop.enabled))
            }
            _ => None,
        }
    }

    fn note_pointer(&mut self, px: (f64, f64), frame_row: Option<usize>, hovered: Option<Hit>) {
        if self.rest.px != Some(px) {
            self.rest_on(px, frame_row, hovered);
        }
    }

    fn rest_on(&mut self, px: (f64, f64), frame_row: Option<usize>, hovered: Option<Hit>) {
        self.rest = PointerRest {
            px: Some(px),
            frame_row,
            choice: hovered.and_then(|hit| self.choice_at(hit)),
            generation: self.generation,
        };
    }

    /// Whether a press at `(frame_row, hit)` lands without rows having shifted under the
    /// pointer.
    fn press_lands(&self, frame_row: usize, hit: Hit) -> bool {
        let rest = &self.rest;
        rest.generation == self.generation
            || (rest.frame_row == Some(frame_row)
                && rest.choice.is_some()
                && rest.choice == self.choice_at(hit))
    }

    /// Rest the keys or the pointer on `stop` of the slider at row `i`, returning whether that
    /// moved it: a stop that cannot be picked takes no rest.
    fn rest_on_stop(&mut self, i: usize, stop: Option<usize>) -> bool {
        let Some(MenuRow::Stops { stops, focus, .. }) = self.rows.get_mut(i) else {
            return false;
        };
        let stop = stop.filter(|j| stops.get(*j).is_some_and(|stop| stop.enabled));
        if stop.is_none() || *focus == stop {
            return false;
        }
        *focus = stop;
        true
    }

    /// Let every slider's rest go, returning whether one rested: the pointer left their stops.
    fn leave_stops(&mut self) -> bool {
        let mut left = false;
        for row in &mut self.rows {
            if let MenuRow::Stops { focus, .. } = row {
                left |= focus.take().is_some();
            }
        }
        left
    }

    /// The slider the Left and Right keys move: the selected row where it is one, else the
    /// card's first.
    fn slider_row(&self) -> Option<usize> {
        let is_slider = |i: &usize| matches!(self.rows.get(*i), Some(MenuRow::Stops { .. }));
        self.highlight.filter(is_slider).or_else(|| {
            self.rows
                .iter()
                .position(|row| matches!(row, MenuRow::Stops { .. }) && row.selectable())
        })
    }

    /// Take `rows` whole in place of a card's with no row to choose (the permission chip's
    /// box): nothing is selected, and the card is laid afresh.
    pub(crate) fn replace_rows(&mut self, rows: Vec<MenuRow>, spoken: String) {
        self.rows = rows;
        self.spoken = spoken;
        self.highlight = None;
        self.viewport = MenuViewport::default();
        self.rect = None;
        self.generation = self.generation.wrapping_add(1);
    }

    /// Rebuild menu rows in place, returning whether the card must be painted again.
    pub(crate) fn refresh_rows(&mut self, fresh: Vec<MenuRow>, spoken: String) -> bool {
        let kept = keep_places(&self.rows, &fresh);
        let stays = kept.is_some();
        let mut laid = kept.unwrap_or(fresh);
        // Where the keys or the pointer rest on a slider is the card's, not the rebuilt rows':
        // each slider keeps it, in the sliders' order, while the stop is still one to pick.
        let mut rests = self.rows.iter().filter_map(|row| match row {
            MenuRow::Stops { focus, .. } => Some(*focus),
            _ => None,
        });
        for row in &mut laid {
            if let MenuRow::Stops { stops, focus, .. } = row {
                *focus = rests
                    .next()
                    .flatten()
                    .filter(|j| stops.get(*j).is_some_and(|stop| stop.enabled));
            }
        }
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
        if self
            .viewport
            .reveal(self.rows.len(), self.page_rows, self.highlight)
        {
            self.rect = None;
        }
        self.generation = self.generation.wrapping_add(1);
        true
    }

    /// Format `controls fact-menu` lines.
    #[must_use]
    pub(crate) fn controls_lines(&self, sid: &str) -> Vec<String> {
        let target = "fact-menu";
        let fact = match self.owner {
            MenuOwner::Fact(fact) => format!(" fact={}", fact.key()),
            MenuOwner::Permission => " box=permission".to_owned(),
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
                MenuRow::Section(title) => {
                    out.push(format!("{target} section index={i} text={title:?}"));
                }
                MenuRow::Fact {
                    label, value, tone, ..
                } => out.push(format!(
                    "{target} fact index={i} label={label:?} tone={} text={value:?}",
                    tone.key()
                )),
                MenuRow::Gap(_) => out.push(format!("{target} gap index={i}")),
                MenuRow::Header(text) | MenuRow::Note { text, .. } => {
                    out.push(format!("{target} label index={i} text={text:?}"));
                }
                MenuRow::Name(_) => {
                    out.push(format!("{target} label index={i} text={:?}", row.text()));
                }
                MenuRow::Plot { .. } => {
                    out.push(format!("{target} plot index={i} text={:?}", row.text()));
                }
                MenuRow::Stops {
                    stops, at, focus, ..
                } => {
                    out.push(format!(
                        "{target} slider index={i} stops={} at={} selected={} text={:?}",
                        stops.len(),
                        at.and_then(|at| stops.get(at))
                            .map_or("none", |stop| stop.label.as_str()),
                        self.highlight == Some(i),
                        row.text()
                    ));
                    for (j, stop) in stops.iter().enumerate() {
                        out.push(format!(
                            "{target} stop index={i} stop={j} key={} enabled={} current={} \
                             rested={} text={:?}",
                            stop.act.key(),
                            stop.enabled,
                            *at == Some(j),
                            *focus == Some(j),
                            stop.label
                        ));
                    }
                }
            }
        }
        out
    }

    /// Compact accessibility epoch ([`crate::a11y_tree::menu_epoch_from`]) over session, spoken
    /// title, and rows.
    #[cfg(a11y_tree)]
    #[must_use]
    fn a11y_epoch(&self) -> u32 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.session.hash(&mut h);
        self.spoken.hash(&mut h);
        self.rows.hash(&mut h);
        self.viewport.hash(&mut h);
        self.rect.hash(&mut h);
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
        // Where the keys or the pointer rest on a slider, which a row's identity leaves out
        // ([`MenuRow`]'s `Hash`), is painted.
        for row in &self.rows {
            if let MenuRow::Stops { focus, .. } = row {
                focus.hash(&mut h);
            }
        }
        self.viewport.hash(&mut h);
        self.page_rows.hash(&mut h);
        h.finish() | 1
    }
}

impl std::hash::Hash for MenuRow {
    fn hash<H: std::hash::Hasher>(&self, h: &mut H) {
        match self {
            Self::Header(t) => (0u8, t).hash(h),
            Self::Name(t) => (5u8, t).hash(h),
            Self::Note { text, tone, .. } => (1u8, text, tone).hash(h),
            Self::Rule => 2u8.hash(h),
            Self::Section(title) => (6u8, title).hash(h),
            Self::Fact {
                label, value, tone, ..
            } => (9u8, label, value, tone).hash(h),
            Self::Gap(cells) => (4u8, cells).hash(h),
            // A drawn row is what it shows in cells and what a reader is told of it.
            Self::Plot { cells, spoken } => (7u8, cells, spoken).hash(h),
            Self::Item {
                label,
                act,
                enabled,
                checked,
                trail,
                ..
            } => (3u8, label, act.key(), enabled, checked, trail).hash(h),
            // A slider is its name, its stops and where its thumb stands. Where the keys or
            // the pointer rest moves under them while nothing it says moves, so it stays out
            // of the reader identity; the repaint key hashes it.
            Self::Stops {
                name, stops, at, ..
            } => {
                (8u8, name, at).hash(h);
                for stop in stops {
                    (&stop.label, stop.act.key(), stop.enabled).hash(h);
                }
            }
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
    /// The Autopilot palette the glass of `session`'s pane wears
    /// ([`crate::autopilot::harness_theme`]), which a fact card it opens wears too.
    pub(crate) fn card_palette(&self, session: u64) -> Option<CardPalette> {
        let (word, view, supervised) = self.session_autopilot(session);
        crate::autopilot::harness_theme(
            view,
            supervised,
            word,
            self.unhosted_glass(self.session_agent(session)),
        )
    }

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

    /// Close any open menu and request a redraw.
    pub(crate) fn close_autopilot_menu(&mut self) -> bool {
        let Some(menu) = self.autopilot_menu.take() else {
            return false;
        };
        if matches!(menu.owner, MenuOwner::Fact(Fact::Account)) {
            crate::accounts_outlook::card_open(None);
        }
        self.account_menu_read.close();
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

    /// Whether the menu open in `wid` still has its focus and its anchor row uncovered by
    /// chrome. The permission chip's box also stays while the click that opened it stands, its
    /// chip gone with the agent between the signal and the relaunch
    /// ([`Self::permission_box_anchored`]).
    pub(crate) fn autopilot_menu_anchored(&self, wid: WindowId) -> bool {
        let Some(menu) = self.autopilot_menu.as_ref().filter(|m| m.wid == wid) else {
            return false;
        };
        if self.focused_session_id(wid) != menu.focus
            || menu.anchor.0 < self.top_confirmation_band_rows(wid)
        {
            return false;
        }
        match menu.owner {
            MenuOwner::Fact(fact) => self.shows_fact_on_row(wid, menu.session, fact, menu.anchor.0),
            MenuOwner::Permission => self.permission_box_anchored(wid, menu.session, menu.anchor.0),
        }
    }

    fn paint_autopilot_menu(&mut self, wid: WindowId) -> Option<crate::tab_menu::MenuRect> {
        self.fit_permission_box(wid);
        let menu = self.autopilot_menu.as_ref().filter(|m| m.wid == wid)?;
        let (session, anchor) = (menu.session, menu.anchor);
        if !self.autopilot_menu_anchored(wid) {
            self.close_autopilot_menu();
            return None;
        }
        let top = usize::from(self.chrome_rows(wid)).max(self.top_confirmation_band_rows(wid));
        let theme = self.overlay_card_theme(wid);
        // A fact card wears the glass of its session's pane: its Autopilot palette, as the
        // pane's lines do (`docs/DESIGN-harness-glass-rainbow-2026-11-05.md` §1.4).
        let palette = self.card_palette(session);
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
        let menu = self.autopilot_menu.as_mut()?;
        menu.page_rows = rect.h.saturating_sub(2);
        if menu
            .viewport
            .reveal(menu.rows.len(), menu.page_rows, menu.highlight)
        {
            menu.generation = menu.generation.wrapping_add(1);
        }
        let laid = self.glass_grid(wid);
        let menu = self.autopilot_menu.as_ref()?;
        let arrived = (menu.rect != Some(rect) || menu.laid != laid)
            && menu
                .rest
                .px
                .and_then(|(x, y)| self.frame_cell_at(wid, x, y))
                .is_some_and(|(row, col)| rect.contains(row, col));
        let painted = paint(
            rect,
            &menu.rows,
            menu.viewport,
            menu.highlight,
            theme,
            palette,
        );
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

    /// Execute selectable row `i`: an item's act, the menu closed; a slider's rested stop, the
    /// menu left open. A rest on the stop the thumb already stands on picks nothing: the level
    /// is the one in use.
    pub(crate) fn choose_autopilot_menu_row(&mut self, i: usize) {
        let Some(menu) = self.autopilot_menu.as_ref() else {
            return;
        };
        let (wid, session) = (menu.wid, menu.session);
        // A slider's pick leaves the card open: the stop the keys or the pointer rest on.
        if let Some(row @ MenuRow::Stops { stops, at, .. }) = menu.rows.get(i) {
            let picked = row
                .rested_stop()
                .filter(|j| Some(*j) != *at)
                .and_then(|j| stops.get(j))
                .filter(|stop| stop.enabled)
                .map(|stop| stop.act);
            if let Some(MenuAct::Fact(fact, act)) = picked {
                self.choose_fact_stop(wid, session, fact, act);
            }
            return;
        }
        let Some(MenuRow::Item {
            act: MenuAct::Fact(fact, act),
            enabled: true,
            ..
        }) = menu.rows.get(i).cloned()
        else {
            return;
        };
        if matches!(act, FactAct::Type(_) | FactAct::SetModel(..)) {
            let (anchor, keyboard) = (menu.anchor, menu.highlight.is_some());
            self.choose_fact_type(wid, session, fact, act, anchor, keyboard);
            return;
        }
        self.close_autopilot_menu();
        self.choose_fact_act(wid, session, act);
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
        if meaning != MenuKey::Inert {
            self.account_menu_read.cancel_keyboard_selection();
        }
        match meaning {
            MenuKey::Inert => true,
            MenuKey::Up | MenuKey::Down => {
                let down = meaning == MenuKey::Down;
                menu.highlight = step(&menu.rows, menu.highlight, down);
                menu.reveal_highlight();
                self.request_menu_frame(wid);
                true
            }
            MenuKey::Home | MenuKey::End | MenuKey::PageUp | MenuKey::PageDown => {
                menu.highlight = if meaning == MenuKey::Home {
                    menu.rows.iter().position(MenuRow::selectable)
                } else if meaning == MenuKey::End {
                    menu.rows.iter().rposition(MenuRow::selectable)
                } else {
                    let down = meaning == MenuKey::PageDown;
                    let selected = page(
                        &menu.rows,
                        menu.highlight,
                        down,
                        menu.viewport,
                        menu.page_rows,
                    );
                    let amount = isize::try_from(menu.page_rows).unwrap_or(isize::MAX);
                    if menu.viewport.scroll(
                        menu.rows.len(),
                        menu.page_rows,
                        if down { amount } else { -amount },
                    ) {
                        menu.rect = None;
                        menu.generation = menu.generation.wrapping_add(1);
                    }
                    selected
                };
                menu.reveal_highlight();
                self.request_menu_frame(wid);
                true
            }
            MenuKey::Left | MenuKey::Right => {
                // A slider's keys; on a card with none they are any other key.
                let Some(i) = menu.slider_row() else {
                    self.close_autopilot_menu();
                    return false;
                };
                let from = menu.rows[i].rested_stop();
                let to = match &menu.rows[i] {
                    MenuRow::Stops { stops, .. } => {
                        stop_step(stops, from, meaning == MenuKey::Right)
                    }
                    _ => None,
                };
                menu.highlight = Some(i);
                menu.rest_on_stop(i, to);
                menu.reveal_highlight();
                self.request_menu_frame(wid);
                true
            }
            MenuKey::Choose => {
                match menu.highlight {
                    Some(i)
                        if menu.rect.is_some_and(|rect| {
                            menu.viewport
                                .range(menu.rows.len(), rect.h.saturating_sub(2))
                                .contains(&i)
                        }) =>
                    {
                        self.choose_autopilot_menu_row(i)
                    }
                    Some(_) => self.request_menu_frame(wid),
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

    /// Consume wheel input for this modal popup, including at its bounds and before first paint.
    pub(crate) fn autopilot_menu_wheel(
        &mut self,
        wid: WindowId,
        dir: aterm_types::mouse::WheelDir,
        lines: i32,
    ) -> bool {
        let Some(menu) = self.autopilot_menu.as_mut().filter(|menu| menu.wid == wid) else {
            return false;
        };
        self.account_menu_read.cancel_keyboard_selection();
        if let Some(up) = dir.vertical_up() {
            let amount = isize::try_from(lines.max(0)).unwrap_or(isize::MAX);
            let delta = if up { -amount } else { amount };
            if menu.viewport.scroll(menu.rows.len(), menu.page_rows, delta) {
                menu.highlight = None;
                menu.rect = None;
                menu.generation = menu.generation.wrapping_add(1);
                self.request_menu_frame(wid);
            }
        }
        true
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
            InputEvent::Wheel { dir, lines, .. } => self.autopilot_menu_wheel(wid, *dir, *lines),
            _ => {
                self.close_autopilot_menu();
                false
            }
        }
    }

    /// Handle a mouse press in `wid` at `at` while the menu is open.
    pub(crate) fn autopilot_menu_press(
        &mut self,
        wid: WindowId,
        left: bool,
        x: f64,
        y: f64,
        at: std::time::Instant,
    ) -> bool {
        let Some(menu) = self.autopilot_menu.as_ref().filter(|m| m.wid == wid) else {
            return false;
        };
        let (laid, cell) = (menu.laid, self.laid_cell_at(wid, menu.laid, x, y));
        let on_card = menu
            .rect
            .zip(cell)
            .filter(|(rect, (row, col))| rect.contains(*row, *col));
        // The box's own chip stays a chip beside it: a press there is the chip's. A press on
        // the card is the card's, where it covers the chip too (a pane too short to hang it
        // clear of its anchor row).
        if left
            && on_card.is_none()
            && menu.owner == MenuOwner::Permission
            && self.permission_box_chip_press(wid, (x, y), at)
        {
            return true;
        }
        let Some(menu) = self.autopilot_menu.as_ref().filter(|m| m.wid == wid) else {
            return true;
        };
        self.account_menu_read.cancel_keyboard_selection();
        match on_card {
            Some((rect, (row, col))) => {
                let hit = hit_at(rect, &menu.rows, menu.viewport, row, col);
                // A slider's stop that cannot be picked takes no press.
                let hit = hit.filter(|hit| {
                    hit.1.is_none() || menu.choice_at(*hit).is_some_and(|choice| choice.2)
                });
                if left && let Some(hit @ (i, stop)) = hit {
                    if menu.press_lands(row, hit) {
                        if let Some(menu) = self.autopilot_menu.as_mut() {
                            menu.highlight = Some(i);
                            menu.rest_on_stop(i, stop);
                        }
                        self.choose_autopilot_menu_row(i);
                        if self.autopilot_menu.as_ref().is_none_or(|m| m.wid != wid) {
                            self.autopilot_menu_spent = Some(SpentCard {
                                wid,
                                rect,
                                laid,
                                at,
                            });
                        }
                    } else if let Some(menu) = self.autopilot_menu.as_mut() {
                        menu.highlight = Some(i);
                        menu.rest_on_stop(i, stop);
                        menu.rest_on((x, y), Some(row), Some(hit));
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

    /// Swallow the second press, at `at`, of a double-click on the rect a menu choice just closed.
    pub(crate) fn spends_menu_follow_up(&mut self, wid: WindowId, at: std::time::Instant) -> bool {
        let Some(spent) = self.autopilot_menu_spent.take() else {
            return false;
        };
        let (x, y) = self
            .windows
            .get(&wid)
            .map_or((0.0, 0.0), |ws| ws.last_cursor_px);
        spent.wid == wid
            && at.saturating_duration_since(spent.at).as_millis() <= crate::multi_click_ms()
            && spent
                .laid
                .and_then(|laid| laid.cell_at(x, y))
                .is_some_and(|(row, col)| spent.rect.contains(row, col))
    }

    /// Update hover highlight and pointer rest state on mouse motion in `wid`.
    pub(crate) fn autopilot_menu_motion(&mut self, wid: WindowId, x: f64, y: f64) -> bool {
        let Some((rect, laid, moved)) = self
            .autopilot_menu
            .as_ref()
            .filter(|m| m.wid == wid)
            .map(|m| (m.rect, m.laid, m.rest.px != Some((x, y))))
        else {
            return false;
        };
        if moved {
            self.account_menu_read.cancel_keyboard_selection();
        }
        let cell = self.laid_cell_at(wid, laid, x, y);
        let Some(menu) = self.autopilot_menu.as_mut() else {
            return false;
        };
        let Some((rect, (row, col))) = rect
            .zip(cell)
            .filter(|(rect, (r, c))| rect.contains(*r, *c))
        else {
            if moved && menu.leave_stops() {
                self.request_menu_frame(wid);
            }
            let Some(menu) = self.autopilot_menu.as_mut() else {
                return false;
            };
            menu.note_pointer((x, y), cell.map(|(r, _)| r), None);
            return false;
        };
        let hovered = hit_at(rect, &menu.rows, menu.viewport, row, col);
        menu.note_pointer((x, y), Some(row), hovered);
        // A pointer that left a slider's stops rests on none of them: the keys then start
        // from the thumb, and Return picks nothing the pointer passed over.
        if moved && !matches!(hovered, Some((_, Some(_)))) && menu.leave_stops() {
            self.request_menu_frame(wid);
        }
        let Some(menu) = self.autopilot_menu.as_mut() else {
            return false;
        };
        if let Some((i, stop)) = hovered {
            let rested = menu.rest_on_stop(i, stop);
            if rested || menu.highlight != Some(i) {
                menu.highlight = Some(i);
                self.request_menu_frame(wid);
            }
        }
        true
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
        let visible = menu.viewport.range(menu.rows.len(), shown_rows(rect));
        for (i, row) in menu
            .rows
            .iter()
            .enumerate()
            .take(visible.end)
            .skip(visible.start)
        {
            match row {
                // What the card states between its choices, in row order: a group's heading
                // before the facts under it.
                MenuRow::Note { spoken, .. }
                | MenuRow::Plot { spoken, .. }
                | MenuRow::Fact { spoken, .. } => {
                    notes.push(spoken.as_str());
                }
                MenuRow::Section(title) => notes.push(title.as_str()),
                MenuRow::Item {
                    spoken,
                    enabled,
                    act,
                    checked,
                    ..
                } => {
                    if menu.highlight == Some(i) {
                        selected = Some(items.len());
                    }
                    // A row that is one of a card's exclusive choices (the model card's two
                    // models) is a radio, checked where it is the one in use; any other row is
                    // an action.
                    let radio = matches!(act, MenuAct::Fact(_, FactAct::SetModel(..)));
                    items.push(GridMenuItem {
                        label: spoken.clone(),
                        checked: radio.then_some(*checked),
                        disabled: !enabled,
                        row: top + 1 + i - visible.start,
                        slider: None,
                    });
                }
                MenuRow::Stops {
                    name, stops, at, ..
                } => {
                    if menu.highlight == Some(i) {
                        selected = Some(items.len());
                    }
                    // Its value in words: the stop the thumb stands on, or that it stands on
                    // none, which the card's own note then says the value of.
                    let words = at.and_then(|at| stops.get(at)).map_or_else(
                        || {
                            let of = stops
                                .iter()
                                .map(|stop| stop.label.as_str())
                                .collect::<Vec<_>>()
                                .join(", ");
                            format!("none of {of}")
                        },
                        |stop| stop.label.clone(),
                    );
                    items.push(GridMenuItem {
                        label: name.clone(),
                        checked: None,
                        disabled: !row.selectable(),
                        row: top + 1 + i - visible.start,
                        slider: Some(crate::a11y_tree::GridSlider {
                            at: at.map(|at| at + 1),
                            stops: stops.len(),
                            words,
                        }),
                    });
                }
                MenuRow::Header(_) | MenuRow::Name(_) | MenuRow::Rule | MenuRow::Gap(_) => {}
            }
        }
        let mut label = menu.spoken.clone();
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
                let visible = m.viewport.range(m.rows.len(), shown_rows(rect));
                m.rows
                    .iter()
                    .enumerate()
                    .take(visible.end)
                    .skip(visible.start)
                    .filter(|(_, row)| matches!(row, MenuRow::Item { .. } | MenuRow::Stops { .. }))
                    .nth(item)
                    .map(|(i, _)| i)
            })
        else {
            return;
        };
        self.choose_autopilot_menu_row(row);
    }

    /// Move the slider published as `item` one stop for a screen reader's increment (`up`) or
    /// decrement, from where its thumb stands, and pick that stop, if `epoch` matches the
    /// painted menu. A slider whose thumb stands on no stop has nothing below its value to
    /// decrement to; its increment picks its first stop.
    #[cfg(a11y_tree)]
    pub(crate) fn step_autopilot_menu_slider_for_reader(
        &mut self,
        wid: WindowId,
        epoch: u32,
        item: usize,
        up: bool,
    ) {
        let Some(menu) = self
            .autopilot_menu
            .as_mut()
            .filter(|m| m.wid == wid && m.a11y_epoch() == epoch)
        else {
            return;
        };
        let Some(rect) = menu.rect else {
            return;
        };
        let visible = menu.viewport.range(menu.rows.len(), shown_rows(rect));
        let Some((i, to)) = menu
            .rows
            .iter()
            .enumerate()
            .take(visible.end)
            .skip(visible.start)
            .filter(|(_, row)| matches!(row, MenuRow::Item { .. } | MenuRow::Stops { .. }))
            .nth(item)
            .and_then(|(i, row)| match row {
                MenuRow::Stops { stops, at, .. } if up || at.is_some() => {
                    Some((i, stop_step(stops, *at, up)?))
                }
                _ => None,
            })
        else {
            return;
        };
        menu.highlight = Some(i);
        menu.rest_on_stop(i, Some(to));
        self.choose_autopilot_menu_row(i);
    }
}

#[cfg(a11y_tree)]
const fn shown_rows(rect: MenuRect) -> usize {
    rect.h.saturating_sub(2)
}

#[cfg(test)]
#[path = "autopilot_menu_tests.rs"]
mod tests;
