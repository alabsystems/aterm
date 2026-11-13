// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Line E, the quota row (`docs/DESIGN-harness-glass-rainbow-2026-11-05.md` §1.3, §5.2, §5.3):
//! the row Claude Code draws under the composer's bottom rule from aterm's usage tap, which the
//! glass takes over with the pane's quota view ([`QuotaView::lay`]) wherever it is the tap's own.
//!
//! Each frame [`PaneFooter::claim_quota_row`] claims line E when three things hold: the row
//! directly under the bottom rule reads as the tap's words from column 2 ([`tap::parse_line`]);
//! the pane's newest tap reading says that row is the tap's own (read on the resolver thread,
//! [`aterm_agent::harness::profile::SubscriptionDisplay::tap_own`], so no file is read here);
//! and the cells are plain enough to rewrite ([`super::rewritable`]). The glass runs from column
//! 2 to two cells before Claude's own notice on the row, or to `cols − 2`, never ending before
//! the tap's words do; those words are blanked under it for painting only, so the grid, a copy
//! and a recording keep them.
//!
//! While the row is claimed, line D carries no account unit ([`FooterFacts::line_e`]); it takes
//! the words-only unit back [`GRACE`] after the session's first frame in the window, or after the
//! first frame that missed the claim, when no frame since has claimed the row ([`Grace::note`]),
//! a wake armed for the deadline. The grace is the session's, not its leaf's: a pane shown again
//! (its tab switched back to, the pane scrolled back to the bottom, a sibling split closed) is
//! not at its first frame. The row's cells are in the glass's tokens by role, its chart's
//! letters in the hue of their column, its local midnights ticks of one device pixel at the
//! foot of the cells that draw a level ([`push_ticks`]); its lead, chart and tail are each a
//! hit of [`Fact::Account`], the chart's naming the column under the pointer
//! ([`RowPart::Chart`]). Lit, the lead and the tail wear the glass's one hover (§1.5) from their
//! first glyph to their last, which is each one's hit, and the chart keeps its inks.

use std::ops::Range;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use aterm_agent::harness::outlook::Offsets;
use aterm_agent::harness::quota_view::{CELL_PLACES, QuotaLine, QuotaView, Role};
use aterm_agent::harness::tap;
use aterm_core::render::{FxBlend, FxContent, FxInk, FxItem, FxLayer, FxRect, FxSlot, FxSpace};
use aterm_core::terminal::{RenderCell, Script, UnderlineStyle};
use aterm_effects::glass_inks::{GlassInks, HoverPart, field};

#[cfg(doc)]
use super::FooterFacts;
use super::{PaneFooter, PaneTextRegion, RenderInput, layer_id, rewritable, rgb_of};
#[cfg(doc)]
use crate::claude_lights::Fact;
use crate::claude_lights::RowPart;

/// Claude Code's indent before the statusLine's words: the glass starts at this pane column.
pub(super) const INDENT: usize = 2;

/// The cells Claude keeps blank at the row's end, and before its own notice.
const MARGIN: usize = 2;

/// How long a pane may go without a claim before line D carries the account unit again: the
/// tap's refresh ([`tap::TAP_REFRESH_S`]), from the session's first frame or the first frame that
/// missed the claim.
pub(super) const GRACE: Duration = Duration::from_secs(tap::TAP_REFRESH_S as u64);

#[cfg(test)]
thread_local! {
    /// Whether a session's grace starts at its first frame on this thread ([`time_the_grace`]).
    /// Tests of everything but the grace see a pane whose grace was spent before its first frame,
    /// so line D carries the unit from the first frame wherever line E is not claimed.
    static GRACE_TIMED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Time the grace from each session's first frame on this thread, as a running window does.
#[cfg(test)]
pub(crate) fn time_the_grace() {
    GRACE_TIMED.set(true);
}

/// The instant a session's grace counts from at its first frame at `now`.
fn first_frame(now: SystemTime) -> SystemTime {
    #[cfg(test)]
    if !GRACE_TIMED.get() {
        return now.checked_sub(GRACE).unwrap_or(now);
    }
    now
}

/// Where line E's glass may lie on the row under a pane's bottom rule ([`row_span`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct RowSpan {
    /// The tap's words' cells, from [`INDENT`].
    pub(super) words: usize,
    /// The cells the glass may take from [`INDENT`]: to two cells before Claude's own notice, or
    /// to `cols − 2`, never fewer than the words.
    pub(super) room: usize,
}

/// The span of line E's glass on `row`, a pane `cols` wide, where it reads as the tap's words
/// from [`INDENT`] ([`tap::parse_line`]); `None` for any other row, an owner's, one Claude cut
/// with `…`, or one not indented as Claude indents the statusLine.
pub(super) fn row_span(row: &str, cols: usize) -> Option<RowSpan> {
    let words = tap::parse_line(row.strip_prefix("  ")?)?;
    let after = INDENT + words;
    let notice = row
        .chars()
        .enumerate()
        .skip(after)
        .find(|(_, ch)| *ch != ' ')
        .map(|(at, _)| at);
    let end = notice.unwrap_or(cols).saturating_sub(MARGIN).max(after);
    Some(RowSpan {
        words,
        room: end - INDENT,
    })
}

/// A frame's claim on line E ([`PaneFooter::claim_quota_row`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Claim {
    /// Line E is Claude's to draw: not the tap's own row, or no rung of the view fits it.
    Not,
    /// Claimed, but its cells are not plain this frame (a selection, the cursor's row): the tap's
    /// words show, and the frame counts as claimed for the grace.
    Held,
    /// Claimed and laid on `.0`.
    Laid(RowSpan),
}

/// What the row is composed from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct QuotaKey {
    /// The view's generation ([`PaneFooter::view_gen`]) and the room it was laid in.
    pub(super) laid: (u64, usize),
    /// The cells written: the row's, then blanks over the rest of the tap's words.
    pub(super) cells: usize,
    /// The pane's width, across which each cell takes its column's hue.
    pub(super) cols: usize,
    pub(super) glass: GlassInks,
    pub(super) blank: RenderCell,
    /// The pointer or the keyboard is on the account, which lights the whole row.
    pub(super) lit: bool,
    /// The device pixels of a cell, for the midnights' ticks.
    pub(super) cell: (u16, u16),
}

/// One session's grace on line E in a window: how long line D keeps its account unit back while
/// no frame claims the row. Kept per session ([`super::PaneTextCache`]), never in a leaf's footer,
/// which is replaced whenever its leaf shows another session or skips a frame: a pane shown again
/// would be at a first frame each time, and line D would drop its unit for [`GRACE`] after every
/// tab switch.
#[derive(Debug, Default)]
pub(super) struct Grace {
    /// When the grace counts from: the session's first frame, or the first frame that missed the
    /// claim after one that made it; `None` while the row is claimed.
    since: Option<SystemTime>,
    /// A frame has claimed the row, or missed it, since the session's first.
    seen: bool,
    /// The deadline line D's unit waits for while the grace runs, a wake armed for it.
    deadline: Option<SystemTime>,
    /// The last frame claimed line E.
    claimed: bool,
}

/// One pane's line E as the frames before composed it.
#[derive(Debug, Default)]
pub(super) struct QuotaRow {
    /// What [`Self::line`] was laid from, and whether a rung fit.
    laid: Option<((u64, usize), bool)>,
    line: QuotaLine,
    key: Option<QuotaKey>,
    cells: Vec<RenderCell>,
    /// The midnights' ticks over the chart ([`push_ticks`]).
    ticks: Option<Arc<FxContent>>,
}

impl QuotaRow {
    /// Lay `view` in `room` cells, as generation `generation` of the facts gave it; whether a
    /// rung fits.
    fn lay(&mut self, generation: u64, view: &QuotaView, room: usize) -> bool {
        match self.laid {
            Some((key, fits)) if key == (generation, room) => fits,
            _ => {
                let fits = view.lay(room, &mut self.line);
                self.laid = Some(((generation, room), fits));
                fits
            }
        }
    }

    /// The row composed from `key` ([`compose_cells`]), its midnights' ticks over its chart
    /// ([`push_ticks`]) in the inks of what each stands on ([`TickInks`]: the pane's ground, or a
    /// glyph that inks the tick's foot).
    fn compose(&mut self, key: QuotaKey) {
        if self.key == Some(key) {
            return;
        }
        #[cfg(test)]
        crate::work_counts::footer_rule_composed();
        compose_cells(&self.line, &key, &mut self.cells);
        let mut ticks = Vec::new();
        let chart = self
            .line
            .cells
            .get(self.line.chart.clone())
            .unwrap_or_default();
        push_ticks(
            &mut ticks,
            &self.line.ticks,
            (0, self.line.chart.len()),
            key.cell,
            |i| chart.get(i).map_or(TickOn::Ground, |c| TickOn::of(c.0)),
        );
        let inks = TickInks::new(key.glass.track(), key.glass.key().ground);
        self.ticks = tick_content(&ticks, inks);
        self.key = Some(key);
    }

    /// The composed row's cells.
    pub(super) fn cells(&self) -> &[RenderCell] {
        &self.cells
    }
}

impl Grace {
    /// Take in this frame's claim: `None` where line E can never be claimed (agy and the other
    /// CLIs, a harness switched off), else whether it was. Says whether line D gives up its
    /// account unit, and how long to wait before a frame must see the grace lapse. `now` is read
    /// only on a frame that misses the claim, so a claimed pane at rest, and one that can never
    /// claim, read no clock.
    pub(super) fn note(
        &mut self,
        now: impl FnOnce() -> SystemTime,
        claim: Option<bool>,
    ) -> (bool, Option<Duration>) {
        self.claimed = claim == Some(true);
        match claim {
            None => {
                self.since = None;
                self.deadline = None;
                (false, None)
            }
            Some(true) => {
                self.since = None;
                self.deadline = None;
                self.seen = true;
                (true, None)
            }
            Some(false) => {
                let now = now();
                let first = !std::mem::replace(&mut self.seen, true);
                let since = *self
                    .since
                    .get_or_insert_with(|| if first { first_frame(now) } else { now });
                let deadline = since + GRACE;
                match deadline.duration_since(now) {
                    Ok(left) if !left.is_zero() => {
                        let wake = (self.deadline != Some(deadline)).then_some(left);
                        self.deadline = Some(deadline);
                        (true, wake)
                    }
                    _ => {
                        self.deadline = None;
                        (false, None)
                    }
                }
            }
        }
    }

    /// The repaint key's term at `now`: whether line E is claimed, and whether a grace still holds
    /// line D's unit back, so a frame redraws when it lapses.
    pub(super) fn term(&self, now: SystemTime) -> u64 {
        u64::from(self.claimed) | u64::from(self.deadline.is_some_and(|d| now < d)) << 1
    }
}

/// The ink a quota row's cell of `role` wears at rest at palette position `t` (§1.3).
const fn ink(glass: &GlassInks, role: Role, t: f32) -> u32 {
    match role {
        Role::Lit => glass.lit(),
        Role::Line => glass.letter(t),
        Role::Value => glass.value(),
        Role::Quiet => glass.quiet(),
        Role::Mark => glass.mark(t),
        Role::Warn => glass.warn(),
        Role::Track => glass.track(),
    }
}

/// The cells of `part` of `line` its ink lies in: from its first glyph to its last, the blanks
/// between them too and none at either end; empty for a part with no glyph. A hover underlines
/// these and they are the part's hit ([`QuotaWrite::parts`]).
fn inked(line: &QuotaLine, part: &Range<usize>) -> Range<usize> {
    let glyph = |i: &usize| line.cells.get(*i).is_some_and(|(ch, ..)| *ch != ' ');
    match (part.clone().find(glyph), part.clone().rev().find(glyph)) {
        (Some(first), Some(last)) => first..last + 1,
        _ => part.start..part.start,
    }
}

/// The lead's cells of `line` to the chart, or to the tail where no chart is drawn, and the
/// tail's from there to the line's end: the two runs of words either side of the chart.
fn words(line: &QuotaLine) -> (Range<usize>, Range<usize>) {
    let len = line.cells.len();
    let (lead_end, tail_start) = if line.chart.is_empty() {
        let at = line.tail.start.min(len);
        (at, at)
    } else {
        (line.chart.start, line.chart.end)
    };
    (0..lead_end, tail_start..len)
}

/// `line`'s cells in `key`'s tokens into `out` (cleared first), then blanks to `key.cells`: each
/// cell the ink of its role at its pane column's place in the field, bold where the line says.
/// Lit, the lead and the tail wear the glass's one hover ([`GlassInks::hovered`]) over their
/// inked cells ([`inked`]), a warning's glyphs keeping their ink; the chart keeps its inks and
/// takes no line: an underline is drawn whatever a cell's glyph, and under a one-eighth line at
/// level 2 to 8, a `╳` or the `▕` it would read as a second line at level 1.
fn compose_cells(line: &QuotaLine, key: &QuotaKey, out: &mut Vec<RenderCell>) {
    out.clear();
    let blank = RenderCell {
        script: Script::Normal,
        ..key.blank
    };
    let (lead, tail) = words(line);
    let (lead, tail) = (inked(line, &lead), inked(line, &tail));
    for (i, &(ch, role, bold)) in line.cells.iter().enumerate() {
        let t = field(INDENT + i, key.cols);
        let cell = RenderCell {
            ch,
            fg: rgb_of(ink(&key.glass, role, t)),
            bold,
            underline: UnderlineStyle::None,
            underline_color: None,
            ..blank
        };
        let part = if line.chart.contains(&i) {
            HoverPart::Chart
        } else if role == Role::Warn {
            HoverPart::Warn
        } else {
            HoverPart::Text
        };
        let hit = line.chart.contains(&i) || lead.contains(&i) || tail.contains(&i);
        out.push(if key.lit && hit {
            crate::claude_lights::hovered_cell(&key.glass, cell, part)
        } else {
            cell
        });
    }
    out.resize(key.cells.max(out.len()), blank);
}

/// What a midnight's tick stands on (§1.3, §2.2), which its ink is read against ([`TickInks`],
/// whose palette base it names, [`Self::ink`]): the row's ground (a blank cell, `┄`, the
/// projected line above the tick's foot quarter), or a glyph that inks that quarter (a measured
/// step, whose block fills it from level 2 and half of it at level 1, or the projected line at
/// level 1, `▁`, or 2, `🭻`, which lies inside it).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TickOn {
    Ground,
    Fill,
}

impl TickOn {
    /// Its ink's palette base in [`TickInks::palette`] (a rect reads the entry there and the
    /// next).
    pub(crate) const fn ink(self) -> u32 {
        match self {
            TickOn::Ground => 0,
            TickOn::Fill => 2,
        }
    }

    /// What a tick stands on in a cell that draws `glyph`: a measured step (`▁` to `█`) or the
    /// projected line at level 2 (`🭻`, level 1 being `▁`) inks the tick's foot quarter; any
    /// other glyph stands clear of it, on the ground.
    pub(crate) fn of(glyph: char) -> Self {
        if ('\u{2581}'..='\u{2588}').contains(&glyph) || glyph == '\u{1fb7b}' {
            TickOn::Fill
        } else {
            TickOn::Ground
        }
    }
}

/// The inks of a row's midnights' ticks by what each stands on, in [`TickOn`]'s order (§1.3):
/// the glass's track on the ground (Claude's rule grey floored to 3:1 there); and the ground's
/// own ink across a glyph that inks the tick's foot, a notch through a measured step or a low
/// projected line that stands from it as far as it stands from the ground (a mark is floored to
/// 3:1 on a dark ground and 4.5:1 on Day, a letter to 4.5:1, §4.1), where the track, solved on
/// the ground alone, can take a step's luminance (1.0:1 on Night's violet steps) or strike
/// through the line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TickInks(pub(crate) [u32; 2]);

impl TickInks {
    /// The inks of ticks drawn in `track` on a row of `ground` (`0x00RRGGBB` each).
    pub(crate) const fn new(track: u32, ground: u32) -> Self {
        TickInks([track, ground & 0x00FF_FFFF])
    }

    /// The layer's palette: each ink twice, as a rect reads its base entry and the next.
    pub(crate) fn palette(self) -> [FxInk; 4] {
        let [ground, fill] = self.0.map(|ink| FxInk::over(ink, 255));
        [ground, ground, fill, fill]
    }
}

/// Line E's chart's midnights' ticks (§1.3, §2.2) into `out`: one at each of `places` (from the
/// row's left edge, [`CELL_PLACES`] a cell, each in a cell that takes one, [`Chart::ticks`]) that
/// falls before the
/// row's `room`-th cell, one device pixel wide at the foot of a cell `cw` by `ch` pixels on the
/// row whose top is `top`, a quarter of the cell tall, at its exact place, in the ink of what
/// `on` says it stands on in its cell (counted from the row's left edge, [`TickInks`]). A tick,
/// not a full-height hairline: only `▕` stands a whole cell high, so now never reads as one
/// more midnight, and no tick meets a `▔` at a cell's head to read as `T`. Nothing in a cell of
/// no size.
///
/// [`Chart::ticks`]: aterm_agent::harness::quota_view::Chart::ticks
pub(crate) fn push_ticks(
    out: &mut Vec<FxRect>,
    places: &[i64],
    (top, room): (i32, usize),
    (cw, ch): (u16, u16),
    on: impl Fn(usize) -> TickOn,
) {
    if cw == 0 || ch == 0 {
        return;
    }
    let h = (ch / 4).max(1);
    let end = i64::try_from(room)
        .unwrap_or(i64::MAX)
        .saturating_mul(CELL_PLACES);
    let y = top.saturating_add(i32::from(ch - h));
    out.extend(
        places
            .iter()
            .filter(|&&place| (0..end).contains(&place))
            .filter_map(|&place| {
                let x = i32::try_from(place * i64::from(cw) / CELL_PLACES).ok()?;
                let cell = usize::try_from(place / CELL_PLACES).ok()?;
                Some(FxRect {
                    x,
                    y,
                    w: 1,
                    h,
                    ink: on(cell).ink(),
                })
            }),
    );
}

/// `ticks` ([`push_ticks`]) as a layer's content in `inks` (§1.3); `None` with no tick.
pub(crate) fn tick_content(ticks: &[FxRect], inks: TickInks) -> Option<Arc<FxContent>> {
    (!ticks.is_empty()).then(|| {
        Arc::new(FxContent::new(
            ticks.iter().map(|&r| FxItem::Rect(r)).collect(),
            inks.palette().to_vec(),
        ))
    })
}

/// The stable FX layer id of `session`'s quota row's ticks.
const fn tick_id(session: u64) -> u64 {
    layer_id(session ^ 0x7175_6f74_612d_7469)
}

/// The stable FX layer id of the frame of the card `session`'s pane has open
/// ([`crate::footer_card_plate`]).
pub(crate) const fn card_plate_id(session: u64) -> u64 {
    layer_id(session ^ 0x6361_7264_2d70_6c74)
}

/// The stable FX layer id of the wash of the card `session`'s pane has open: the pill under
/// the row the pointer or the keys rest on ([`crate::footer_card_plate::CardWash`]).
pub(crate) const fn card_wash_id(session: u64) -> u64 {
    layer_id(session ^ 0x6361_7264_2d77_7368)
}

/// One pane's quota row, as the write puts it on the glass.
#[derive(Debug)]
pub(super) struct QuotaWrite {
    pub(super) session: u64,
    pub(super) leaf_index: usize,
    pub(super) frame_row: usize,
    pub(super) term_row: usize,
    /// The frame column of the row's first cell.
    pub(super) col: usize,
    pub(super) ground: RenderCell,
    /// The row's own cells; the rest of the write blanks the tap's words.
    pub(super) line: usize,
    /// The chart's cells in the row, and its cells a day.
    pub(super) chart: Range<usize>,
    pub(super) per_day: u8,
    /// The lead's and the tail's inked cells in the row ([`inked`]), either one empty where it
    /// has no glyph.
    pub(super) lead: Range<usize>,
    pub(super) tail: Range<usize>,
    /// The midnights' ticks, placed.
    pub(super) ticks: Option<FxLayer>,
}

impl QuotaWrite {
    /// The row's hits, `(frame columns, part)`: its lead's inked cells, the chart, and its
    /// tail's inked cells; each nonempty. A hit is the cells a hover underlines and the chart
    /// between them: never a blank before the lead, after the tail, or over the tap's words.
    pub(super) fn parts(&self) -> impl Iterator<Item = ((usize, usize), RowPart)> + '_ {
        [
            (self.lead.clone(), RowPart::Lead),
            (self.chart.clone(), RowPart::Chart(self.per_day)),
            (self.tail.clone(), RowPart::Tail),
        ]
        .into_iter()
        .filter(|(cells, _)| !cells.is_empty())
        .map(|(cells, part)| ((self.col + cells.start, self.col + cells.end), part))
    }
}

/// Where a pane's line E lies, for [`PaneFooter::claim_quota_row`].
pub(super) struct RowAt<'a> {
    pub(super) scratch: &'a RenderInput,
    /// The pane's rows as text.
    pub(super) rows: &'a [String],
    pub(super) region: PaneTextRegion,
    /// The bottom rule's pane row.
    pub(super) rule: usize,
    /// Window chrome owns line E's grid row.
    pub(super) chrome_owned: bool,
}

impl PaneFooter {
    /// This frame's claim on line E at `at` (§5.2): the row under the bottom rule is the tap's
    /// words, the newest reading says the row is the tap's own, a rung of the view fits the
    /// span, and the span's cells are plain ([`Claim`]). The caller has already judged that
    /// the pane's CLI and harness switch allow a claim at all.
    pub(super) fn claim_quota_row(&mut self, at: &RowAt<'_>) -> Claim {
        let Some(row) = at.rows.get(at.rule + 1) else {
            return Claim::Not;
        };
        let Some(span) = row_span(row, at.region.cols) else {
            return Claim::Not;
        };
        if !self.shown.subscription.as_ref().is_some_and(|s| s.tap_own) {
            return Claim::Not;
        }
        let generation = self.view_gen;
        let Some(view) = self.shown.quota_view() else {
            return Claim::Not;
        };
        if !self.quota.lay(generation, view, span.room) {
            return Claim::Not;
        }
        let term_row = at.region.row_off + at.rule + 1;
        let frame_row = at.region.strip + term_row;
        let from = at.region.col_off + INDENT;
        let written = from..from + self.quota.line.cells.len().max(span.words);
        if rewritable(at.scratch, frame_row, &written, at.chrome_owned) {
            Claim::Laid(span)
        } else {
            Claim::Held
        }
    }

    /// Whether line E is the glass's ([`FooterFacts::line_e`]): line D then carries no account
    /// unit. A change recomposes the rule.
    pub(super) fn note_line_e(&mut self, on: bool) {
        if self.line_e != on {
            self.line_e = on;
            self.shown.line_e = on;
            self.shown_gen += 1;
        }
    }

    /// The quota row laid on `span` composed in `glass` on `blank`'s pane, lit or not, its
    /// ticks placed for `session`'s pane `pane` at frame row `frame_row`: the write
    /// [`QuotaWrite`] puts on the glass, its cells kept here ([`QuotaRow::cells`]).
    #[allow(clippy::too_many_arguments)]
    pub(super) fn compose_quota_row(
        &mut self,
        session: u64,
        leaf_index: usize,
        span: RowSpan,
        (region, frame_row): (PaneTextRegion, usize),
        (glass, blank, lit): (GlassInks, RenderCell, bool),
        cell: (u16, u16),
        clip: aterm_core::render::PxRect,
    ) -> Option<QuotaWrite> {
        self.shown.quota_view()?;
        let line = &self.quota.line;
        let key = QuotaKey {
            laid: (self.view_gen, span.room),
            cells: line.cells.len().max(span.words),
            cols: region.cols,
            glass,
            blank,
            lit,
            cell,
        };
        let (lead, tail) = words(line);
        let (len, chart, per_day, lead, tail) = (
            line.cells.len(),
            line.chart.clone(),
            line.per_day,
            inked(line, &lead),
            inked(line, &tail),
        );
        self.quota.compose(key);
        let col = region.col_off + INDENT;
        let px = |cells: usize, side: u16| {
            i32::try_from(cells)
                .ok()
                .and_then(|c| c.checked_mul(i32::from(side)))
        };
        let ticks = self.quota.ticks.clone().and_then(|content| {
            let mut layer = FxLayer::new(
                tick_id(session),
                content,
                FxSlot::Over,
                FxSpace::Grid,
                FxBlend::Exact,
            );
            layer.offset = (px(col + chart.start, cell.0)?, px(frame_row, cell.1)?);
            layer.clip = clip;
            Some(layer)
        });
        Some(QuotaWrite {
            session,
            leaf_index,
            frame_row,
            term_row: frame_row - region.strip,
            col,
            ground: blank,
            line: len,
            chart,
            per_day,
            lead,
            tail,
            ticks,
        })
    }
}

impl crate::App {
    /// What the pointer on cell `cell` of `session`'s quota row's chart, laid at `per_day` cells
    /// a day, names (§2.6, [`QuotaView::column_title`]) on this Mac's local clock; `None` for the
    /// now cell, a cell past the chart, or a clock that does not read, where the row's own title
    /// stands.
    pub(crate) fn quota_column_title(
        &self,
        session: u64,
        per_day: u8,
        cell: usize,
    ) -> Option<String> {
        let (facts, _) = self.shown_facts(session)?;
        let view = facts.quota_view()?;
        let offsets = Offsets::read_around(view.chart.now, crate::presence::local_offset_at)?;
        view.column_title(per_day, cell, &offsets)
    }
}

#[cfg(test)]
#[path = "claude_footer_quota_tests.rs"]
mod tests;
