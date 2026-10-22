// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE AGENTS PANEL on the glass (`docs/DESIGN-harness-autopilot-2026-10-15.md`
//! §10.6.13). Owner, 2026-10-19: "for more bling, aterm bling the subagents
//! display at the bottom. I want to see rainbow" — "and some more informative
//! coloring about the agent status in those dots. Design something more
//! information dense. In particular, I want to know if the subagent is stuck
//! or running and how much work it is doing in some visual indication of
//! brightness and glow and maybe saturation".
//!
//! While the harness's theme is on a Claude Code pane — the composer border's
//! own predicate (`crate::autopilot::harness_theme`: Cruise's spectrum,
//! Land's sunset, Off's moonbow, the unhosted window's word) — the agents
//! panel Claude draws under its footer (`  ⏺ main`, `  ◯ general-purpose …
//! 21s · ↓ 6.2k tokens`, `  ◯ sweep  ▰▰▰▰▱▱  3/4 · …`) is RE-INKED (fg only)
//! and LIT:
//!
//! * THE RAINBOW. Its rows take the palette's hues top down (the main row
//!   the first stripe, the task rows the palette's dot hues: every stripe but
//!   those near Claude's own success green and error red — counted from task
//!   row 1 where Claude draws no main row, as for a workflow alone): the name of the
//!   main row and of every agent or workflow not done runs along the palette
//!   from its row's hue, at the lightness Claude drew it in; a workflow's bar
//!   runs the whole palette, its filled cells vivid and — while an agent
//!   runs, the window is focused and motion allowed — crested by Claude's
//!   mascot ripple; the `❯` of a selected row in its row's hue. The activity
//!   text, the elapsed time, the tokens, `↓ N more` and the idle summary keep
//!   Claude's ink.
//! * THE DOTS. Each agent's or workflow's bullet says what the harness's fold
//!   of the panel says of it (`aterm_agent::harness::agents`): HUE = which
//!   agent (its row's hue, never done's or failed's), SATURATION = freshness
//!   (quiet visibly paler), BRIGHTNESS and GLOW = work rate (on paper a
//!   filled hollow and a round tint); in its tool (quiet, but its own call
//!   in flight inside that call's timeout plus 30 s) a fresh ring in its
//!   full hue with a dimmed light held inside the ring (no glow past it; on
//!   paper its hollow half lit), still; stuck (no
//!   work for 150 s, nothing of its own in flight) is a grey breath,
//!   awaiting approval a still amber ring, done or failed Claude's own green
//!   or red. The look is `aterm_effects::harness_panel`'s, pure; this module
//!   places it.
//! * ITS OWN CALLS. Which row is which agent's file the session's flight
//!   table says (`aterm_agent::harness::flight`, folded with the usage the
//!   resolver already reads and published on the footer's side,
//!   `claude_footer::FactSide::flights_at`): each reading keys its rows by
//!   their cells ([`PanelRead`]'s `RowKey`s) and `flight::place` maps a row
//!   only to the one file that agrees exactly (name, activity, start), at
//!   the wall clock, when the reading or the table moves. A row no file is
//!   certainly its keeps the screen's rules exactly. With Claude's own `❯`
//!   on a row in its tool ([`pointed_wait`]) the footer's title slot says
//!   what it waits on (`in Bash 4m12s`, `crate::claude_lights`).
//!
//! * WHAT IS READ. The panel is found by its ONE locator
//!   (`aterm_agent::harness::footer::agents_panel_under`) under the composer's
//!   bottom rule the footer's reading already holds, at most once per rebuild
//!   of the pane's text (`claude_footer::PaneTextCache::agents_panel`), with
//!   what Claude drew each part in ([`read_panel`]): a part is inked only
//!   where every cell of it is Claude's PLAIN text — its default ink, a
//!   neutral grey or a dim of the default (the 2.1.304 panel grey is an
//!   explicit `#999999`, never SGR 2), on the pane's own ground, no
//!   underline, no wide glyph, no cluster, mark or image — so a coloured, a
//!   linked or a highlighted cell keeps Claude's colour; a bullet in one of
//!   Claude's status colours is a done, failed or killed agent
//!   (`Tint::Marked`). An equal reading is the last one, shared, and folds
//!   nothing.
//! * GATES. The footer's own (`tab_status` on, a live viewport, the facts
//!   shown), Claude Code the pane's program, `[claude_footer] agents` on
//!   (absent is on, `Config::claude_footer_agents`), and the theme on. Per
//!   frame: no selection in the window (its highlight is the grid's), and a
//!   row under the cursor, one the chrome owns or one BiDi reordered keeps
//!   Claude's look.
//! * GLASS ONLY. The ink goes to `RenderInput::ink` (on cells no other
//!   producer inked), the dots' light to `glow_halo` over the bullet (a dark
//!   ground's additive halos, a light ground's round source-over veils),
//!   recorded with the card's (`claude_card::paint_panel`) and taken back by
//!   `claude_card::strip` at the footer's undo points. No grid cell,
//!   `text`/`screen` read or recording changes.
//! * AT REST, NOTHING. The fold runs when the reading moves (Claude repaints
//!   the elapsed time every second while an agent runs), when the flight
//!   table is published anew (a subagent's file moved), or when a dot's next
//!   turn comes due with nothing new on the screen ([`PanelEntry::due`]);
//!   the look is made anew only when what it is made of moves
//!   ([`LookKey`]), else the kept look is put on the glass as it is — no
//!   allocation once its buffers are grown, and no clock read for a panel
//!   with nothing running, breathing or due.
//! * MOTION. The bar's crest steps every `ripple::FLOW_STEP_MS` on ONE grid
//!   ([`PanelGrid`], the one phase source): a card's that flows on the same
//!   window — in phase with its ripple, each step's frame asked with the
//!   card's own — else the window's own clock, while a row runs and the
//!   window is focused, motion allowed and serious mode off; a stuck dot
//!   breathes on the same steps (`harness_panel::BREATH_STEP_MS` is the
//!   flow's, its course started on the grid and counted on its steps, so a
//!   present in a step's slack draws the crest, the breath and the card's
//!   step at once) for at most
//!   `harness_panel::BREATH_FOR_MS`, then rests until another row turns
//!   stuck (the set of stuck rows gains one: a new course) or a person
//!   returns to the window (a focus regain after `claude_card::RETURN_MS`
//!   away, [`App::rearm_claude_panels`]). Each step is a frame asked for on
//!   the loop's wake (`metrics::DeadlineOwner::AgentsPanel`), judged every
//!   wake like the card's ([`App::claude_panels_due`]), and keyed in the
//!   footer's repaint key ([`App::claude_panel_term`]). Serious mode keeps
//!   the state ink and drops every light and breath.

use std::sync::Arc;
use std::time::{Duration, Instant};

use aterm_agent::harness::agents::{DotState, PanelTrack, Tint};
use aterm_agent::harness::flight::{FlightTable, RowKey};
use aterm_core::render::{InkCell, RainHalo, RenderInput};
use aterm_core::terminal::{RenderCell, Script, UnderlineStyle};
use aterm_effects::harness_card::{CardPalette, FLOW_SLACK_MS};
use aterm_effects::harness_panel::{self as glass, DotMood, DotPlace, PanelMark};
use aterm_effects::ripple::{FLOW_STEP_MS, GLINT_PERIOD_MS};
use aterm_phase::agents_panel::{AgentsPanel, PANEL_ROWS_MAX, RowKind, Span};

use crate::claude_card::{PaneGeom, card_now};
use crate::{App, WindowId};

/// What Claude drew one panel row's parts in: each `Some(fg)` where every
/// cell of the part is plain ([`plain_cell`]) and of that one ink, else
/// `None` (Claude's look stands there).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct RowInk {
    pointer: Option<[u8; 3]>,
    bullet: Option<[u8; 3]>,
    name: Option<[u8; 3]>,
    /// The bar's filled cells and its empty cells.
    fill: Option<[u8; 3]>,
    empty: Option<[u8; 3]>,
    /// BiDi reordered the row: its glass columns are not its cells'.
    bidi: bool,
}

/// One reading of a pane's agents panel ([`read_panel`]): the rows as
/// Claude drew them and what it drew each part in. Fixed arrays, compared by
/// value: an equal reading is the last one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PanelRead {
    panel: AgentsPanel,
    ink: [RowInk; PANEL_ROWS_MAX],
    /// What the fold is told of each row's bullet colour.
    tints: [Tint; PANEL_ROWS_MAX],
    /// What each task row is matched to its agent's file by
    /// (`aterm_agent::harness::flight::place`), read off its text.
    keys: [Option<RowKey>; PANEL_ROWS_MAX],
}

/// Read the agents panel under the composer bottom rule `bottom` of pane
/// `pane`'s `rows` (its text, one char a cell), and what Claude drew its
/// parts in off `scratch` (the frame those rows were read from); `blank` is
/// the pane's blank cell. `None` where no panel hangs there, or where the
/// locator panicked (behind the reader's fence).
pub(crate) fn read_panel(
    session: u64,
    rows: &[String],
    bottom: usize,
    scratch: &RenderInput,
    pane: PaneGeom,
    blank: RenderCell,
) -> Option<PanelRead> {
    #[cfg(test)]
    crate::work_counts::panel_read();
    let panel =
        crate::reader_guard::read_or_none(session, "Claude Code agents panel", rows, || {
            aterm_agent::harness::footer::agents_panel_under(rows, bottom)
        })
        .flatten()?;
    let mut read = PanelRead {
        panel,
        ink: [RowInk::default(); PANEL_ROWS_MAX],
        tints: [Tint::Plain; PANEL_ROWS_MAX],
        keys: [None; PANEL_ROWS_MAX],
    };
    for (i, row) in panel.rows().iter().enumerate() {
        let frame_row = pane.strip + pane.row_off + usize::from(row.row);
        let cells = scratch.cells.get(frame_row).map_or(&[][..], Vec::as_slice);
        let part = |span: Span| part_ink(scratch, cells, frame_row, pane, span, blank);
        read.keys[i] = rows
            .get(usize::from(row.row))
            .and_then(|text| RowKey::of(text, row));
        read.tints[i] = match row
            .bullet
            .and_then(|(span, _)| cells.get(pane.col_off + usize::from(span.start)))
        {
            Some(cell) if !plain_fg(cell.fg, blank) => Tint::Marked,
            _ => Tint::Plain,
        };
        let (fill, empty) = row.bar.map_or((None, None), |bar| {
            let mid = bar
                .span
                .start
                .saturating_add(u16::from(bar.filled))
                .min(bar.span.end);
            (
                part(Span {
                    start: bar.span.start,
                    end: mid,
                }),
                part(Span {
                    start: mid,
                    end: bar.span.end,
                }),
            )
        });
        read.ink[i] = RowInk {
            pointer: row.pointer.and_then(part),
            bullet: row.bullet.and_then(|(span, _)| part(span)),
            name: part(row.name),
            fill,
            empty,
            bidi: !scratch.visual_to_logical_row(frame_row).is_empty(),
        };
    }
    Some(read)
}

/// The one ink of every cell of `span` (pane columns) on frame row
/// `frame_row`, where each is plain ([`plain_cell`]) and none holds a
/// cluster, a combining mark or an image; else `None`. An empty span, or one
/// past the pane, is `None`.
fn part_ink(
    scratch: &RenderInput,
    cells: &[RenderCell],
    frame_row: usize,
    pane: PaneGeom,
    span: Span,
    blank: RenderCell,
) -> Option<[u8; 3]> {
    if span.is_empty() || usize::from(span.end) > pane.cols {
        return None;
    }
    let cols = pane.col_off + usize::from(span.start)..pane.col_off + usize::from(span.end);
    let part = cells.get(cols.clone())?;
    if touches(&scratch.clusters, frame_row, &cols)
        || touches(&scratch.combining, frame_row, &cols)
        || touches(&scratch.images, frame_row, &cols)
    {
        return None;
    }
    let fg = part.first()?.fg;
    part.iter()
        .all(|c| c.fg == fg && plain_cell(c, blank))
        .then_some(fg)
}

/// Whether a column-keyed side channel holds an entry in `cols` of `row`.
fn touches<T>(channel: &[Vec<(usize, T)>], row: usize, cols: &std::ops::Range<usize>) -> bool {
    channel
        .get(row)
        .is_some_and(|entries| entries.iter().any(|(col, _)| cols.contains(col)))
}

/// Whether `cell` is Claude's plain text on the pane's ground: its ink plain
/// ([`plain_fg`]), its ground the pane's, no line of any kind, one cell
/// wide and no presentation asked, set on the line.
fn plain_cell(cell: &RenderCell, blank: RenderCell) -> bool {
    cell.bg == blank.bg
        && !cell.wide
        && !cell.emoji_presentation
        && !cell.text_presentation
        && cell.underline == UnderlineStyle::None
        && !cell.strikethrough
        && !cell.overline
        && cell.underline_color.is_none()
        && cell.overline_color.is_none()
        && cell.script == Script::Normal
        && plain_fg(cell.fg, blank)
}

/// How far, per channel, a plain ink may stray from neutral or from the
/// line between the pane's ground and its ink.
const PLAIN_SLACK: f32 = 12.0;

/// Whether `fg` is one of Claude's plain inks on a pane whose blank is
/// `blank`: a neutral grey (Claude's panel grey, `#999999` dark and
/// `#666666` light, measured on 2.1.304), or a colour on the line from the
/// pane's ground to its default ink (the default itself, a dim of it) —
/// never one of Claude's status colours (success, error, warning, its
/// blues), which lie far off both.
fn plain_fg(fg: [u8; 3], blank: RenderCell) -> bool {
    let f = fg.map(f32::from);
    let spread =
        f.iter().copied().fold(f32::MIN, f32::max) - f.iter().copied().fold(f32::MAX, f32::min);
    if spread <= 8.0 {
        return true;
    }
    let (a, b) = (blank.bg.map(f32::from), blank.fg.map(f32::from));
    let d = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
    let len2 = d.iter().map(|v| v * v).sum::<f32>();
    let t = if len2 <= f32::EPSILON {
        0.0
    } else {
        ((f[0] - a[0]) * d[0] + (f[1] - a[1]) * d[1] + (f[2] - a[2]) * d[2]) / len2
    }
    .clamp(0.0, 1.0);
    (0..3).all(|i| (f[i] - (a[i] + t * d[i])).abs() <= PLAIN_SLACK)
}

/// One Claude Code pane the footer's splice found an agents panel on, its
/// gates all holding, gathered for [`App::compose_claude_panels`].
#[derive(Clone, Debug)]
pub(crate) struct PanelLeaf {
    pub(crate) leaf_index: usize,
    pub(crate) session: u64,
    pub(crate) pane: PaneGeom,
    pub(crate) blank: RenderCell,
    pub(crate) palette: CardPalette,
    /// The pane cache's reading, shared.
    pub(crate) read: Arc<PanelRead>,
    /// The session's agents and workflow runs with their calls in flight,
    /// as its side last published them (`claude_footer::FactSide::flights`),
    /// shared.
    pub(crate) flights: Option<Arc<FlightTable>>,
    /// The rows this frame leaves as Claude drew them (bit `k`: the
    /// panel's row `k`), filled by [`App::compose_claude_panels`].
    pub(crate) skip: u32,
}

/// The window's pixel geometry for a panel's light: the cell, the left
/// padding and the y of frame row 0, window-absolute.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Px {
    cw: usize,
    ch: usize,
    pad: usize,
    grid_top: usize,
}

/// What a panel's look was made from ([`PanelEntry::key`]): the same key,
/// the same ink and light.
#[derive(Clone, Copy, Debug, PartialEq)]
struct LookKey {
    read_gen: u64,
    dots: u64,
    palette: CardPalette,
    dark: bool,
    /// The pane's ground: the paper a light ground's rings are solved on.
    ground: u32,
    serious: bool,
    crest_step: Option<u64>,
    /// The breath's step on the grid ([`PanelGrid::step`]) while it breathes.
    breath_step: Option<u64>,
    pane: PaneGeom,
    px: Px,
    /// Rows the frame leaves as Claude drew them (bit `k`: the panel's row
    /// `k`): the cursor's, the chrome's.
    skip: u32,
}

/// One session's agents panel on the window's glass: its fold, its last
/// reading and look, and its motion.
#[derive(Debug)]
struct PanelEntry {
    /// The plan leaf it was last found at: the repaint key's lookup.
    leaf_index: usize,
    session: u64,
    track: PanelTrack,
    read: Option<Arc<PanelRead>>,
    /// The flight table the fold last placed its rows by.
    flights: Option<Arc<FlightTable>>,
    /// Moved with every new reading: what the look is keyed by.
    read_gen: u64,
    /// The fold's next turn, in ms on the window's clock, as of the last
    /// fold or retime: the wake that judges it retimes the dots
    /// ([`App::claude_panels_due`]), and a frame is asked only where their
    /// look moved.
    due: Option<u64>,
    /// When the panel's stuck dots began to breathe (ms), while one shows.
    breath_since: Option<u64>,
    /// The breath ran its course: the stuck dots rest until a re-arm.
    breath_rested: bool,
    /// The panel's rows whose dot was stuck at the last frame (bit `k`: the
    /// panel's row `k`): a row joining them starts a new course.
    stuck_rows: u32,
    /// At the last frame: the bar crested, a stuck dot breathed.
    crest: bool,
    breathing: bool,
    /// Drawn by the latest splice of this window.
    shown: bool,
    /// Its next frame was asked for (`App::claude_panels_due`); no further
    /// wake is armed for it until a frame draws it.
    asked: bool,
    key: Option<LookKey>,
    ink: Vec<InkCell>,
    halo: Vec<RainHalo>,
}

impl PanelEntry {
    fn new(leaf_index: usize, session: u64) -> PanelEntry {
        PanelEntry {
            leaf_index,
            session,
            track: PanelTrack::default(),
            read: None,
            flights: None,
            read_gen: 0,
            due: None,
            breath_since: None,
            breath_rested: false,
            stuck_rows: 0,
            crest: false,
            breathing: false,
            shown: false,
            asked: false,
            key: None,
            ink: Vec::new(),
            halo: Vec::new(),
        }
    }

    /// Whether this panel on show may want a frame with nothing new on the
    /// screen — known without a clock: its bar crests, a stuck dot
    /// breathes, or a dot's next turn is due some time.
    fn live(&self) -> bool {
        self.shown && (self.crest || self.breathing || self.due.is_some())
    }

    /// The frame it wants at `now` (ms on the window's clock, `None`: not
    /// read, the panel not live), as the repaint key's term: the crest's
    /// step and the breath's while they move, and the dots' look as the
    /// last fold or retime left it — so a turn come due that moves no dot's
    /// look (a fade's tick under its next 1/32 step) moves no key.
    fn wanted(&self, now: Option<u64>, grid: PanelGrid) -> u64 {
        let mut h = mix(self.track.fingerprint()).rotate_left(42);
        if let Some(now) = now {
            if self.crest {
                h ^= mix(1 + grid.step(now));
            }
            if self.breathing
                && let Some(since) = self.breath_since
                && now.saturating_sub(since) < glass::BREATH_FOR_MS
            {
                h ^= mix(1 + grid.step(now)).rotate_left(21);
            }
        }
        h
    }

    /// When (ms) it next wants a frame, from the frame it last drew: the
    /// crest's next step on `grid` (its wake, [`PanelGrid::wake`]), the
    /// breath's next step (its rest at the end), the fold's next turn, and
    /// now where a retime since moved the dots' look. `None` once asked or
    /// with nothing to come.
    fn next(&self, grid: PanelGrid) -> Option<u64> {
        if !self.shown || self.asked {
            return None;
        }
        let key = self.key.as_ref();
        let crest = self
            .crest
            .then(|| key.and_then(|k| k.crest_step))
            .flatten()
            .map(|step| grid.wake(step + 1));
        // A breath re-armed since the last frame wants its first step now.
        let breath = self
            .breathing
            .then(|| {
                let since = self.breath_since?;
                Some(match key.and_then(|k| k.breath_step) {
                    Some(step) => grid.wake(step + 1).min(since + glass::BREATH_FOR_MS),
                    None => since,
                })
            })
            .flatten();
        let moved = key
            .is_some_and(|k| k.dots != self.track.fingerprint())
            .then_some(0);
        [crest, breath, self.due, moved].into_iter().flatten().min()
    }
}

/// The agents panels on one window's glass (kept in `claude_card::WindowCards`,
/// beside the cards whose record their light joins).
#[derive(Debug, Default)]
pub(crate) struct WindowPanels {
    entries: Vec<PanelEntry>,
    /// The window's panel clock's zero: every ms here is since it.
    epoch: Option<Instant>,
    /// Where the motion's steps fall on that clock, as the last frame found
    /// it ([`PanelGrid::of`]).
    grid: PanelGrid,
}

/// THE PANELS' STEP GRID, the one phase source of their motion: the ripple
/// phase the crest is drawn at and the slack each step's frame is asked
/// after the step's start, on the window's panel clock. With a card flowing
/// on the window (`claude_card::WindowCards::flow_origin`) they are the
/// card's — its ring births, and its steps' wakes `FLOW_SLACK_MS` past each
/// start — so the crest runs in phase with the card's ripple and a window
/// that shows both draws ONE frame a step; with none, the window's own
/// clock (`0`, `0`). A stuck dot's breath starts its course on the grid
/// ([`Self::floor`]) and steps with it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct PanelGrid {
    /// The ripple's ring birth on the window's clock, mod `GLINT_PERIOD_MS`.
    phase: u64,
    /// How long after a step's start its frame is asked.
    slack: u64,
}

impl PanelGrid {
    /// The grid of a window whose panel clock started at `epoch`, a card on
    /// it flowing on a ripple that started at `origin` (`None`: no card
    /// flows, the window's own).
    pub(crate) fn of(epoch: Instant, origin: Option<Instant>) -> Self {
        let Some(origin) = origin else {
            return Self::default();
        };
        let phase = if origin >= epoch {
            ms_since(epoch, origin) % GLINT_PERIOD_MS
        } else {
            (GLINT_PERIOD_MS - ms_since(origin, epoch) % GLINT_PERIOD_MS) % GLINT_PERIOD_MS
        };
        Self {
            phase,
            slack: FLOW_SLACK_MS,
        }
    }

    /// `t` (ms on the window's clock) on the ripple's: in phase with the
    /// card's (`crest` is periodic in `GLINT_PERIOD_MS`), a period ahead so
    /// it never underflows.
    fn ripple(self, t: u64) -> u64 {
        t + GLINT_PERIOD_MS - self.phase
    }

    /// The ripple's step `t` falls in.
    fn step(self, t: u64) -> u64 {
        self.ripple(t) / FLOW_STEP_MS
    }

    /// The crest's ms for its step at `t`: the step's start on the ripple.
    fn crest_ms(self, t: u64) -> u64 {
        self.step(t) * FLOW_STEP_MS
    }

    /// When (ms on the window's clock) step `step`'s frame is asked: its
    /// start, and the slack.
    fn wake(self, step: u64) -> u64 {
        (step * FLOW_STEP_MS + self.phase + self.slack).saturating_sub(GLINT_PERIOD_MS)
    }

    /// The latest wake at or before `t` (`t` itself before the first): where
    /// a breath's course starts, so its steps are the crest's.
    fn floor(self, t: u64) -> u64 {
        let k = (t + GLINT_PERIOD_MS).saturating_sub(self.phase + self.slack) / FLOW_STEP_MS;
        Some(self.wake(k)).filter(|&w| w <= t).unwrap_or(t)
    }
}

impl WindowPanels {
    /// No panel is on show: the splice returned before its panel pass.
    pub(crate) fn hide_all(&mut self) {
        for entry in &mut self.entries {
            entry.shown = false;
        }
    }

    /// Whether a stuck dot on show breathes, or rests from its breath: what
    /// a person's return plays again ([`App::rearm_claude_panels`]).
    pub(crate) fn any_breath(&self) -> bool {
        self.entries
            .iter()
            .any(|e| e.shown && e.breath_since.is_some())
    }

    /// Whether any panel on show may want a frame (clock-free).
    fn live(&self) -> bool {
        self.epoch.is_some() && self.entries.iter().any(PanelEntry::live)
    }

    /// `now` in ms on this window's panel clock.
    fn ms(&self, now: Instant) -> Option<u64> {
        Some(ms_since(self.epoch?, now))
    }
}

/// The wall clock in unix ms, or a test's: what the flight table's times
/// are on.
fn wall_ms() -> i64 {
    #[cfg(test)]
    if let Some(ms) = TEST_WALL.with(std::cell::Cell::get) {
        return ms;
    }
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

#[cfg(test)]
thread_local! {
    static TEST_WALL: std::cell::Cell<Option<i64>> = const { std::cell::Cell::new(None) };
}

/// Set (or, with `None`, clear) this thread's test wall clock ([`wall_ms`]).
#[cfg(test)]
pub(crate) fn set_test_wall(ms: Option<i64>) {
    TEST_WALL.with(|t| t.set(ms));
}

/// `now` in whole ms since `epoch` (`0` before it).
fn ms_since(epoch: Instant, now: Instant) -> u64 {
    u64::try_from(now.saturating_duration_since(epoch).as_millis()).unwrap_or(u64::MAX)
}

/// What the agents panel of `session` on show in `panels` says its row
/// under Claude's own `❯` (a row the person points at or selects in
/// Claude's panel) waits on, as its last fold left it: the tool and the
/// whole seconds since its call was written, while that row's dot is in its
/// tool (`DotState::Tool`). What the footer's title slot names
/// (`claude_lights`).
pub(crate) fn pointed_wait(
    panels: &WindowPanels,
    session: u64,
) -> Option<(aterm_agent::harness::flight::ToolName, u64)> {
    let entry = panels
        .entries
        .iter()
        .find(|e| e.shown && e.session == session)?;
    let read = entry.read.as_ref()?;
    let row = read
        .panel
        .rows()
        .iter()
        .find(|r| r.is_task() && r.pointer.is_some())?;
    let dot = entry.track.dots().iter().find(|d| d.row == row.row)?;
    let waits = dot.waits.filter(|_| dot.state == DotState::Tool)?;
    Some((waits.tool, waits.for_ms / 1000))
}

/// The rows of `panel` whose dot is stuck (bit `k`: the panel's row `k`).
fn stuck_rows(panel: &AgentsPanel, dots: &[aterm_agent::harness::agents::Dot]) -> u32 {
    panel
        .rows()
        .iter()
        .enumerate()
        .filter(|(_, row)| {
            dots.iter()
                .any(|d| d.row == row.row && d.state == DotState::Stuck)
        })
        .fold(0, |bits, (k, _)| bits | 1 << k)
}

/// What the fold says of a dot, as its look takes it.
fn mood_of(state: DotState) -> DotMood {
    match state {
        DotState::Running => DotMood::Running,
        DotState::Quiet => DotMood::Quiet,
        DotState::Tool => DotMood::Tool,
        DotState::Stuck => DotMood::Stuck,
        DotState::Awaiting => DotMood::Awaiting,
        DotState::Halted
        | DotState::Waiting
        | DotState::Idle
        | DotState::Paused
        | DotState::Settled => DotMood::Still,
    }
}

fn pack(rgb: [u8; 3]) -> u32 {
    (u32::from(rgb[0]) << 16) | (u32::from(rgb[1]) << 8) | u32::from(rgb[2])
}

fn unpack(rgb: u32) -> [u8; 3] {
    [(rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8]
}

fn u16_of(v: usize) -> u16 {
    u16::try_from(v).unwrap_or(u16::MAX)
}

/// SplitMix64's finalizer: a key term from plain integers.
fn mix(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}

/// What a panel's look is drawn with this frame.
struct LookIn<'a> {
    read: &'a PanelRead,
    track: &'a PanelTrack,
    palette: CardPalette,
    dark: bool,
    ground: u32,
    serious: bool,
    crest_ms: Option<u64>,
    breath: f32,
    pane: PaneGeom,
    px: Px,
    skip: u32,
}

/// Make a panel's look into `ink` (sorted, unique) and `halo`, each emptied
/// first (module header: THE RAINBOW, THE DOTS).
fn make_look(look: &LookIn<'_>, ink: &mut Vec<InkCell>, halo: &mut Vec<RainHalo>) {
    #[cfg(test)]
    crate::work_counts::panel_look_made();
    ink.clear();
    halo.clear();
    let (pane, px, palette, dark) = (look.pane, look.px, look.palette, look.dark);
    let top = pane.strip + pane.row_off;
    let clip = (
        u16_of(px.grid_top + top * px.ch),
        u16_of(px.grid_top + (top + pane.rows) * px.ch),
    );
    // The main row's stripe is the main row's alone: where Claude draws none
    // (a workflow alone, measured on 2.1.304) the first task row is still
    // task row 1, never in the main row's hue (Cruise's red, by Claude's
    // failed red).
    let no_main = !look
        .read
        .panel
        .rows()
        .first()
        .is_some_and(|r| r.kind == RowKind::Main);
    for (k, (row, cells)) in look
        .read
        .panel
        .rows()
        .iter()
        .zip(look.read.ink.iter())
        .enumerate()
    {
        if look.skip & (1 << k) != 0 || cells.bidi {
            continue;
        }
        let frame_row = top + usize::from(row.row);
        let hue_k = k + usize::from(no_main);
        let row_t = glass::row_hue(palette, hue_k, dark);
        let dot = row
            .is_task()
            .then(|| look.track.dots().iter().find(|d| d.row == row.row))
            .flatten();
        let named = match row.kind {
            RowKind::Main => Some(1.0),
            RowKind::Agent | RowKind::Workflow => dot
                .filter(|d| !matches!(d.state, DotState::Settled | DotState::Idle))
                .map(|d| d.sat),
            RowKind::IdleSummary | RowKind::More => None,
        };
        let mut put = |col: usize, rgb: u32| {
            if col < pane.cols {
                ink.push(InkCell {
                    row: u16_of(frame_row),
                    col: u16_of(pane.col_off + col),
                    color: unpack(rgb),
                });
            }
        };
        if let Some(sat) = named {
            let text =
                |mark, x, fg: [u8; 3]| glass::panel_ink(palette, mark, x, dark, pack(fg), sat, 0.0);
            if let (Some(span), Some(fg)) = (row.pointer, cells.pointer) {
                for col in span.range() {
                    put(col, text(PanelMark::Pointer, row_t, fg));
                }
            }
            if let Some(fg) = cells.name {
                let len = row.name.range().len();
                for (j, col) in row.name.range().enumerate() {
                    put(col, text(PanelMark::Name, glass::name_x(row_t, j, len), fg));
                }
            }
            if let Some(bar) = row.bar {
                let width = usize::from(bar.cells).max(bar.span.range().len());
                for (i, col) in bar.span.range().enumerate() {
                    let x = glass::bar_x(i, width);
                    if i < usize::from(bar.filled) {
                        let Some(fg) = cells.fill else { continue };
                        let crest = look.crest_ms.map_or(0.0, |ms| glass::crest(i as f32, ms));
                        put(
                            col,
                            glass::panel_ink(
                                palette,
                                PanelMark::BarFill,
                                x,
                                dark,
                                pack(fg),
                                sat,
                                crest,
                            ),
                        );
                    } else if let Some(fg) = cells.empty {
                        put(col, text(PanelMark::BarEmpty, x, fg));
                    }
                }
            }
        }
        let (Some(dot), Some((span, _)), Some(_)) = (dot, row.bullet, cells.bullet) else {
            continue;
        };
        let dot_look = glass::dot_look(
            mood_of(dot.state),
            dot.level,
            dot.sat,
            palette,
            hue_k,
            dark,
            look.ground,
            look.breath,
            look.serious,
        );
        let col = usize::from(span.start);
        if let Some(rgb) = dot_look.ink {
            put(col, rgb);
        }
        if let Some(light) = dot_look.light
            && col < pane.cols
        {
            let place = DotPlace {
                cx: u16_of(px.pad + (pane.col_off + col) * px.cw + px.cw / 2),
                cy: u16_of(px.grid_top + frame_row * px.ch + px.ch / 2),
                cw: u16_of(px.cw),
                ch: u16_of(px.ch),
                row: u16_of(frame_row),
                clip,
            };
            glass::dot_light_quads(&light, dark, place, halo);
        }
    }
    ink.sort_unstable_by_key(|c| (c.row, c.col));
    ink.dedup_by_key(|c| (c.row, c.col));
}

/// What window `wid` draws its panels with this frame.
#[derive(Clone, Copy)]
struct Look {
    moving: bool,
    serious: bool,
    px: Px,
}

impl App {
    fn panel_look(&self, wid: WindowId, focused: bool) -> Look {
        let motion = crate::motion::MotionEffect::HarnessCard;
        let focus = self.motion_focus(wid, focused);
        let animate = self.effect_policy(motion, focus).animate(motion);
        let serious = !self
            .serious_mode_policy()
            .allows(crate::motion::SeriousEffect::HarnessCard);
        let (cw, ch) = self.win_cell_size(wid);
        Look {
            moving: animate && focus && !serious,
            serious,
            px: Px {
                cw,
                ch,
                pad: self.win_pad(wid),
                grid_top: self.win_pad_top(wid) + self.win_head(wid),
            },
        }
    }

    /// The panel pass of the footer's splice, after the card's: each
    /// gathered pane's panel folded where its reading moved or its dots came
    /// due, its look made where what it is made of moved, and put on window
    /// `wid`'s glass — recorded for `claude_card::strip`. Panels of closed
    /// sessions are forgotten; every panel not drawn this frame stops keying
    /// the repaint and arming wakes. `panels` is emptied.
    pub(crate) fn compose_claude_panels(&mut self, wid: WindowId, panels: &mut Vec<PanelLeaf>) {
        let Some(ws) = self.windows.get(&wid) else {
            panels.clear();
            return;
        };
        let focused = ws.focused;
        let look = (!panels.is_empty()).then(|| self.panel_look(wid, focused));
        // The rows each frame leaves as Claude drew them, read before the
        // window is borrowed: the cursor's and the chrome's.
        let scratch = &ws.input_scratch;
        for leaf in panels.iter_mut() {
            leaf.skip = 0;
            for (k, row) in leaf.read.panel.rows().iter().enumerate() {
                let term_row = leaf.pane.row_off + usize::from(row.row);
                let frame_row = leaf.pane.strip + term_row;
                if (scratch.cursor_visible && scratch.cursor_row == frame_row)
                    || self.chrome_owns_terminal_row(wid, term_row)
                {
                    leaf.skip |= 1 << k;
                }
            }
        }
        let pool = &self.pool;
        let Some(ws) = self.windows.get_mut(&wid).map(Box::as_mut) else {
            panels.clear();
            return;
        };
        let crate::WindowState {
            claude_card: cards,
            input_scratch: scratch,
            ..
        } = ws;
        let selection = scratch.any_selection();
        let mut clock: Option<Instant> = None;
        let mut now = || *clock.get_or_insert_with(card_now);
        // The ripple of a card flowing on this window, for the crest to step
        // on ([`PanelGrid`]): read only for a panel that may move, and the
        // clock only for a card on show that is not at rest.
        let origin = look
            .filter(|l| l.moving)
            .and_then(|_| cards.flow_origin(&mut now));
        let (state, painted) = cards.panels_and_painted();
        state.entries.retain(|e| pool.get(e.session).is_some());
        state.hide_all();
        for leaf in panels.drain(..) {
            let Some(look) = look else { continue };
            let epoch = *state.epoch.get_or_insert_with(&mut now);
            state.grid = PanelGrid::of(epoch, origin);
            let grid = state.grid;
            let at = match state.entries.iter().position(|e| e.session == leaf.session) {
                Some(at) => at,
                None => {
                    state
                        .entries
                        .push(PanelEntry::new(leaf.leaf_index, leaf.session));
                    state.entries.len() - 1
                }
            };
            let entry = &mut state.entries[at];
            entry.leaf_index = leaf.leaf_index;
            // The fold: a reading that moved, the session's flight table
            // that moved (its rows placed anew on their files, at the wall
            // clock, read only while there is a table), or a dot's turn come
            // due.
            let read_moved = !entry
                .read
                .as_ref()
                .is_some_and(|r| Arc::ptr_eq(r, &leaf.read));
            let flights_moved = match (&entry.flights, &leaf.flights) {
                (Some(a), Some(b)) => !Arc::ptr_eq(a, b),
                (None, None) => false,
                _ => true,
            };
            if read_moved || flights_moved {
                let t = ms_since(epoch, now());
                #[cfg(test)]
                crate::work_counts::panel_folded();
                let placed = leaf.flights.as_deref().map(|table| {
                    aterm_agent::harness::flight::place(&leaf.read.keys, table, wall_ms())
                });
                let flights = placed.as_ref().map_or(&[][..], |f| &f[..]);
                if read_moved {
                    entry
                        .track
                        .fold_flown(&leaf.read.panel, &leaf.read.tints, flights, t);
                    entry.read = Some(Arc::clone(&leaf.read));
                    entry.read_gen += 1;
                } else {
                    entry.track.fly(flights, t);
                }
                entry.flights = leaf.flights.clone();
                entry.due = entry.track.due();
            } else if let Some(due) = entry.due {
                let t = ms_since(epoch, now());
                if due <= t {
                    #[cfg(test)]
                    crate::work_counts::panel_folded();
                    entry.track.retime(t);
                    entry.due = entry.track.due();
                }
            }
            if selection {
                continue;
            }
            // The motion: the crest while a row runs and a lit bar has a
            // filled cell, the breath while a stuck dot shows (for its
            // course).
            let dots = entry.track.dots();
            let running = dots.iter().any(|d| d.state == DotState::Running);
            let stuck_rows = stuck_rows(&leaf.read.panel, dots);
            let bar = leaf
                .read
                .panel
                .rows()
                .iter()
                .zip(leaf.read.ink.iter())
                .any(|(r, cells)| {
                    r.bar.is_some_and(|b| b.filled > 0)
                        && cells.fill.is_some()
                        && dots.iter().any(|d| {
                            d.row == r.row && !matches!(d.state, DotState::Settled | DotState::Idle)
                        })
                });
            entry.crest = look.moving && running && bar;
            // A breath's course per stuck episode: it starts when a row first
            // turns stuck, and again when another joins the stuck rows (a
            // second agent's stall shows even after the first's course ran
            // out); a row leaving them starts none.
            if stuck_rows == 0 {
                entry.breath_since = None;
                entry.breath_rested = false;
            } else if entry.breath_since.is_none() || stuck_rows & !entry.stuck_rows != 0 {
                entry.breath_since = Some(grid.floor(ms_since(epoch, now())));
                entry.breath_rested = false;
            }
            entry.stuck_rows = stuck_rows;
            let crest_ms = entry.crest.then(|| grid.crest_ms(ms_since(epoch, now())));
            let mut breath = glass::BREATH_REST;
            let mut breath_step = None;
            entry.breathing = false;
            if look.moving
                && !entry.breath_rested
                && let Some(since) = entry.breath_since
            {
                let now_ms = ms_since(epoch, now());
                if now_ms.saturating_sub(since) < glass::BREATH_FOR_MS {
                    // Counted on the grid's steps, so the breath's step
                    // changes at a step's start as the crest's and the
                    // card's do: a present in the step's slack (Claude's
                    // repaint, where the card's lock puts it) draws it, and
                    // the step's own wake finds it drawn.
                    let step = grid.step(now_ms);
                    let steps = step.saturating_sub(grid.step(since));
                    breath = glass::breath(Some(steps * glass::BREATH_STEP_MS));
                    breath_step = Some(step);
                    entry.breathing = true;
                } else {
                    entry.breath_rested = true;
                }
            }
            let dark = crate::claude_card::dark_ground(leaf.blank);
            let ground = pack(leaf.blank.bg);
            let key = LookKey {
                read_gen: entry.read_gen,
                dots: entry.track.fingerprint(),
                palette: leaf.palette,
                dark,
                ground,
                serious: look.serious,
                crest_step: crest_ms.map(|ms| ms / FLOW_STEP_MS),
                breath_step,
                pane: leaf.pane,
                px: look.px,
                skip: leaf.skip,
            };
            if entry.key != Some(key) {
                make_look(
                    &LookIn {
                        read: &leaf.read,
                        track: &entry.track,
                        palette: leaf.palette,
                        dark,
                        ground,
                        serious: look.serious,
                        crest_ms,
                        breath,
                        pane: leaf.pane,
                        px: look.px,
                        skip: key.skip,
                    },
                    &mut entry.ink,
                    &mut entry.halo,
                );
                entry.key = Some(key);
            }
            crate::claude_card::paint_panel(painted, scratch, &entry.ink, &entry.halo, &[]);
            entry.shown = true;
            entry.asked = false;
        }
    }

    /// The agents panel's repaint-key term for one Claude Code leaf (plan
    /// leaf `index` of window `ws`, showing `session`), folded into
    /// `claude_footer_fp` beside the card's — `theme` the leaf's harness
    /// theme as the key read it: whether the panel is lit at all (the theme
    /// and `[claude_footer] agents`), serious mode where it is, and for a
    /// panel on show that may want a frame ([`PanelEntry::live`]) the frame
    /// it WANTS at the clock's now ([`PanelEntry::wanted`]: the crest's and
    /// the breath's steps, the due that has come). Reads the clock only for
    /// a live panel; takes no lock; allocates nothing.
    pub(crate) fn claude_panel_term(
        &self,
        ws: &crate::WindowState,
        index: usize,
        session: u64,
        theme: Option<CardPalette>,
    ) -> u64 {
        let on = theme.is_some() && self.config.claude_footer_agents();
        let serious = on
            && !self
                .serious_mode_policy()
                .allows(crate::motion::SeriousEffect::HarnessCard);
        let mut h = mix(u64::from(on) | u64::from(serious) << 1);
        let panels = ws.claude_card.panels();
        if on
            && let Some(entry) = panels
                .entries
                .iter()
                .find(|e| e.shown && (e.leaf_index, e.session) == (index, session))
        {
            let now = entry.live().then(|| panels.ms(card_now())).flatten();
            h ^= entry.wanted(now, panels.grid);
        }
        h
    }

    /// Whether any panel on any window's glass may want a frame — known
    /// without a clock: the wake judges read the clock only when one does.
    pub(crate) fn claude_panels_live(&self) -> bool {
        self.windows
            .values()
            .any(|ws| ws.claude_card.panels().live())
    }

    /// The panels' wake (`metrics::DeadlineOwner::AgentsPanel`): the nearest
    /// frame a panel on show wants ([`PanelEntry::next`]) on a window on
    /// screen whose tab in front shows its session, each asked once
    /// ([`Self::claude_panels_due`]); `None` with nothing running, breathing
    /// or due.
    pub(crate) fn claude_panel_deadline(&self) -> Option<Instant> {
        self.windows
            .values()
            .filter_map(|ws| {
                let panels = ws.claude_card.panels();
                let epoch = panels.epoch?;
                let soonest = panels
                    .entries
                    .iter()
                    .filter(|e| crate::active_tab_displays_session(ws, &self.view_store, e.session))
                    .filter_map(|e| e.next(panels.grid))
                    .min()?;
                crate::messages_host::band_on_screen(ws)
                    .then(|| epoch + Duration::from_millis(soonest))
            })
            .min()
    }

    /// [`Self::claude_panel_deadline`] for the loop's wake, judged only while
    /// a panel may want a frame ([`Self::claude_panels_live`]).
    pub(crate) fn claude_panel_wake(&self) -> Option<Instant> {
        if !self.claude_panels_live() {
            return None;
        }
        self.claude_panel_deadline()
    }

    /// The windows on screen whose panel's frame is due at `now`, each
    /// frame asked for once: one that never comes arms nothing more. Their
    /// repaint key's panel term has moved, so the redraw goes through.
    /// Allocates nothing when nothing is due.
    pub(crate) fn claude_panels_due(&mut self, now: Instant) -> Vec<WindowId> {
        let views = &self.view_store;
        let mut due = Vec::new();
        for (wid, ws) in &mut self.windows {
            let Some(t) = ws.claude_card.panels().ms(now) else {
                continue;
            };
            // A dot's turn come due with nothing new on the screen is taken
            // here: the dots retimed, and a frame asked below only where
            // their look moved ([`PanelEntry::next`]) — a fade's tick that
            // moves no dot a 1/32 step presents nothing.
            for e in &mut ws.claude_card.panels_mut().entries {
                if e.shown && !e.asked && e.due.is_some_and(|d| d <= t) {
                    #[cfg(test)]
                    crate::work_counts::panel_folded();
                    e.track.retime(t);
                    e.due = e.track.due();
                }
            }
            let wants = |ws: &crate::WindowState, e: &PanelEntry| {
                e.next(ws.claude_card.panels().grid)
                    .is_some_and(|at| at <= t)
                    && crate::active_tab_displays_session(ws, views, e.session)
            };
            if !ws.claude_card.panels().entries.iter().any(|e| wants(ws, e))
                || !crate::messages_host::band_on_screen(ws)
            {
                continue;
            }
            for at in 0..ws.claude_card.panels().entries.len() {
                if wants(ws, &ws.claude_card.panels().entries[at]) {
                    ws.claude_card.panels_mut().entries[at].asked = true;
                }
            }
            due.push(*wid);
        }
        due
    }

    /// [`Self::claude_panels_due`] for the loop's wake, on the card's clock,
    /// read only while a panel may want a frame.
    pub(crate) fn claude_panel_redraws(&mut self) -> Vec<WindowId> {
        if !self.claude_panels_live() {
            return Vec::new();
        }
        self.claude_panels_due(card_now())
    }

    /// Window `wid` regained focus: every stuck dot on show there breathes
    /// its course again. A window with none reads no clock.
    pub(crate) fn rearm_claude_panels(&mut self, wid: WindowId, now: impl FnOnce() -> Instant) {
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        let panels = ws.claude_card.panels_mut();
        if !panels.any_breath() {
            return;
        }
        let Some(t) = panels.ms(now()) else {
            return;
        };
        let t = panels.grid.floor(t);
        for entry in panels
            .entries
            .iter_mut()
            .filter(|e| e.shown && e.breath_since.is_some())
        {
            entry.breath_since = Some(t);
            entry.breath_rested = false;
            entry.breathing = true;
            entry.key = None;
            entry.asked = false;
        }
    }
}

#[cfg(test)]
#[path = "claude_panel_tests.rs"]
mod tests;
