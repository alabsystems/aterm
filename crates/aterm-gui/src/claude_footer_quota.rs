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
//! foot of the cells that draw a level ([`tick_content`]); its lead, chart and tail are each a
//! hit of [`Fact::Account`], the chart's naming the column under the pointer
//! ([`RowPart::Chart`]). Lit, the lead and the tail are underlined and the chart's inks lift.

use std::ops::Range;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use aterm_agent::harness::outlook::Offsets;
use aterm_agent::harness::quota_view::{Chart, QuotaLine, QuotaView, Role};
use aterm_agent::harness::tap;
use aterm_core::render::{FxBlend, FxContent, FxInk, FxItem, FxLayer, FxRect, FxSlot, FxSpace};
use aterm_core::terminal::{RenderCell, Script, UnderlineStyle};
use aterm_effects::glass_inks::{GlassInks, field};

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
    /// The device pixels of a cell, for the midnights' hairlines.
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
    /// The midnights' hairlines over the chart ([`tick_content`]).
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

    /// The row composed from `key` and `view` ([`compose_cells`], [`tick_content`]).
    fn compose(&mut self, key: QuotaKey, view: &QuotaView) {
        if self.key == Some(key) {
            return;
        }
        #[cfg(test)]
        crate::work_counts::footer_rule_composed();
        compose_cells(&self.line, &key, &mut self.cells);
        self.ticks = tick_content(&view.chart, &self.line, key.cell, key.glass.track());
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

/// The ink a quota row's cell of `role` wears at palette position `t` (§1.3), lit or at rest:
/// lit, every ink but a warning's and the projected line's lifts one step toward the ink.
const fn ink(glass: &GlassInks, role: Role, t: f32, lit: bool) -> u32 {
    match (role, lit) {
        (Role::Lit, false) => glass.lit(),
        (Role::Lit | Role::Mark | Role::Line, true) | (Role::Line, false) => glass.letter(t),
        (Role::Value, false) => glass.value(),
        (Role::Value, true) => glass.lit(),
        (Role::Quiet | Role::Track, true) => glass.value(),
        (Role::Quiet, false) => glass.quiet(),
        (Role::Mark, false) => glass.mark(t),
        (Role::Warn, _) => glass.warn(),
        (Role::Track, false) => glass.track(),
    }
}

/// `line`'s cells in `key`'s tokens into `out` (cleared first), then blanks to `key.cells`: each
/// cell the ink of its role at its pane column's place in the field, bold where the line says;
/// lit, the lead's and the tail's glyphs underlined in their own ink, and the chart's lit by its
/// inks alone ([`ink`]): an underline is drawn whatever a cell's glyph, and under a one-eighth
/// line at level 2 to 8, a `╳` or the `▕` it would read as a second line at level 1.
fn compose_cells(line: &QuotaLine, key: &QuotaKey, out: &mut Vec<RenderCell>) {
    out.clear();
    let blank = RenderCell {
        script: Script::Normal,
        ..key.blank
    };
    for (i, &(ch, role, bold)) in line.cells.iter().enumerate() {
        let t = field(INDENT + i, key.cols);
        let fg = rgb_of(ink(&key.glass, role, t, key.lit));
        let underlined = key.lit && ch != ' ' && !line.chart.contains(&i);
        out.push(RenderCell {
            ch,
            fg,
            bold,
            underline: if underlined {
                UnderlineStyle::Single
            } else {
                UnderlineStyle::None
            },
            underline_color: underlined.then_some(fg),
            ..blank
        });
    }
    out.resize(key.cells.max(out.len()), blank);
}

/// The local midnights inside `line`'s chart of `chart`, each as its chart cell (from the chart's
/// first) and its exact place in device pixels from the chart's left edge at `cw` pixels a cell:
/// a past cell spans `1/k` of a day before `▕`, a future one after it. Nothing with no chart.
fn midnights_placed<'a>(
    chart: &'a Chart,
    line: &'a QuotaLine,
    cw: i64,
) -> impl Iterator<Item = (usize, i64)> + 'a {
    const DAY: i64 = 86_400;
    let k = i64::from(line.per_day);
    chart.midnights.iter().filter_map(move |&t| {
        if k == 0 {
            return None;
        }
        let (cell, x) = if t < chart.now {
            let n = (t - (chart.now - DAY)) * k;
            if n < 0 {
                return None;
            }
            (n / DAY, n * cw / DAY)
        } else if t < chart.now + 7 * DAY {
            let n = (t - chart.now) * k;
            (k + 1 + n / DAY, (k + 1) * cw + n * cw / DAY)
        } else {
            return None;
        };
        let cell = usize::try_from(cell).ok()?;
        (cell < line.chart.len()).then_some((cell, x))
    })
}

/// Whether a midnight's tick may stand in a chart cell holding `glyph` in `role`: one that draws
/// a level (measured or projected), `┄` or nothing, never a letter or a mark (a name, `→`, `↺`,
/// `╳`, `$`), which a line of ink would strike through.
const fn ticks_over(glyph: char, role: Role) -> bool {
    glyph == ' ' || matches!(role, Role::Mark | Role::Line | Role::Track)
}

/// The local midnights inside `line`'s chart of `chart` as ticks one device pixel wide at the foot
/// of a cell `cw` by `ch` pixels, a quarter of it tall, in `track` (§1.3, §2.2), each at its
/// exact place within its cell ([`midnights_placed`]) where that cell draws a level
/// ([`ticks_over`]). A tick, not a full-height hairline: only `▕` stands a whole cell high, so
/// now never reads as one more midnight, and no tick meets a `▔` at a cell's head to read as
/// `T`. `None` with no chart or no tick.
fn tick_content(
    chart: &Chart,
    line: &QuotaLine,
    (cw, ch): (u16, u16),
    track: u32,
) -> Option<Arc<FxContent>> {
    if line.per_day == 0
        || line.chart.is_empty()
        || chart.midnights.is_empty()
        || cw == 0
        || ch == 0
    {
        return None;
    }
    let h = (ch / 4).max(1);
    let items: Vec<FxItem> = midnights_placed(chart, line, i64::from(cw))
        .filter(|&(cell, _)| {
            let (glyph, role, _) = line.cells[line.chart.start + cell];
            ticks_over(glyph, role)
        })
        .filter_map(|(_, x)| i32::try_from(x).ok())
        .map(|x| {
            FxItem::Rect(FxRect {
                x,
                y: i32::from(ch - h),
                w: 1,
                h,
                ink: 0,
            })
        })
        .collect();
    (!items.is_empty()).then(|| Arc::new(FxContent::new(items, vec![FxInk::over(track, 255); 2])))
}

/// The stable FX layer id of `session`'s quota row's hairlines.
const fn tick_id(session: u64) -> u64 {
    layer_id(session ^ 0x7175_6f74_612d_7469)
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
    /// Every cell written.
    pub(super) cells: usize,
    /// The chart's cells in the row, and its cells a day.
    pub(super) chart: Range<usize>,
    pub(super) per_day: u8,
    /// The tail's first cell.
    pub(super) tail: usize,
    /// The midnights' hairlines, placed.
    pub(super) ticks: Option<FxLayer>,
}

impl QuotaWrite {
    /// The row's hits, `(frame columns, part)`: its lead to the chart (or the tail), the chart,
    /// and the tail to the write's end; each nonempty.
    pub(super) fn parts(&self) -> impl Iterator<Item = ((usize, usize), RowPart)> + '_ {
        let lead_end = if self.chart.is_empty() {
            self.tail.min(self.cells)
        } else {
            self.chart.start
        };
        let tail_start = if self.chart.is_empty() {
            lead_end
        } else {
            self.chart.end
        };
        [
            (0..lead_end, RowPart::Lead),
            (self.chart.clone(), RowPart::Chart(self.per_day)),
            (tail_start..self.cells, RowPart::Tail),
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
    /// hairlines placed for `session`'s pane `pane` at frame row `frame_row`: the write
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
        let view = self.shown.quota_view()?;
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
        let (len, chart, per_day, tail) = (
            line.cells.len(),
            line.chart.clone(),
            line.per_day,
            line.tail.start,
        );
        self.quota.compose(key, view);
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
            cells: key.cells,
            chart,
            per_day,
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
