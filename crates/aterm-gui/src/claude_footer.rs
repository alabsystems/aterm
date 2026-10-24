// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Host-side Claude Code composer footer resolver, cache, and glass splice.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use aterm_agent::harness::accounts;
use aterm_agent::harness::footer::{self, FactKind, FactMask, FooterFacts, LaunchFacts};
use aterm_agent::harness::profile::{SubscriptionDisplay, USAGE_SHOWN_S, WeeklyFigure, WeeklyGap};
use aterm_agent::harness::tap;
use aterm_core::render::RenderInput;
use aterm_core::terminal::RenderCell;
use aterm_effects::cli_marks::{Build, CliMark};
use aterm_window::event_loop::EventLoopProxy;

use crate::session_timeline::SessionTimeline;
use crate::{App, VisibleContentRoute, Wake, WindowId};

/// Interval between background footer-fact refreshes while a session is visible.
pub(crate) const RECHECK: Duration = Duration::from_secs(2);

/// Returns `true` if `program` is a harness-supported agent whose composer footer is rendered.
pub(crate) fn drawn_for(program: Option<&str>) -> bool {
    program
        .and_then(aterm_phase::program_of)
        .is_some_and(|program| match program {
            aterm_phase::Program::Claude
            | aterm_phase::Program::Agy
            | aterm_phase::Program::Muse => true,
            aterm_phase::Program::Codex
            | aterm_phase::Program::Grok
            | aterm_phase::Program::Kimi
            | aterm_phase::Program::Gemini
            | aterm_phase::Program::Generic => false,
        })
}

/// Whether `program`'s composer is left bare but for the top rule's switch under `switch` (agy
/// while its switch is not on).
pub(crate) fn bare_while_off(
    program: Option<aterm_phase::Program>,
    switch: Option<crate::autopilot::HarnessSwitch>,
) -> bool {
    program == Some(aterm_phase::Program::Agy) && switch.is_some_and(|s| !s.is_on())
}

/// One pane's painted light block.
struct Lit {
    session: u64,
    frame_row: usize,
    /// The frame column the rule's edit starts at — its key in the write.
    col: usize,
    term_row: usize,
    /// The frame column the lights (not the title) start at.
    lights_col: usize,
    /// The frame columns of the title slot's words (empty: no title).
    title: std::ops::Range<usize>,
    block: crate::claude_lights::Block,
    /// The write put its row on the frame.
    shown: bool,
}

/// One pane's composed top rule to write ([`ComposedTop`], [`ComposedTop::glass`], [`rule_edits`]).
struct TopWrite {
    frame_row: usize,
    col: usize,
    leaf_index: usize,
    ground: RenderCell,
    glass: std::ops::Range<usize>,
}

/// The top-effort key's target on a top rule.
struct TopHit {
    frame_row: usize,
    term_row: usize,
    cols: (usize, usize),
    session: u64,
    ghost: bool,
}

/// A top line's lockup on the glass, the hover title's target.
struct MarkWrite {
    frame_row: usize,
    term_row: usize,
    edit_col: usize,
    cols: std::ops::Range<usize>,
    session: u64,
}

/// What one pane's top line is spliced with ([`App::splice_top_line`], [`top_build`]).
struct TopLineAt {
    leaf_index: usize,
    session: u64,
    program_pgid: i32,
    program: aterm_phase::Program,
    build: Build,
    region: PaneTextRegion,
    pane: crate::claude_card::PaneGeom,
    blank: RenderCell,
    switch: Option<SwitchPaint>,
    palette: Option<aterm_effects::harness_card::CardPalette>,
    held: bool,
    cell: (u16, u16),
    mark_fg: [u8; 3],
}

/// One undoable row edit for agy's transcript presentation on `RenderInput`.
struct AgyRowEdit {
    frame_row: usize,
    col: usize,
    cells: Vec<RenderCell>,
    ground: RenderCell,
}

/// Reusable per-window scratch buffers for composing the Claude footer splice.
#[derive(Default)]
pub(crate) struct SpliceLists {
    panes: Vec<FooterPane>,
    seen: Vec<SeenPane>,
    /// Panes whose composer is on the glass with no plain rule glyph to write into.
    bare: Vec<u64>,
    /// The rules' compositions, written as [`rule_edits`].
    rules: Vec<RuleWrite>,
    /// agy's glass-only transcript markdown edits.
    agy_edits: Vec<AgyRowEdit>,
    /// Claude's mode pills relabelled `YOLO on`/`YOLO off` ([`collect_pill_relabel`]).
    relabels: Vec<RelabelWrite>,
    relabel_cells: Vec<RenderCell>,
    /// The composer top rules' compositions ([`ComposedTop`]).
    tops: Vec<TopWrite>,
    /// The top-effort key's targets each top rule would put on the glass ([`TopHit`]).
    top_hits: Vec<TopHit>,
    /// Sessions whose top rule showed Claude's top-effort tag for a process not yet noted as having
    /// shown it.
    top_seen: Vec<(u64, i32)>,
    /// The lockups each top line would put on the glass ([`MarkWrite`]).
    marks: Vec<MarkWrite>,
    /// The leaves whose top line alone was read ([`PaneTextCache::retain_active`]).
    top_only: Vec<usize>,
    /// The harness switches this splice drew on composer top rules.
    border_toggles: Vec<crate::claude_lights::BorderSwitch>,
    lit: Vec<Lit>,
    /// The facts each rule's write would put on the glass ([`FactWrite`]).
    facts: Vec<FactWrite>,
    /// The rule row's coverable cells ([`rule_mask_into`]), one pane's at a time.
    mask: Vec<bool>,
    /// Sessions whose log the launch card's amend moved.
    log_owed: Vec<u64>,
    /// The agents panels this frame lights (`crate::claude_panel`).
    panels: Vec<crate::claude_panel::PanelLeaf>,
}

/// One pane's composition for its composer rule ([`compose_rule_spans`], [`PaneFooter`]).
struct RuleWrite {
    frame_row: usize,
    col: usize,
    leaf_index: usize,
    ground: RenderCell,
    glass: std::ops::Range<usize>,
}

/// One fact a rule's write would put on the glass ([`Lit::shown`]).
struct FactWrite {
    frame_row: usize,
    term_row: usize,
    /// The column of the edit that writes the facts ([`rule_edits`]).
    edit_col: usize,
    session: u64,
    cols: (usize, usize),
    fact: crate::claude_lights::Fact,
    restores: FactMask,
}

impl SpliceLists {
    /// Empty every list, keeping its buffer.
    fn clear(&mut self) {
        self.panes.clear();
        self.seen.clear();
        self.bare.clear();
        self.rules.clear();
        self.agy_edits.clear();
        self.relabels.clear();
        self.relabel_cells.clear();
        self.tops.clear();
        self.top_hits.clear();
        self.top_seen.clear();
        self.marks.clear();
        self.top_only.clear();
        self.border_toggles.clear();
        self.lit.clear();
        self.facts.clear();
        self.mask.clear();
        self.log_owed.clear();
        self.panels.clear();
    }
}

/// One Claude Code pane's composer rule.
struct FooterPane {
    session: u64,
    /// The pane's leaf ([`PaneFooter`]).
    leaf_index: usize,
    frame_row: usize,
    /// The same row as a window grid row.
    term_row: usize,
    col_off: usize,
    /// The pane columns aterm may write into (`footer::rule_run`).
    run: std::ops::Range<usize>,
    /// One of the rule's own glyph cells.
    rule: RenderCell,
    blank: RenderCell,
    /// The session's own Autopilot word and its view.
    autopilot: (
        Option<aterm_agent::supervise::Autopilot>,
        crate::autopilot::AutopilotView,
    ),
    /// Whether Claude's mode pill reads `YOLO on`/`YOLO off` on this pane's glass
    /// ([`relabels_pill`], [`collect_pill_relabel`]).
    relabelled: bool,
}

/// A visible Claude Code pane and the lights' reading of its rows.
struct SeenPane {
    leaf_index: usize,
    session: u64,
    lights: LightsRead,
}

/// Cached harness-lights reading (`None` if the reader panicked, `Some(reading)` otherwise).
pub(crate) type LightsRead = Option<Option<aterm_agent::harness::lights::Reading>>;

/// Per-pane cached screen rows and derived footer, launch-card, and lights readings.
#[derive(Debug, Default)]
pub(crate) struct PaneTextCache {
    panes: Vec<Option<CachedPaneText>>,
    /// Each leaf's footer as earlier frames composed it.
    footers: Vec<PaneFooter>,
    /// The last rebuild's number ([`CachedPaneText::text_gen`]).
    rebuilds: u64,
    /// The rows a rebuild found changed ([`Self::rows`]), reused.
    changed: Vec<usize>,
    /// The top rules' mark contents, shared by the leaves ([`MarkContents`]).
    marks: MarkContents,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PaneTextKey {
    scratch: Tokens,
    session: u64,
    region: PaneTextRegion,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PaneTextRegion {
    /// Rows of window chrome before the terminal cells.
    strip: usize,
    row_off: usize,
    col_off: usize,
    rows: usize,
    cols: usize,
}

struct CachedPaneText {
    key: PaneTextKey,
    rows: Arc<Vec<String>>,
    /// This rebuild's number, unique in the cache.
    text_gen: u64,
    /// The footer's and the lights' readings of `rows` ([`Readings::after`]).
    readings: Readings,
    /// The Claude Code build the vendor-drift check last read these readings for
    /// ([`note_vendor_drift`]).
    drift: std::cell::Cell<Option<u64>>,
    /// The launch card on `rows` for the footer's open model or effort
    /// ([`PaneTextCache::footer_card`]).
    footer_card: std::cell::OnceCell<Option<(aterm_phase::Program, aterm_phase::LaunchCardRead)>>,
    /// Claude Code's context indicator on `rows` ([`PaneTextCache::context_reading`]).
    ctx_reading: std::cell::OnceCell<Option<u8>>,
    /// Whether `rows` say Claude Code's indicator has gone ([`PaneTextCache::indicator_gone`]).
    ctx_gone: std::cell::OnceCell<bool>,
    /// The signed-in account on `rows` ([`PaneTextCache::screen_account`]).
    account: std::cell::OnceCell<Option<(String, Option<String>)>>,
    /// Muse Code's weekly quota on `rows` ([`PaneTextCache::usage_weekly`]).
    weekly: std::cell::OnceCell<Option<aterm_phase::muse::UsageCardWeekly>>,
    /// Muse Code's subscription tier on `rows` ([`PaneTextCache::subscription_plan`]).
    plan: std::cell::OnceCell<Option<String>>,
    /// The rule's coverable run and its glyph on frame row `.frame_row` ([`rule_mask_into`],
    /// [`Self::stamps`]).
    rule_run: std::cell::Cell<Option<KeptRun>>,
    /// The top rule's plain run on frame row `.frame_row` ([`read_top_run`], [`Self::rule_run`]).
    top_run: std::cell::Cell<Option<KeptTop>>,
    /// Gemini CLI's top line on `rows` ([`top_line`]).
    gemini_top: std::cell::OnceCell<Option<usize>>,
    /// Codex's top line on `rows` ([`TopLine::Slab`], [`top_line`]).
    codex_pad: std::cell::OnceCell<Option<usize>>,
    /// The harness card's reading of `rows`.
    card: std::cell::OnceCell<crate::claude_card::CardReading>,
    /// The last reading's card, until this rebuild's reading is taken.
    card_before: std::cell::Cell<Option<crate::claude_card::CardBefore>>,
    /// The agents panel's reading of `rows` and the frame they were read from
    /// ([`PaneTextCache::agents_panel`]).
    panel: std::cell::OnceCell<Option<Arc<crate::claude_panel::PanelRead>>>,
    /// The last reading's panel, until this rebuild's is taken.
    panel_before: std::cell::Cell<Option<Arc<crate::claude_panel::PanelRead>>>,
    /// Claude Code's update notice under the card on `rows`.
    notice: std::cell::Cell<Option<(usize, Option<aterm_phase::NoticeScan>)>>,
    /// The engine's revisions of the rows `rows` were read from ([`RowStamps`],
    /// [`Self::rule_run`]).
    stamps: RowStamps,
    /// The kept runs' rows as they were read, where no revision vouches for
    /// them ([`KeptRows`]).
    kept_cells: std::cell::RefCell<KeptRows>,
}

/// The row revisions of the scratch a pane's rows were read from ([`StampLane`]).
#[derive(Debug, Default)]
struct RowStamps {
    lane: Option<StampLane>,
    revs: Vec<u64>,
}

/// What two scratches' row revisions must share to be compared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StampLane {
    lane: u64,
    rows: usize,
    cols: usize,
    row_shift: usize,
    engine_alt: bool,
    base_y: i64,
    absolute_row_revision: u64,
}

impl StampLane {
    /// `scratch`'s lane, where its stamps vouch for its rows.
    fn of(scratch: &RenderInput) -> Option<Self> {
        #[cfg(test)]
        if !READ_BY_STAMP.with(std::cell::Cell::get) {
            return None;
        }
        (scratch.row_rev_lane != 0
            && scratch.row_rev.len() == scratch.rows
            && scratch.cells.len() == scratch.rows
            && scratch.snapshot_seq == scratch.shifted_fill_seq
            && scratch.engine_row_order == aterm_core::render::RowOrder::Logical
            && scratch.display_offset == 0)
            .then_some(Self {
                lane: scratch.row_rev_lane,
                rows: scratch.rows,
                cols: scratch.cols,
                row_shift: scratch.row_shift,
                engine_alt: scratch.engine_alt,
                base_y: scratch.base_y,
                absolute_row_revision: scratch.absolute_row_revision,
            })
    }
}

#[cfg(test)]
thread_local! {
    /// Row revisions vouch for unchanged rows ([`StampLane::of`], [`read_every_row_in`]).
    static READ_BY_STAMP: std::cell::Cell<bool> = const { std::cell::Cell::new(true) };
}

/// Runs `f` with row-revision caching disabled so every row is re-compared.
#[cfg(test)]
pub(crate) fn read_every_row_in<R>(f: impl FnOnce() -> R) -> R {
    let was = READ_BY_STAMP.with(|k| k.replace(false));
    let out = f();
    READ_BY_STAMP.with(|k| k.set(was));
    out
}

/// Whether copied cells may vouch for a kept run's row of `scratch`: a
/// scratch no row revision vouches for ([`StampLane::of`]), as a split
/// window's composed frame is.
fn cells_vouch(scratch: &RenderInput) -> bool {
    #[cfg(test)]
    if !READ_BY_STAMP.with(std::cell::Cell::get) {
        return false;
    }
    StampLane::of(scratch).is_none()
}

/// The rows of a pane's kept runs as they were read ([`KeptRun`], [`KeptTop`]).
#[derive(Debug, Default)]
struct KeptRows {
    rule: KeptCells,
    top: KeptCells,
}

/// One kept run's row as it was read, where no row revision vouches for it
/// ([`cells_vouch`]): a rebuild keeps the run while the row holds these
/// cells, all of the row a read reads but its images ([`has_images`], which
/// a rebuild checks apart).
#[derive(Debug, Default)]
struct KeptCells {
    /// The frame row the cells were copied from, `None` where none were: a
    /// cluster or a combining mark in the span, whose reads the copy omits.
    frame_row: Option<usize>,
    cells: Vec<RenderCell>,
}

impl KeptCells {
    /// Frame row `frame_row`'s cells in `cols`, where they are all a read reads.
    fn span<'a>(
        scratch: &'a RenderInput,
        frame_row: usize,
        cols: &std::ops::Range<usize>,
    ) -> Option<&'a [RenderCell]> {
        if touches(&scratch.clusters, frame_row, cols)
            || touches(&scratch.combining, frame_row, cols)
        {
            return None;
        }
        let row = scratch.cells.get(frame_row)?;
        row.get(cols.start..row.len().min(cols.end))
    }

    /// Copy frame row `frame_row`'s cells in `cols` of `scratch`, its buffer reused.
    fn note(&mut self, scratch: &RenderInput, frame_row: usize, cols: &std::ops::Range<usize>) {
        self.cells.clear();
        self.frame_row = None;
        if let Some(span) = Self::span(scratch, frame_row, cols) {
            self.cells.extend_from_slice(span);
            self.frame_row = Some(frame_row);
        }
    }

    /// Forget the copy: nothing vouches for the row.
    fn forget(&mut self) {
        self.frame_row = None;
    }

    /// Whether frame row `frame_row` of `scratch` holds the cells copied off it.
    fn holds(
        &self,
        scratch: &RenderInput,
        frame_row: usize,
        cols: &std::ops::Range<usize>,
    ) -> bool {
        self.frame_row == Some(frame_row)
            && Self::span(scratch, frame_row, cols) == Some(self.cells.as_slice())
    }
}

/// Whether frame row `frame_row` of `scratch` holds an image placement.
fn has_images(scratch: &RenderInput, frame_row: usize) -> bool {
    scratch
        .images
        .get(frame_row)
        .is_some_and(|row| !row.is_empty())
}

/// A composer rule's run read off frame row `frame_row` ([`CachedPaneText::rule_run`]).
#[derive(Debug, Clone, Copy)]
struct KeptRun {
    frame_row: usize,
    run: Option<(usize, usize, RenderCell)>,
    /// The row held an image placement when it was read.
    images: bool,
}

impl KeptRun {
    /// The frame row it was read off, where it may be kept past its rebuild.
    fn row(&self) -> Option<usize> {
        (!self.images).then_some(self.frame_row)
    }
}

/// A composer top line's plain run read off frame row `frame_row` as a line of kind `line` for a
/// pane ground `bg` ([`CachedPaneText::top_run`]).
#[derive(Debug, Clone, Copy)]
struct KeptTop {
    frame_row: usize,
    line: TopLine,
    bg: [u8; 3],
    run: Option<TopRun>,
    /// Claude's own top-effort tag on the row, its key's frame columns (`footer::top_rule_tag`).
    tag: Option<(usize, usize)>,
    /// The row held an image placement when it was read.
    images: bool,
}

impl KeptTop {
    /// The frame row it was read off, where it may be kept past its rebuild.
    fn row(&self) -> Option<usize> {
        (!self.images).then_some(self.frame_row)
    }
}

impl std::fmt::Debug for CachedPaneText {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CachedPaneText")
            .field("key", &self.key)
            .field("rows", &self.rows.len())
            .field("text_gen", &self.text_gen)
            .field("readings", &self.readings)
            .field("card", &self.card)
            .finish_non_exhaustive()
    }
}

/// The footer's and the lights' readings of one pane's rows ([`read_pane`]).
#[derive(Debug, Clone, Copy)]
struct Readings {
    /// The composer's frame and the rows its search read.
    reach: Option<aterm_phase::phase::FrameReach>,
    /// The footer's: where its rule is, and whether a mode row this build reads sits under it.
    rule: FooterRead,
    /// The lights'.
    lights: LightsRead,
    /// Claude's mode pill under the bottom rule ([`collect_pill_relabel`]).
    relabel: Option<footer::Relabel>,
}

impl Readings {
    /// These readings brought up to `rows`.
    fn after(self, session: u64, rows: &[String], changed: &[usize]) -> (Readings, bool) {
        let Some(reach) = self.reach else {
            return (read_pane(session, rows, None), false);
        };
        let now = if reach.unmoved(changed) {
            Some(reach)
        } else {
            #[cfg(test)]
            crate::work_counts::footer_frame_searched();
            crate::reader_guard::read_or_none(session, "Claude Code footer", rows, || {
                aterm_phase::phase::FrameReach::of_or_agy(rows)
            })
        };
        match now {
            Some(now) if reach.holds_under(&now, changed) => (
                Readings {
                    reach: Some(now),
                    ..self
                },
                true,
            ),
            now => (read_pane(session, rows, now), false),
        }
    }
}

/// Read pane `session`'s `rows` for the footer and the lights ([`read_footer`]).
fn read_pane(
    session: u64,
    rows: &[String],
    found: Option<aterm_phase::phase::FrameReach>,
) -> Readings {
    #[cfg(test)]
    crate::work_counts::footer_pane_read();
    let reach = found.or_else(|| {
        crate::reader_guard::read_or_none(session, "Claude Code footer", rows, || {
            aterm_phase::phase::FrameReach::of_or_agy(rows)
        })
    });
    let rule =
        reach.and_then(|reach| read_footer(session, rows, |rows| read_rule_under(rows, reach)));
    let lights =
        crate::reader_guard::read_or_none(session, "Claude Code lights", rows, || match reach {
            Some(reach) => reach
                .rules()
                .map(|(_, bottom)| aterm_agent::harness::lights::reading_under(rows, bottom)),
            None => aterm_agent::harness::lights::read_lights(rows),
        });
    let relabel = rule.and_then(|read| {
        #[cfg(test)]
        crate::work_counts::pill_relabel_read();
        crate::reader_guard::read_or_none(session, "Claude Code mode pill", rows, || {
            footer::relabel_under(rows, read.rule)
        })
        .flatten()
    });
    Readings {
        reach,
        rule,
        lights,
        relabel,
    }
}

impl PaneTextCache {
    /// A removed leaf must not retain a transcript.
    fn trim(&mut self, leaves: usize) {
        self.panes.truncate(leaves);
        self.footers.truncate(leaves);
    }

    /// A pane that no longer shows a Claude footer must not keep its old transcript resident just
    /// because another pane still has one.
    fn retain_active(
        &mut self,
        active: impl Fn(usize) -> bool,
        card: impl Fn(usize) -> bool,
        top: impl Fn(usize) -> bool,
    ) {
        for (index, cached) in self.panes.iter_mut().enumerate() {
            if !active(index) && !card(index) && !top(index) {
                *cached = None;
            }
        }
        for (index, footer) in self.footers.iter_mut().enumerate() {
            if active(index) {
                continue;
            }
            if top(index) {
                if footer.from.is_some() || footer.rule.key.is_some() {
                    *footer = PaneFooter {
                        session: footer.session,
                        top: std::mem::take(&mut footer.top),
                        ..PaneFooter::default()
                    };
                }
            } else if footer.from.is_some() || footer.rule.key.is_some() || footer.top.key.is_some()
            {
                *footer = PaneFooter::default();
            }
        }
    }

    /// Leaf `leaf_index`'s rows.
    fn rows(
        &mut self,
        leaf_index: usize,
        session: u64,
        scratch: &RenderInput,
        region: PaneTextRegion,
    ) -> Arc<Vec<String>> {
        let key = PaneTextKey {
            scratch: Tokens::of(scratch),
            session,
            region,
        };
        if let Some(Some(cached)) = self.panes.get(leaf_index)
            && cached.key == key
        {
            return Arc::clone(&cached.rows);
        }
        let last = self
            .panes
            .get_mut(leaf_index)
            .and_then(Option::take)
            .filter(|cached| cached.key.session == session && cached.key.region == region);
        let changed = &mut self.changed;
        changed.clear();
        let frame_row = |r: usize| region.strip + region.row_off + r;
        let span = |r: usize| {
            scratch.cells.get(frame_row(r)).map(|row| {
                let row = row.get(region.col_off..).unwrap_or_default();
                &row[..row.len().min(region.cols)]
            })
        };
        let cells = |r: usize| span(r).map(|row| row.iter().map(|c| c.ch));
        let same = |was: &str, r: usize| {
            #[cfg(test)]
            crate::work_counts::footer_row_compared();
            same_text(was, span(r))
        };
        let fresh = || -> Vec<String> {
            (0..region.rows)
                .map(|r| cells(r).map_or_else(String::new, Iterator::collect))
                .collect()
        };
        let lane = StampLane::of(scratch);
        let rev = |r: usize| {
            lane.map_or(0, |_| {
                scratch.row_rev.get(frame_row(r)).copied().unwrap_or(0)
            })
        };
        let (
            text,
            readings,
            drift,
            card,
            card_before,
            notice,
            stamps,
            rule_run,
            panel_before,
            top_run,
            kept_cells,
        ) = match last {
            Some(cached) => {
                let vouched = lane.is_some() && cached.stamps.lane == lane;
                let was = &cached.stamps.revs;
                let unmoved = |r: usize| {
                    vouched && {
                        let now = rev(r);
                        now != 0 && was.get(r) == Some(&now)
                    }
                };
                let mut rows = cached.rows;
                if let Some(texts) = Arc::get_mut(&mut rows) {
                    texts.resize_with(region.rows, String::new);
                    for (r, text) in texts.iter_mut().enumerate() {
                        if !unmoved(r) && !same(text, r) {
                            changed.push(r);
                            text.clear();
                            text.extend(cells(r).into_iter().flatten());
                        }
                    }
                } else {
                    changed.extend((0..region.rows).filter(|&r| {
                        !unmoved(r) && !same(rows.get(r).map_or("", String::as_str), r)
                    }));
                    if !changed.is_empty() {
                        rows = Arc::new(fresh());
                    }
                }
                let pane_row = |frame: usize| {
                    frame
                        .checked_sub(region.strip + region.row_off)
                        .filter(|&r| r < region.rows)
                };
                // A row a revision cannot vouch for, a split window's, is kept
                // while it holds the cells its run was read from.
                let by_cells = cells_vouch(scratch);
                let pane_span = region.col_off..region.col_off + region.cols;
                let kept_cells = cached.kept_cells.into_inner();
                let keeps = |frame: usize, noted: &KeptCells| {
                    pane_row(frame).is_some_and(|r| {
                        unmoved(r) || (by_cells && noted.holds(scratch, frame, &pane_span))
                    }) && !has_images(scratch, frame)
                };
                let rule_run = cached
                    .rule_run
                    .get()
                    .filter(|kept| kept.row().is_some_and(|f| keeps(f, &kept_cells.rule)));
                let top_run = cached
                    .top_run
                    .get()
                    .filter(|kept| kept.row().is_some_and(|f| keeps(f, &kept_cells.top)));
                let (readings, kept) = cached.readings.after(session, &rows, changed);
                let drift = if kept { cached.drift.get() } else { None };
                let panel_before = match cached.panel.into_inner() {
                    Some(read) => read,
                    None => cached.panel_before.into_inner(),
                };
                let reading = cached.card.into_inner();
                let before = cached.card_before.into_inner();
                let (card, card_before) = match reading {
                    Some(reading) if reading.holds(session, &rows, changed) => {
                        (std::cell::OnceCell::from(reading), None)
                    }
                    Some(reading) => (
                        std::cell::OnceCell::new(),
                        match reading.read {
                            Some(card) => Some(crate::claude_card::CardBefore { card, last: true }),
                            None => before.map(crate::claude_card::CardBefore::earlier),
                        },
                    ),
                    None if changed.is_empty() => (std::cell::OnceCell::new(), before),
                    None => (
                        std::cell::OnceCell::new(),
                        before.map(crate::claude_card::CardBefore::earlier),
                    ),
                };
                let notice = cached.notice.get().filter(|(_, scan)| {
                    scan.is_some_and(|scan| {
                        crate::reader_guard::read_or_none(session, "harness notice", &rows, || {
                            aterm_phase::update_notice_unmoved(&rows, &scan, changed)
                        })
                        .unwrap_or(false)
                    })
                });
                let mut stamps = cached.stamps;
                stamps.lane = lane;
                stamps.revs.clear();
                if lane.is_some() {
                    stamps.revs.extend((0..region.rows).map(rev));
                }
                (
                    rows,
                    readings,
                    drift,
                    card,
                    card_before,
                    notice,
                    stamps,
                    rule_run,
                    panel_before,
                    top_run,
                    kept_cells,
                )
            }
            None => {
                let rows = Arc::new(fresh());
                let readings = read_pane(session, &rows, None);
                let stamps = RowStamps {
                    lane,
                    revs: if lane.is_some() {
                        (0..region.rows).map(rev).collect()
                    } else {
                        Vec::new()
                    },
                };
                (
                    rows,
                    readings,
                    None,
                    std::cell::OnceCell::new(),
                    None,
                    None,
                    stamps,
                    None,
                    None,
                    None,
                    KeptRows::default(),
                )
            }
        };
        if self.panes.len() <= leaf_index {
            self.panes.resize_with(leaf_index + 1, || None);
        }
        #[cfg(test)]
        crate::work_counts::pane_text_rebuilt();
        self.rebuilds += 1;
        self.panes[leaf_index] = Some(CachedPaneText {
            key,
            rows: Arc::clone(&text),
            text_gen: self.rebuilds,
            readings,
            drift: std::cell::Cell::new(drift),
            footer_card: std::cell::OnceCell::new(),
            ctx_reading: std::cell::OnceCell::new(),
            ctx_gone: std::cell::OnceCell::new(),
            account: std::cell::OnceCell::new(),
            weekly: std::cell::OnceCell::new(),
            plan: std::cell::OnceCell::new(),
            rule_run: std::cell::Cell::new(rule_run),
            top_run: std::cell::Cell::new(top_run),
            gemini_top: std::cell::OnceCell::new(),
            codex_pad: std::cell::OnceCell::new(),
            card,
            card_before: std::cell::Cell::new(card_before),
            panel: std::cell::OnceCell::new(),
            panel_before: std::cell::Cell::new(panel_before),
            notice: std::cell::Cell::new(notice),
            stamps,
            kept_cells: std::cell::RefCell::new(kept_cells),
        });
        text
    }

    /// The footer's and the lights' readings of leaf `leaf_index`'s cached rows ([`Self::rows`]).
    fn readings(&self, leaf_index: usize) -> Option<Readings> {
        Some(self.panes.get(leaf_index)?.as_ref()?.readings)
    }

    /// The vendor-drift check ([`note_vendor_drift`]).
    fn note_drift(&self, leaf_index: usize, session: u64, version: Option<&str>) {
        use std::hash::{Hash, Hasher};
        let Some(Some(cached)) = self.panes.get(leaf_index) else {
            return;
        };
        let mut h = std::collections::hash_map::DefaultHasher::new();
        version.hash(&mut h);
        let build = h.finish();
        if cached.drift.replace(Some(build)) != Some(build) {
            note_vendor_drift(session, version, &cached.rows);
        }
    }

    /// The launch card on leaf `leaf_index`'s cached rows.
    fn footer_card(
        &self,
        leaf_index: usize,
        session: u64,
    ) -> Option<&(aterm_phase::Program, aterm_phase::LaunchCardRead)> {
        let cached = self.panes.get(leaf_index)?.as_ref()?;
        if cached.key.session != session {
            return None;
        }
        cached
            .footer_card
            .get_or_init(|| {
                #[cfg(test)]
                crate::work_counts::footer_card_read();
                crate::reader_guard::read_or_none(
                    session,
                    "Claude Code launch card",
                    &cached.rows,
                    || footer::launch_card_read(&cached.rows),
                )
                .flatten()
            })
            .as_ref()
    }

    /// Claude Code's context indicator on leaf `leaf_index`'s cached rows.
    fn context_reading(&self, leaf_index: usize) -> Option<u8> {
        let cached = self.panes.get(leaf_index)?.as_ref()?;
        *cached.ctx_reading.get_or_init(|| {
            crate::reader_guard::read_or_none(
                cached.key.session,
                "Claude Code context indicator",
                &cached.rows,
                || aterm_phase::phase::context_left_shown(&cached.rows),
            )
            .flatten()
        })
    }

    /// Whether leaf `leaf_index`'s cached rows say Claude Code's context indicator has gone.
    fn indicator_gone(&self, leaf_index: usize) -> bool {
        let Some(Some(cached)) = self.panes.get(leaf_index) else {
            return false;
        };
        *cached.ctx_gone.get_or_init(|| {
            crate::reader_guard::read_or_none(
                cached.key.session,
                "Claude Code context indicator gone",
                &cached.rows,
                || footer::context_gone(&cached.rows),
            )
            .unwrap_or(false)
        })
    }

    /// The signed-in account on leaf `leaf_index`'s cached rows, its email and its plan.
    fn screen_account(
        &self,
        leaf_index: usize,
        agent: Option<aterm_phase::Program>,
    ) -> Option<(&str, Option<&str>)> {
        let cached = self.panes.get(leaf_index)?.as_ref()?;
        cached
            .account
            .get_or_init(|| {
                crate::reader_guard::read_or_none(
                    cached.key.session,
                    "launch or status card account",
                    &cached.rows,
                    || match agent {
                        Some(aterm_phase::Program::Muse) => {
                            aterm_phase::muse::status_card_account(&cached.rows)
                        }
                        _ => aterm_phase::agy::launch_card_account(&cached.rows)
                            .map(|(email, plan)| (email.to_owned(), plan.map(str::to_owned))),
                    },
                )
                .flatten()
            })
            .as_ref()
            .map(|(email, plan)| (email.as_str(), plan.as_deref()))
    }

    /// Muse Code's or agy's weekly quota on leaf `leaf_index`'s cached rows.
    fn usage_weekly(
        &self,
        leaf_index: usize,
        program: Option<aterm_phase::Program>,
    ) -> Option<(&str, Option<&str>)> {
        let cached = self.panes.get(leaf_index)?.as_ref()?;
        let model = self
            .footers
            .get(leaf_index)
            .and_then(|f| f.shown().model.as_deref());
        cached
            .weekly
            .get_or_init(|| {
                crate::reader_guard::read_or_none(
                    cached.key.session,
                    "usage card weekly",
                    &cached.rows,
                    || match program {
                        Some(aterm_phase::Program::Agy) => {
                            aterm_phase::agy::usage_card_weekly_for_model(&cached.rows, model)
                        }
                        _ => aterm_phase::muse::usage_card_weekly(&cached.rows),
                    },
                )
                .flatten()
            })
            .as_ref()
            .map(|weekly| (weekly.text.as_str(), weekly.reset.as_deref()))
    }

    /// Muse Code's subscription tier on leaf `leaf_index`'s cached rows.
    fn subscription_plan(&self, leaf_index: usize) -> Option<&str> {
        let cached = self.panes.get(leaf_index)?.as_ref()?;
        cached
            .plan
            .get_or_init(|| {
                crate::reader_guard::read_or_none(
                    cached.key.session,
                    "Muse Code subscription card plan",
                    &cached.rows,
                    || aterm_phase::muse::subscription_card_plan(&cached.rows),
                )
                .flatten()
            })
            .as_deref()
    }

    /// The run of leaf `leaf_index`'s composer rule on frame row `frame_row` of `scratch` that
    /// aterm may cover ([`rule_mask_into`]).
    fn rule_run(
        &self,
        leaf_index: usize,
        scratch: &RenderInput,
        frame_row: usize,
        (col_off, pane_cols): (usize, usize),
        mask: &mut Vec<bool>,
    ) -> Option<(std::ops::Range<usize>, RenderCell)> {
        let read = |mask: &mut Vec<bool>| {
            #[cfg(test)]
            crate::work_counts::footer_rule_masked();
            let rule = rule_mask_into(mask, scratch, frame_row, col_off, pane_cols)?;
            let run = footer::rule_run(mask)?;
            Some((run.start, run.end, rule))
        };
        let run = match self.panes.get(leaf_index) {
            Some(Some(cached)) => match cached.rule_run.get() {
                Some(kept) if kept.frame_row == frame_row => kept.run,
                _ => {
                    let run = read(mask);
                    let mut noted = cached.kept_cells.borrow_mut();
                    if cells_vouch(scratch) {
                        let span = col_off..col_off + pane_cols;
                        noted.rule.note(scratch, frame_row, &span);
                    } else {
                        noted.rule.forget();
                    }
                    cached.rule_run.set(Some(KeptRun {
                        frame_row,
                        run,
                        images: has_images(scratch, frame_row),
                    }));
                    run
                }
            },
            _ => read(mask),
        };
        run.map(|(start, end, rule)| (start..end, rule))
    }

    /// The plain run of leaf `leaf_index`'s composer top line ([`read_top_run`],
    /// [`Self::rule_run`]).
    #[expect(
        clippy::too_many_arguments,
        reason = "the pane, its row and the line kind, as the read is keyed"
    )]
    fn top_run(
        &self,
        leaf_index: usize,
        (session, program): (u64, aterm_phase::Program),
        scratch: &RenderInput,
        region: PaneTextRegion,
        top_rule: usize,
        line: TopLine,
        blank: RenderCell,
    ) -> (Option<TopRun>, Option<(usize, usize)>) {
        let frame_row = region.strip + region.row_off + top_rule;
        let muse = program == aterm_phase::Program::Muse;
        let Some(Some(cached)) = self.panes.get(leaf_index) else {
            return (
                read_top_run(scratch, region, top_rule, line, blank, muse),
                None,
            );
        };
        match cached.top_run.get() {
            Some(kept)
                if kept.frame_row == frame_row && kept.line == line && kept.bg == blank.bg =>
            {
                (kept.run, kept.tag)
            }
            _ => {
                let run = read_top_run(scratch, region, top_rule, line, blank, muse);
                let tag = match line {
                    TopLine::Rule => crate::reader_guard::read_or_none(
                        session,
                        "Claude Code top rule",
                        &cached.rows,
                        || footer::top_rule_tag(cached.rows.get(top_rule)?),
                    )
                    .flatten()
                    .map(|(a, b)| (region.col_off + a, region.col_off + b)),
                    TopLine::Slab => None,
                };
                let mut noted = cached.kept_cells.borrow_mut();
                // A slab's read reads the prompt row under it too: no copy of
                // its own row vouches for it.
                if line == TopLine::Rule && cells_vouch(scratch) {
                    let span = region.col_off..region.col_off + region.cols;
                    noted.top.note(scratch, frame_row, &span);
                } else {
                    noted.top.forget();
                }
                cached.top_run.set(Some(KeptTop {
                    frame_row,
                    line,
                    bg: blank.bg,
                    run,
                    tag,
                    images: has_images(scratch, frame_row),
                }));
                (run, tag)
            }
        }
    }

    /// Where leaf `leaf_index`'s cached rows put `program`'s top line ([`top_line`]).
    fn top_line(
        &self,
        leaf_index: usize,
        program: aterm_phase::Program,
    ) -> Option<(usize, TopLine)> {
        top_line(program, self.panes.get(leaf_index)?.as_ref()?)
    }

    /// Leaf `leaf_index`'s top rule composed from `key` ([`PaneFooter::compose_top`]).
    fn compose_top(&mut self, leaf_index: usize, key: TopKey) -> &ComposedTop {
        let Self { footers, marks, .. } = self;
        footers[leaf_index].compose_top(key, marks)
    }

    /// Leaf `leaf_index`'s composed top rule's mark sprite ([`mark_layer`]).
    fn top_mark(&self, leaf_index: usize) -> Option<&aterm_core::render::FxLayer> {
        self.footers.get(leaf_index)?.top.mark.as_ref()
    }

    /// Leaf `leaf_index`'s composed top rule's cells ([`compose_top`]).
    fn top_cells(&self, leaf_index: usize) -> &[RenderCell] {
        self.footers
            .get(leaf_index)
            .map_or(&[], |f| f.top.cells.as_slice())
    }

    /// Leaf `leaf_index`'s footer, `session`'s.
    fn footer(&mut self, leaf_index: usize, session: u64) -> &mut PaneFooter {
        if self.footers.len() <= leaf_index {
            self.footers
                .resize_with(leaf_index + 1, PaneFooter::default);
        }
        let footer = &mut self.footers[leaf_index];
        if footer.session != session {
            *footer = PaneFooter {
                session,
                ..PaneFooter::default()
            };
        }
        footer
    }

    /// Take in the timeline's facts; a change of the shown model drops the weekly quota read for
    /// the old one.
    fn note_facts(
        &mut self,
        leaf_index: usize,
        session: u64,
        facts: &FooterFacts,
        card: Option<&aterm_phase::LaunchCard>,
        usage: bool,
        off: FactMask,
    ) {
        if self
            .footer(leaf_index, session)
            .note_facts(facts, card, usage, off)
            && let Some(Some(cached)) = self.panes.get_mut(leaf_index)
        {
            cached.weekly.take();
        }
    }

    /// Leaf `leaf_index`'s footer as this frame gathered it.
    fn footer_mut(&mut self, leaf_index: usize) -> Option<&mut PaneFooter> {
        self.footers.get_mut(leaf_index)
    }

    /// The cells of leaf `leaf_index`'s composed rule ([`PaneFooter::compose`]).
    fn composed(&self, leaf_index: usize) -> &[RenderCell] {
        self.footers
            .get(leaf_index)
            .map_or(&[], |f| f.rule.paint.cells.as_slice())
    }

    /// Returns the parsed launch card for `leaf_index` while its cached rows belong to `session`.
    pub(crate) fn launch_card(
        &self,
        leaf_index: usize,
        session: u64,
    ) -> Option<(Arc<crate::claude_card::CardRead>, u64)> {
        let cached = self.panes.get(leaf_index)?.as_ref()?;
        if cached.key.session != session {
            return None;
        }
        let read = cached
            .card
            .get_or_init(|| {
                crate::claude_card::read_card(session, &cached.rows, cached.card_before.take())
            })
            .read
            .clone()?;
        Some((read, cached.text_gen))
    }

    /// Returns the parsed agents panel beneath `bottom` for `leaf_index` while its rows belong to
    /// `session`.
    pub(crate) fn agents_panel(
        &self,
        leaf_index: usize,
        session: u64,
        bottom: usize,
        scratch: &RenderInput,
        pane: crate::claude_card::PaneGeom,
        blank: RenderCell,
    ) -> Option<Arc<crate::claude_panel::PanelRead>> {
        let cached = self.panes.get(leaf_index)?.as_ref()?;
        if cached.key.session != session {
            return None;
        }
        cached
            .panel
            .get_or_init(|| {
                let read = crate::claude_panel::read_panel(
                    session,
                    &cached.rows,
                    bottom,
                    scratch,
                    pane,
                    blank,
                )?;
                match cached.panel_before.take() {
                    Some(before) if *before == read => Some(before),
                    _ => Some(Arc::new(read)),
                }
            })
            .clone()
    }

    /// Returns the parsed update notice beneath the launch card at `place` for `leaf_index`.
    pub(crate) fn update_notice(
        &self,
        leaf_index: usize,
        session: u64,
        place: &aterm_phase::CardPlace,
        cols: usize,
    ) -> Option<aterm_phase::NoticePlace> {
        let cached = self.panes.get(leaf_index)?.as_ref()?;
        if cached.key.session != session {
            return None;
        }
        let from = place.top.saturating_add(place.rows);
        if let Some((at, scan)) = cached.notice.get()
            && at == from
        {
            return scan.and_then(|scan| scan.place);
        }
        #[cfg(test)]
        crate::work_counts::card_notice_read();
        let scan =
            crate::reader_guard::read_or_none(session, "harness notice", &cached.rows, || {
                aterm_phase::update_notice_scan(&cached.rows, place, cols)
            });
        cached.notice.set(Some((from, scan)));
        scan.and_then(|scan| scan.place)
    }
}

/// One pane's footer as earlier frames composed it (a leaf's, in [`PaneTextCache`]).
#[derive(Debug, Default)]
struct PaneFooter {
    session: u64,
    /// What [`Self::shown`] is made of: the facts the timeline holds for the group in front, the
    /// launch card it keeps for them, the usage switch and the facts the person hid.
    from: Option<(FooterFacts, Option<aterm_phase::LaunchCard>, bool, FactMask)>,
    /// The live upgrade's note the timeline holds for the pane ([`Self::lay_upgrade`]).
    note: Option<footer::UpgradeNote>,
    /// What the rule shows ([`with_usage_switch`]).
    shown: FooterFacts,
    /// Bumped on every change of `shown`.
    shown_gen: u64,
    /// The repaint key's term for `from` ([`fingerprint`], [`Self::fingerprint_of`]).
    fp: u64,
    /// `footer::chips_room` of `shown` at `room` cells.
    chips: Option<(u64, usize, usize)>,
    /// The rule composed from `shown`.
    rule: ComposedRule,
    /// The composer's top rule as the frames before composed it.
    top: ComposedTop,
    /// Whether `shown` names a Claude Code build that takes the top-effort key's toggle
    /// ([`Self::key_typed`]).
    typed: Option<(u64, bool)>,
}

/// A footer rule as [`PaneFooter::compose`] composed it, and what it was composed from.
#[derive(Debug, Default)]
struct ComposedRule {
    key: Option<RuleKey>,
    /// The chips' block it was composed with (`None`: no block), copied.
    block: Option<BlockCopy>,
    paint: RulePaint,
}

/// A composed footer's cells and native text side channels, kept together.
#[derive(Debug, Default)]
struct RulePaint {
    cells: Vec<RenderCell>,
    lights_at: Option<usize>,
    /// The title slot's words in `cells` (empty: none drawn).
    title: std::ops::Range<usize>,
    glass: std::ops::Range<usize>,
    /// Where the facts' cells start in `cells` ([`FactWrite`]).
    facts_at: usize,
    spans: footer::FactSpans,
    account: Option<(u16, u16)>,
    /// The facts' native emoji clusters and combining marks at their columns in `cells`
    /// ([`FooterUndo::publish_text`]).
    clusters: Vec<(usize, Box<str>)>,
    combining: Vec<(usize, Box<[char]>)>,
}

/// What a footer rule is composed from, beside the chips' block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RuleKey {
    shown_gen: u64,
    room: usize,
    rule: RenderCell,
    blank: RenderCell,
    mark_fg: [u8; 3],
    /// The fact the pointer or the keyboard's selection is on, drawn lit.
    lit: Option<FactKind>,
}

/// Every part of a chips' block a composition reads.
#[derive(Debug, Default)]
struct BlockCopy {
    lights: Vec<RenderCell>,
    title: Vec<RenderCell>,
    lean: [Vec<RenderCell>; crate::claude_lights::LEAN_FORMS],
    short_title: Vec<RenderCell>,
    reason: bool,
    holds: usize,
    fit: u64,
}

impl BlockCopy {
    /// Whether `block` is this copy's.
    fn is(&mut self, block: &crate::claude_lights::Block) -> bool {
        if block.fit() != 0 && block.fit() == self.fit {
            return true;
        }
        let same = self.lights == block.lights
            && self.title == block.title
            && self.lean == block.lean
            && self.short_title == block.short_title
            && self.reason == block.reason
            && self.holds == block.holds;
        if same {
            self.fit = block.fit();
        }
        same
    }

    fn copy(&mut self, block: &crate::claude_lights::Block) {
        self.lights.clone_from(&block.lights);
        self.title.clone_from(&block.title);
        for (kept, lean) in self.lean.iter_mut().zip(&block.lean) {
            kept.clone_from(lean);
        }
        self.short_title.clone_from(&block.short_title);
        self.reason = block.reason;
        self.holds = block.holds;
        self.fit = block.fit();
    }
}

impl PaneFooter {
    /// Take in the facts the timeline holds; say whether the shown model changed.
    fn note_facts(
        &mut self,
        facts: &FooterFacts,
        card: Option<&aterm_phase::LaunchCard>,
        usage: bool,
        off: FactMask,
    ) -> bool {
        if let Some((was, kept, switch, hid)) = &self.from
            && was == facts
            && kept.as_ref() == card
            && *switch == usage
            && *hid == off
        {
            return false;
        }
        let mut shown = facts.filled_from(card);
        with_usage_switch(&mut shown, usage);
        shown.off = off;
        shown.upgrade = self.shown.upgrade.take();
        let model_changed = shown.model != self.shown.model;
        self.shown = shown;
        self.shown_gen += 1;
        self.fp = fingerprint(Some(facts), card, usage, off);
        self.from = Some((facts.clone(), card.cloned(), usage, off));
        model_changed
    }

    /// The repaint key's term ([`fingerprint`]).
    fn fingerprint_of(
        &self,
        session: u64,
        facts: &FooterFacts,
        card: Option<&aterm_phase::LaunchCard>,
        usage: bool,
        off: FactMask,
    ) -> Option<u64> {
        let (was, kept, switch, hid) = self.from.as_ref()?;
        (self.session == session
            && was == facts
            && kept.as_ref() == card
            && *switch == usage
            && *hid == off)
            .then_some(self.fp)
    }

    /// Take in the live upgrade's note the timeline holds for the pane.
    fn note_upgrade(&mut self, note: Option<&footer::UpgradeNote>) {
        if self.note.as_ref() != note {
            self.note = note.cloned();
        }
    }

    /// Lay in the upgrade's words for a composer that holds a draft (`Some(true)`) or none
    /// (`Some(false)`).
    fn lay_upgrade(&mut self, draft: Option<bool>) {
        let words = draft.and_then(|draft| self.note.as_ref()?.words(draft));
        if self.shown.upgrade.as_ref() != words {
            self.shown.upgrade = words.cloned();
            self.shown_gen += 1;
        }
    }

    /// The facts the rule shows.
    const fn shown(&self) -> &FooterFacts {
        &self.shown
    }

    /// Whether the facts shown name a Claude Code build that takes the top-effort key's toggle
    /// ([`crate::claude_slash::takes_key_toggle`]), read once per change of `shown`.
    fn key_typed(&mut self) -> bool {
        match self.typed {
            Some((at, typed)) if at == self.shown_gen => typed,
            _ => {
                let typed = self
                    .shown
                    .version
                    .as_deref()
                    .is_some_and(crate::claude_slash::takes_key_toggle);
                self.typed = Some((self.shown_gen, typed));
                typed
            }
        }
    }

    /// The facts the timeline holds for the group in front.
    fn published(&self) -> Option<&FooterFacts> {
        self.from.as_ref().map(|(facts, ..)| facts)
    }

    /// The facts' owner where they leave the model or the effort open.
    fn card_owner(&self) -> Option<&footer::FactsOwner> {
        let (facts, _, _, _) = self.from.as_ref()?;
        (facts.model_open || facts.effort_open)
            .then_some(facts.owner.as_ref())
            .flatten()
    }

    /// The launch card the timeline keeps for the facts, as this frame read it.
    fn kept_card(&self) -> Option<&aterm_phase::LaunchCard> {
        self.from.as_ref()?.1.as_ref()
    }

    /// The most cells the chips may take in a rule of `room` cells.
    fn chips_room(&mut self, room: usize) -> usize {
        match self.chips {
            Some((shown_gen, at, chips)) if shown_gen == self.shown_gen && at == room => chips,
            _ => {
                let chips = footer::chips_room(&self.shown, room);
                self.chips = Some((self.shown_gen, room, chips));
                chips
            }
        }
    }

    /// The composer's top line composed from `key` ([`compose_top`], [`mark_layer`]).
    fn compose_top(&mut self, key: TopKey, marks: &mut MarkContents) -> &ComposedTop {
        if self.top.key != Some(key) {
            let version = (key.title_gen != 0)
                .then_some(self.shown.version.as_deref())
                .flatten();
            compose_top(&key, &mut self.top, version);
            self.top.mark = mark_layer(&key, &self.top, self.session, marks);
            self.top.key = Some(key);
        }
        &self.top
    }

    /// The rule's composition in `room` cells with the chips' `block` ([`compose_rule_spans`]).
    fn compose(
        &mut self,
        room: usize,
        block: Option<&crate::claude_lights::Block>,
        rule: RenderCell,
        blank: RenderCell,
        mark_fg: [u8; 3],
        lit: Option<FactKind>,
    ) -> Composed {
        let key = RuleKey {
            shown_gen: self.shown_gen,
            room,
            rule,
            blank,
            mark_fg,
            lit,
        };
        let composed = &mut self.rule;
        let same_block = match (&mut composed.block, block) {
            (None, None) => true,
            (Some(kept), Some(block)) => kept.is(block),
            _ => false,
        };
        if composed.key != Some(key) || !same_block {
            #[cfg(test)]
            crate::work_counts::footer_rule_composed();
            let widths = block.map(|b| footer::LightsWidth {
                chips: b.holds,
                title: b.title.len(),
                lean: b.lean.each_ref().map(Vec::len),
                short: b.short_title.len(),
                reason: b.reason,
            });
            let fit = footer::fit_rule(&self.shown, room, widths);
            composed.key = Some(key);
            composed.paint = compose_rule_spans(&fit, block, rule, blank, mark_fg, room, lit);
            match block {
                Some(block) => composed
                    .block
                    .get_or_insert_with(BlockCopy::default)
                    .copy(block),
                None => composed.block = None,
            }
        }
        Composed {
            cells: composed.paint.cells.len(),
            lights_at: composed.paint.lights_at,
            title: composed.paint.title.clone(),
            glass: composed.paint.glass.clone(),
            facts_at: composed.paint.facts_at,
            spans: composed.paint.spans,
            account: composed.paint.account,
        }
    }
}

/// What a frame reads of a pane's composed rule ([`PaneFooter::compose`]).
#[derive(Debug, Clone)]
struct Composed {
    cells: usize,
    lights_at: Option<usize>,
    /// The title slot's words in the cells (empty: none drawn).
    title: std::ops::Range<usize>,
    glass: std::ops::Range<usize>,
    facts_at: usize,
    spans: footer::FactSpans,
    account: Option<(u16, u16)>,
}

/// Whether `text` reads as `cells`' chars.
fn same_text(text: &str, cells: Option<&[RenderCell]>) -> bool {
    let Some(cells) = cells else {
        return text.is_empty();
    };
    let mut rest = text.as_bytes();
    for cell in cells {
        let ch = u32::from(cell.ch);
        if ch < 0x80 {
            match rest.split_first() {
                Some((&b, after)) if u32::from(b) == ch => rest = after,
                _ => return false,
            }
        } else {
            let mut utf8 = [0; 4];
            match rest.strip_prefix(cell.ch.encode_utf8(&mut utf8).as_bytes()) {
                Some(after) => rest = after,
                None => return false,
            }
        }
    }
    rest.is_empty()
}

/// The loop proxy [`post_changed`] wakes through.
static PROXY: OnceLock<EventLoopProxy<Wake>> = OnceLock::new();

/// Installs the event-loop proxy used to post `Wake::ClaudeFooter` notifications.
pub(crate) fn install_proxy(proxy: EventLoopProxy<Wake>) {
    let _ = PROXY.set(proxy);
}

/// Posts a `Wake::ClaudeFooter` notification when `session`'s footer facts change.
pub(crate) fn post_changed(session: u64) {
    if let Some(proxy) = PROXY.get() {
        let _ = proxy.send_event(Wake::ClaudeFooter { session });
    }
}

/// The follow-up reads after every request, measured from it ([`crate::claude_registry`]).
const FOLLOW_UPS: [Duration; 2] = [Duration::from_secs(2), Duration::from_secs(6)];

/// Why one read ran.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Cause {
    /// A request ([`request_footer`]).
    Ask,
    /// One of [`FOLLOW_UPS`].
    FollowUp,
    /// The registry watch saw the process's Claude dir move ([`Command::RowsMoved`]).
    Event,
    /// A registry watch was newly placed for the process's Claude dir.
    Placed,
    /// The idle clock ([`IDLE_RECHECK`], [`WALL_RECHECK`]).
    Idle,
}

impl Cause {
    /// The word the dev row prints.
    const fn word(self) -> &'static str {
        match self {
            Cause::Ask => "ask",
            Cause::FollowUp => "follow-up",
            Cause::Event => "event",
            Cause::Placed => "placed",
            Cause::Idle => "idle",
        }
    }
}

/// How many times one turn places registry watches and reads what they were placed for
/// ([`Resolver::settle`]).
const PLACE_ROUNDS: usize = 8;

/// How often a Claude Code session's facts are re-read while nothing asks.
const IDLE_RECHECK: Duration = Duration::from_secs(15);
const WATCH_FOR: Duration = Duration::from_secs(10 * 60);

/// How often a session is re-read past [`WATCH_FOR`] while its footer shows a limit wall or weekly
/// percentage and its Claude Code process lives.
const WALL_RECHECK: Duration = Duration::from_secs(60);

/// How long the resolver's roster answers from its last read ([`RosterMemo`]).
const ROSTER_RECHECK: Duration = Duration::from_secs(10);

/// The re-read interval of a session last asked for at `asked`.
fn recheck_after(now: std::time::Instant, asked: std::time::Instant) -> Duration {
    if now.saturating_duration_since(asked) < WATCH_FOR {
        IDLE_RECHECK
    } else {
        WALL_RECHECK
    }
}

/// Whether a session last asked for at `asked` is still watched ([`WATCH_FOR`]).
fn still_watched(
    now: std::time::Instant,
    asked: std::time::Instant,
    walled_alive: impl FnOnce() -> bool,
) -> bool {
    now.saturating_duration_since(asked) < WATCH_FOR || walled_alive()
}

/// Coalesced transcript-step wake helper for notifying session watchers.
#[derive(Clone, Default)]
pub(crate) struct LogWake {
    clause: bool,
}

impl LogWake {
    /// Constructs a [`LogWake`] controlled by `clause`.
    pub(crate) const fn new(clause: bool) -> LogWake {
        LogWake { clause }
    }

    /// Rings `session`'s watchers if `applied` transcript steps changed footer-visible facts.
    pub(crate) fn after(&self, session: u64, applied: &crate::session_log::transcript::Applied) {
        if (applied.owed && self.clause) || applied.settle.is_some() {
            post_log_moved(session);
        }
    }
}

/// A session's log moved off the loop in a way the loop must follow.
fn post_log_moved(session: u64) {
    if let Some(proxy) = PROXY.get() {
        let _ = proxy.send_event(Wake::SessionLogMoved { session });
    }
}

/// One resolution: the session, its timeline and the Claude Code process, and what a moved log
/// wakes ([`LogWake`]).
#[derive(Clone)]
struct Job {
    session: u64,
    timeline: Arc<Mutex<SessionTimeline>>,
    pgid: i32,
    wake: LogWake,
}

enum Command {
    Request(Job),
    /// A `None` pgid retires the session.
    Stop {
        session: u64,
        pgid: Option<i32>,
    },
    /// The registry watch saw the Claude dir `dir`'s rows move ([`crate::claude_registry`]).
    RowsMoved {
        dir: PathBuf,
    },
    /// The registry watch saw a level outside a `sessions/` move ([`Resolver::settle`]).
    LevelMoved,
}

impl From<crate::claude_registry::Told> for Command {
    fn from(told: crate::claude_registry::Told) -> Command {
        match told {
            crate::claude_registry::Told::Rows(dir) => Command::RowsMoved { dir },
            crate::claude_registry::Told::Level => Command::LevelMoved,
        }
    }
}

/// The one background thread footer facts are read on, started on first use and shared by every
/// asker.
static RESOLVER: Mutex<Option<Sender<Command>>> = Mutex::new(None);

/// Requests an off-thread footer-fact refresh for `pgid` into `timeline`.
pub(crate) fn request_footer(
    session: u64,
    timeline: &Arc<Mutex<SessionTimeline>>,
    pgid: i32,
    wake: &LogWake,
) {
    if pgid <= 0 {
        return;
    }
    #[cfg(test)]
    if CAPTURED_ASKS.with(|captured| {
        captured
            .borrow_mut()
            .as_mut()
            .map(|asks| asks.push((session, pgid)))
            .is_some()
    }) {
        return;
    }
    let mut slot = RESOLVER.lock().unwrap_or_else(|p| p.into_inner());
    if slot.is_none() {
        let (tx, rx) = channel::<Command>();
        let rows = tx.clone();
        let sink: crate::claude_registry::Sink =
            Arc::new(move |told| rows.send(Command::from(told)).is_ok());
        let spawned = crate::qos::spawn_lane_thread(
            "aterm-claude-footer",
            crate::qos::Role::Responsive,
            move || {
                run(
                    &rx,
                    crate::claude_registry::RegistryWatch::new(atpkg::dirwatch::Watcher::new, sink),
                );
            },
        );
        match spawned {
            Ok(_) => *slot = Some(tx),
            Err(e) => {
                aterm_log::warn!("claude footer: could not start the resolver: {e}");
                return;
            }
        }
    }
    let job = Job {
        session,
        timeline: Arc::clone(timeline),
        pgid,
        wake: wake.clone(),
    };
    if slot
        .as_ref()
        .is_some_and(|tx| tx.send(Command::Request(job)).is_err())
    {
        *slot = None;
    }
}

/// Stops background footer refreshes for foreground process group `pgid` on `session`.
pub(crate) fn stop(session: u64, pgid: i32) {
    if pgid <= 0 {
        return;
    }
    send_stop(session, Some(pgid));
}

/// Drops all resolver state, watches, and caches held for a retired `session`.
pub(crate) fn stop_session(session: u64) {
    #[cfg(test)]
    STOPPED_SESSIONS.with(|s| s.borrow_mut().push(session));
    send_stop(session, None);
}

#[cfg(test)]
thread_local! {
    /// Thread-local log of sessions passed to [`stop_session`] in tests.
    pub(crate) static STOPPED_SESSIONS: std::cell::RefCell<Vec<u64>> =
        const { std::cell::RefCell::new(Vec::new()) };
    /// Thread-local capture buffer for `(session, pgid)` requests passed to [`request_footer`] in
    /// tests.
    pub(crate) static CAPTURED_ASKS: std::cell::RefCell<Option<Vec<(u64, i32)>>> =
        const { std::cell::RefCell::new(None) };
}

fn send_stop(session: u64, pgid: Option<i32>) {
    let mut slot = RESOLVER.lock().unwrap_or_else(|p| p.into_inner());
    if slot
        .as_ref()
        .is_some_and(|tx| tx.send(Command::Stop { session, pgid }).is_err())
    {
        *slot = None;
    }
}

#[derive(Default)]
struct WatchSchedule {
    owed: Vec<(std::time::Instant, Job)>,
    watched: HashMap<u64, Watch>,
}

/// One watched session: its latest job, when it was last asked for and last read
/// ([`IDLE_RECHECK`]), and whether its process is awaited ([`crate::claude_registry`]).
struct Watch {
    job: Job,
    asked: std::time::Instant,
    read: std::time::Instant,
    awaiting: bool,
}

/// What one batch of commands asks of the resolver ([`WatchSchedule::accept`]).
#[derive(Default)]
struct Accepted {
    /// The newest ask per session, to read now.
    latest: Vec<Job>,
    /// The watched sessions a stop retired.
    stopped: Vec<u64>,
    /// The Claude dirs whose rows the registry watch saw move.
    moved: HashSet<PathBuf>,
}

impl WatchSchedule {
    /// Drop the watches whose time is up: [`WATCH_FOR`] after the last ask, unless `walled_alive`
    /// says the session's footer shows a wall or weekly percentage and its process lives
    /// ([`still_watched`]).
    fn prune(&mut self, now: std::time::Instant, mut walled_alive: impl FnMut(u64, &Job) -> bool) {
        self.watched.retain(|session, watch| {
            still_watched(now, watch.asked, || walled_alive(*session, &watch.job))
        });
    }

    /// When the next read is owed on a clock.
    fn next(&self, now: std::time::Instant) -> Option<std::time::Instant> {
        self.owed
            .iter()
            .map(|(at, _)| *at)
            .chain(
                self.watched
                    .values()
                    .map(|watch| watch.read + recheck_after(now, watch.asked)),
            )
            .min()
    }

    fn matches(job: &Job, session: u64, pgid: Option<i32>) -> bool {
        job.session == session && pgid.is_none_or(|pgid| job.pgid == pgid)
    }

    /// Collapse queued requests while respecting a stop's position in the channel.
    fn accept(&mut self, commands: impl IntoIterator<Item = Command>) -> Accepted {
        let mut latest: HashMap<u64, Job> = HashMap::new();
        let mut accepted = Accepted::default();
        for command in commands {
            match command {
                Command::Request(job) => {
                    latest.insert(job.session, job);
                }
                Command::Stop { session, pgid } => {
                    if latest
                        .get(&session)
                        .is_some_and(|job| Self::matches(job, session, pgid))
                    {
                        latest.remove(&session);
                    }
                    if self.stop(session, pgid) {
                        accepted.stopped.push(session);
                    }
                }
                Command::RowsMoved { dir } => {
                    accepted.moved.insert(dir);
                }
                Command::LevelMoved => {}
            }
        }
        accepted.latest = latest.into_values().collect();
        accepted
    }

    fn stop(&mut self, session: u64, pgid: Option<i32>) -> bool {
        if !self
            .watched
            .get(&session)
            .is_some_and(|watch| Self::matches(&watch.job, session, pgid))
        {
            return false;
        }
        self.watched.remove(&session);
        self.owed.retain(|(_, job)| job.session != session);
        true
    }

    /// Watch `job`, asked for and read at `asked`, that read having found its process's registry
    /// row (`row`) or not.
    fn watch(&mut self, job: Job, asked: std::time::Instant, row: bool) {
        self.owed
            .retain(|(_, pending)| pending.session != job.session);
        for delay in FOLLOW_UPS {
            self.owed.push((asked + delay, job.clone()));
        }
        self.watched.insert(
            job.session,
            Watch {
                job,
                asked,
                read: asked,
                awaiting: !row,
            },
        );
    }

    fn due_followups(&mut self, now: std::time::Instant) -> Vec<Job> {
        let (due, later): (Vec<_>, Vec<_>) = self.owed.drain(..).partition(|(at, _)| *at <= now);
        self.owed = later;
        due.into_iter().map(|(_, job)| job).collect()
    }

    fn due_idle(&self, now: std::time::Instant) -> Option<Job> {
        self.watched
            .values()
            .find(|watch| now >= watch.read + recheck_after(now, watch.asked))
            .map(|watch| watch.job.clone())
    }

    /// The job of each session in `sessions` whose process is awaited, in session order.
    fn awaited_in(&self, sessions: &HashSet<u64>) -> Vec<Job> {
        let mut jobs: Vec<Job> = sessions
            .iter()
            .filter_map(|session| self.watched.get(session))
            .filter(|watch| watch.awaiting)
            .map(|watch| watch.job.clone())
            .collect();
        jobs.sort_by_key(|job| job.session);
        jobs
    }

    /// `session` was read at `at`, the read having found its process's registry row (`row`) or
    /// not.
    fn read(&mut self, session: u64, at: std::time::Instant, row: bool) {
        if let Some(watch) = self.watched.get_mut(&session) {
            watch.read = at;
            watch.awaiting = !row;
        }
    }

    /// Whether `job`'s process is awaited as a read of it starts.
    fn awaits(&self, job: &Job) -> bool {
        self.watched
            .get(&job.session)
            .is_none_or(|watch| watch.job.pgid != job.pgid || watch.awaiting)
    }

    /// One pass of the resolver thread ([`Resolver::turn`]).
    fn serve(
        &mut self,
        latest: Vec<Job>,
        moved: &HashSet<u64>,
        mut read: impl FnMut(&Job, Cause, bool) -> bool,
        mut clock: impl FnMut() -> std::time::Instant,
    ) -> std::time::Instant {
        let mut done: HashSet<u64> = HashSet::new();
        for job in latest {
            let row = read(&job, Cause::Ask, self.awaits(&job));
            done.insert(job.session);
            self.watch(job, clock(), row);
        }
        let now = clock();
        for job in self.due_followups(now) {
            let row = read(&job, Cause::FollowUp, self.awaits(&job));
            done.insert(job.session);
            self.read(job.session, clock(), row);
        }
        for job in self.awaited_in(moved) {
            if done.insert(job.session) {
                let row = read(&job, Cause::Event, true);
                self.read(job.session, clock(), row);
            }
        }
        while let Some(job) = self.due_idle(now) {
            let row = read(&job, Cause::Idle, self.awaits(&job));
            self.read(job.session, clock(), row);
        }
        now
    }
}

/// How many Claude Code processes the resolver keeps transcript caches for at once
/// ([`ProcessTails`]).
const MAX_KEPT: usize = 64;

/// How often the resolver asks the kernel whether the processes it keeps caches for, and no longer
/// watches, still live ([`ProcessTails`]).
const SWEEP_EVERY: Duration = Duration::from_secs(30);

/// The transcript caches of the Claude Code processes the resolver has read, keyed by process,
/// not by watch ([`WATCH_FOR`]).
#[derive(Default)]
struct ProcessTails {
    kept: HashMap<(i32, Option<u64>), KeptTail>,
    /// When the kernel was last asked ([`SWEEP_EVERY`]).
    swept: Option<std::time::Instant>,
}

/// One process's cache in [`ProcessTails`].
struct KeptTail {
    /// The session the process was last read for.
    session: u64,
    cache: footer::TailCache,
    /// When it was last read.
    used: std::time::Instant,
}

impl ProcessTails {
    /// The cache of the process (`pgid`, `started`), read now for `session`.
    fn cache(
        &mut self,
        session: u64,
        pgid: i32,
        started: Option<u64>,
        now: std::time::Instant,
    ) -> &mut footer::TailCache {
        let key = (pgid, started);
        if !self.kept.contains_key(&key)
            && self.kept.len() >= MAX_KEPT
            && let Some(oldest) = self
                .kept
                .iter()
                .min_by_key(|(_, kept)| kept.used)
                .map(|(key, _)| *key)
        {
            self.kept.remove(&oldest);
        }
        let kept = self.kept.entry(key).or_insert_with(|| KeptTail {
            session,
            cache: footer::TailCache::default(),
            used: now,
        });
        kept.session = session;
        kept.used = now;
        &mut kept.cache
    }

    /// Drop the caches whose process has left for good ([`SWEEP_EVERY`], [`birth_seconds`]).
    fn retain(
        &mut self,
        schedule: &WatchSchedule,
        alive: impl Fn(i32, u64) -> bool,
        now: std::time::Instant,
    ) {
        let sweep = self
            .swept
            .is_none_or(|at| now.saturating_duration_since(at) >= SWEEP_EVERY);
        if sweep {
            self.swept = Some(now);
        }
        self.kept.retain(|&(pgid, started), kept| {
            let watched = schedule
                .watched
                .get(&kept.session)
                .is_some_and(|watch| watch.job.pgid == pgid);
            watched || started.is_some_and(|started| !sweep || alive(pgid, started))
        });
    }
}

/// The resolver thread.
fn run(rx: &std::sync::mpsc::Receiver<Command>, registry: crate::claude_registry::RegistryWatch) {
    let mut resolver = Resolver::new(registry, LIVE);
    loop {
        let now = std::time::Instant::now();
        resolver.retain(now);
        let first = match resolver.next(now) {
            Some(at) => match rx.recv_timeout(at.saturating_duration_since(now)) {
                Ok(job) => Some(job),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => None,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
            },
            None => match rx.recv() {
                Ok(job) => Some(job),
                Err(_) => return,
            },
        };
        resolver.turn(
            first
                .into_iter()
                .chain(std::iter::from_fn(|| rx.try_recv().ok())),
            std::time::Instant::now,
        );
    }
}

/// The resolver thread's whole state ([`run`]).
struct Resolver {
    /// Per session: its latest job, when it was last asked for and last read ([`IDLE_RECHECK`]),
    /// and whether its process is awaited ([`Watch`]).
    schedule: WatchSchedule,
    known: HashMap<u64, Identity>,
    /// Per process: its transcript-tail memo, kept while the process lives.
    tails: ProcessTails,
    /// Per watched session: its usage fold.
    folds: HashMap<u64, footer::FooterCache>,
    /// Sessions whose footer has a wall or weekly percentage to expire on the slow clock.
    walled: HashSet<u64>,
    /// The accounts roster, for every session's label.
    roster: RosterMemo,
    registry: crate::claude_registry::RegistryWatch,
    asks: Asks,
}

impl Resolver {
    fn new(registry: crate::claude_registry::RegistryWatch, asks: Asks) -> Resolver {
        Resolver {
            schedule: WatchSchedule::default(),
            known: HashMap::new(),
            tails: ProcessTails::default(),
            folds: HashMap::new(),
            walled: HashSet::new(),
            roster: RosterMemo::default(),
            registry,
            asks,
        }
    }

    /// Before the thread sleeps at `now`: the watches whose time is up go, with what the reads
    /// hold for their sessions, the tails of processes gone, and the registry watches nothing
    /// awaits now.
    fn retain(&mut self, now: std::time::Instant) {
        let walled = &self.walled;
        self.schedule.prune(now, |session, job| {
            walled.contains(&session)
                && u32::try_from(job.pgid).is_ok_and(crate::control_auth::pid_alive)
        });
        let watched = &self.schedule.watched;
        self.known
            .retain(|session, _| watched.contains_key(session));
        self.folds
            .retain(|session, _| watched.contains_key(session));
        self.walled.retain(|session| watched.contains_key(session));
        self.tails.retain(
            &self.schedule,
            |pgid, started| birth_seconds(pgid) == Some(started),
            now,
        );
        self.registry.let_go(&self.awaited());
    }

    /// When the next clock read is owed ([`WatchSchedule::next`]).
    fn next(&self, now: std::time::Instant) -> Option<std::time::Instant> {
        self.schedule.next(now)
    }

    /// One turn over `commands` ([`forget`], [`ProcessTails`], [`WatchSchedule::serve`],
    /// [`Self::settle`]).
    fn turn(
        &mut self,
        commands: impl IntoIterator<Item = Command>,
        mut clock: impl FnMut() -> std::time::Instant,
    ) -> std::time::Instant {
        let now = self.serve_commands(commands, &mut clock);
        self.settle(&mut clock);
        now
    }

    /// [`Self::turn`]'s reads, before its registry watch is settled.
    fn serve_commands(
        &mut self,
        commands: impl IntoIterator<Item = Command>,
        clock: &mut impl FnMut() -> std::time::Instant,
    ) -> std::time::Instant {
        let accepted = self.schedule.accept(commands);
        for session in accepted.stopped {
            forget(session, &mut self.known, &mut self.folds, &mut self.walled);
        }
        let moved = self.sessions_under(&accepted.moved);
        let mut reads = Reads {
            known: &mut self.known,
            tails: &mut self.tails,
            folds: &mut self.folds,
            walled: &mut self.walled,
            roster: &mut self.roster,
            asks: self.asks,
        };
        self.schedule.serve(
            accepted.latest,
            &moved,
            |job, cause, awaited| reads.resolve(job, cause, awaited),
            clock,
        )
    }

    /// Place the registry watches the awaited processes need
    /// ([`crate::claude_registry::RegistryWatch::reconcile`], [`Cause::Placed`]).
    fn settle(&mut self, clock: &mut impl FnMut() -> std::time::Instant) {
        for _ in 0..PLACE_ROUNDS {
            let fresh: HashSet<PathBuf> = self
                .registry
                .reconcile(&self.awaited())
                .into_iter()
                .collect();
            if fresh.is_empty() {
                return;
            }
            let jobs = self.schedule.awaited_in(&self.sessions_under(&fresh));
            let mut reads = Reads {
                known: &mut self.known,
                tails: &mut self.tails,
                folds: &mut self.folds,
                walled: &mut self.walled,
                roster: &mut self.roster,
                asks: self.asks,
            };
            for job in jobs {
                let row = reads.resolve(&job, Cause::Placed, true);
                self.schedule.read(job.session, clock(), row);
            }
        }
    }

    /// The awaited processes whose Claude dir a read has resolved.
    fn awaited(&self) -> Vec<crate::claude_registry::Awaited> {
        let mut awaited: Vec<_> = self
            .schedule
            .watched
            .iter()
            .filter(|(_, watch)| watch.awaiting)
            .filter_map(|(session, watch)| {
                let identity = self
                    .known
                    .get(session)
                    .filter(|identity| identity.pid == watch.job.pgid)?;
                Some(crate::claude_registry::Awaited {
                    dir: identity.dir.clone()?,
                    pid: u32::try_from(watch.job.pgid).ok()?,
                })
            })
            .collect();
        awaited.sort_by(|a, b| (&a.dir, a.pid).cmp(&(&b.dir, b.pid)));
        awaited
    }

    /// The watched sessions whose process's Claude dir is one of `dirs`.
    fn sessions_under(&self, dirs: &HashSet<PathBuf>) -> HashSet<u64> {
        if dirs.is_empty() {
            return HashSet::new();
        }
        self.schedule
            .watched
            .iter()
            .filter(|(session, watch)| {
                self.known.get(session).is_some_and(|identity| {
                    identity.pid == watch.job.pgid
                        && identity.dir.as_ref().is_some_and(|dir| dirs.contains(dir))
                })
            })
            .map(|(session, _)| *session)
            .collect()
    }
}

/// The resolver's state one read touches.
struct Reads<'a> {
    known: &'a mut HashMap<u64, Identity>,
    tails: &'a mut ProcessTails,
    folds: &'a mut HashMap<u64, footer::FooterCache>,
    walled: &'a mut HashSet<u64>,
    roster: &'a mut RosterMemo,
    asks: Asks,
}

impl Reads<'_> {
    /// Read `job`'s session for `cause` and keep it in the walled set exactly while its published
    /// footer has a wall or weekly percentage.
    fn resolve(&mut self, job: &Job, cause: Cause, awaited: bool) -> bool {
        let found = resolve_and_post(
            self.known,
            self.tails,
            self.folds.entry(job.session).or_default(),
            self.roster,
            job,
            self.asks,
        );
        note_wall(self.walled, job.session, found.timed);
        if crate::holder_image::DEV_ROWS && awaited {
            let open = self
                .known
                .get(&job.session)
                .is_some_and(|identity| identity.pid == job.pgid);
            job.timeline
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .record_row(
                    REGISTRY_READ,
                    format!(
                        "cause={} pid={} open={} row={} {}",
                        cause.word(),
                        job.pgid,
                        u8::from(open),
                        u8::from(found.row),
                        crate::holder_image::dev_tail()
                    ),
                );
        }
        found.row
    }
}

/// Side-channel session metadata.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct FactSide {
    /// Current working directory of the session's Bash tool or launch environment.
    pub(crate) cwd: Option<PathBuf>,
    /// Git repository root containing [`Self::cwd`], if any.
    pub(crate) repo_root: Option<PathBuf>,
    /// Token traffic time-series view from the usage fold, if available.
    pub(crate) traffic: Option<Arc<aterm_agent::harness::session_usage::TrafficView>>,
    /// The fold and generation `traffic` was built at.
    built_at: Option<(u64, u64)>,
    /// Active subagents and workflow tool calls in flight from the usage fold.
    pub(crate) flights: Option<Arc<aterm_agent::harness::flight::FlightTable>>,
    /// The fold and generation `flights` was built at (`footer::FooterCache::flight_generation`).
    flights_at: Option<(u64, u64)>,
}

#[cfg(test)]
impl FactSide {
    /// Constructs a [`FactSide`] with `cwd` and `repo_root` and no traffic or flight data.
    pub(crate) fn at(cwd: Option<PathBuf>, repo_root: Option<PathBuf>) -> Self {
        Self {
            cwd,
            repo_root,
            traffic: None,
            built_at: None,
            flights: None,
            flights_at: None,
        }
    }

    /// Returns `self` with `flights` attached.
    pub(crate) fn with_flights(
        mut self,
        flights: aterm_agent::harness::flight::FlightTable,
    ) -> Self {
        self.flights = Some(Arc::new(flights));
        self
    }

    /// Returns `self` with `traffic` attached.
    #[cfg(unix)]
    pub(crate) fn with_traffic(
        mut self,
        traffic: aterm_agent::harness::session_usage::TrafficView,
    ) -> Self {
        self.traffic = Some(Arc::new(traffic));
        self
    }
}

/// What a [`side_after`] read found of the fold.
struct SideFold<T, F> {
    generation: Option<(u64, u64)>,
    traffic: T,
    flight_generation: Option<(u64, u64)>,
    flights: F,
}

/// The side for a read that found `cwd` and `repo_root` and a fold whose parts are at `fold`'s
/// generations.
fn side_after<T, F>(
    held: Option<&Arc<FactSide>>,
    cwd: Option<PathBuf>,
    repo_root: Option<PathBuf>,
    fold: SideFold<T, F>,
) -> Arc<FactSide>
where
    T: FnOnce() -> Option<aterm_agent::harness::session_usage::TrafficView>,
    F: FnOnce() -> Option<aterm_agent::harness::flight::FlightTable>,
{
    let SideFold {
        generation,
        traffic,
        flight_generation,
        flights,
    } = fold;
    if let Some(held) = held
        && held.cwd == cwd
        && held.repo_root == repo_root
        && held.built_at == generation
        && held.flights_at == flight_generation
    {
        return Arc::clone(held);
    }
    let traffic = match held {
        Some(held) if held.built_at == generation => held.traffic.clone(),
        _ => generation.and_then(|_| traffic()).map(Arc::new),
    };
    let flights = match held {
        Some(held) if held.flights_at == flight_generation => held.flights.clone(),
        _ => flight_generation.and_then(|_| flights()).map(Arc::new),
    };
    Arc::new(FactSide {
        cwd,
        repo_root,
        traffic,
        built_at: generation,
        flights,
        flights_at: flight_generation,
    })
}

/// Dev-timeline event name for a single awaited process registry read.
pub(crate) const REGISTRY_READ: &str = "registry-read";

/// What one read found, for the resolver's own state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Found {
    /// The process's registry row answered the read ([`footer::read_pid`] gave facts).
    row: bool,
    /// Its published wall or weekly percentage still needs clock-driven expiry.
    timed: bool,
}

/// Keep the slow clock while a footer value needs time-driven expiry.
fn note_wall(walled: &mut HashSet<u64>, session: u64, timed: bool) {
    if timed {
        walled.insert(session);
    } else {
        walled.remove(&session);
    }
}

/// Forget what the resolver's reads hold for `session` ([`stop_session`], [`stop`]).
fn forget(
    session: u64,
    known: &mut HashMap<u64, Identity>,
    folds: &mut HashMap<u64, footer::FooterCache>,
    walled: &mut HashSet<u64>,
) {
    known.remove(&session);
    folds.remove(&session);
    walled.remove(&session);
}

/// What the resolver knows about one Claude Code process for its whole life
/// ([`footer::launch_facts_of`], [`login_of_args`]).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Identity {
    pid: i32,
    started: Option<u64>,
    dir: Option<PathBuf>,
    launch: LaunchFacts,
    /// The image (`startedAt`) `launch`, `login` and `argv` were read for.
    launch_for: Option<Option<u64>>,
    /// What the process's exec environment says of its login.
    login: Option<accounts::ProcessLogin>,
    /// The process's argv ([`aterm_agent::harness::resume::of_entry`]).
    argv: Vec<String>,
    /// The head of the task a `spawn agent=` worker was started with, read once per image from the
    /// file its exec environment names ([`crate::survey::task_prompt_of_env`]).
    task: Option<String>,
    /// The image and settings directory the stored-login verdict belongs to.
    login_checked_for: Option<(Option<u64>, PathBuf)>,
    /// Settings, provider and profile-store evidence leave the stored login selected.
    stored_login: bool,
    /// Why no relaunch line of this image's argv in its directory would carry the usage tap,
    /// judged beside [`Self::stored_login`] ([`tap_line_refusal`]: its options and settings;
    /// its home and login are judged at each read, [`tap_refusal`]); `None` where one would.
    unoffered: Option<tap::Refusal>,
    /// Exact captured exec environment, fingerprinted without retaining credentials.
    env_stamp: Option<[u8; 32]>,
}

/// The kernel's start time of `pid`.
fn birth_seconds(pid: i32) -> Option<u64> {
    #[cfg(unix)]
    {
        crate::seamless::read_process_birth(pid).map(|birth| birth.seconds)
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        None
    }
}

/// `pid`'s argv and environment at exec, when this platform lets it be read.
fn process_args(pid: i32) -> Option<atpkg::caller_shell::ProcArgs> {
    u32::try_from(pid)
        .ok()
        .and_then(atpkg::caller_shell::process_args)
}

/// Resolves the Claude configuration directory from process exec arguments and environment.
pub(crate) fn claude_dir_of_args(
    args: Option<&atpkg::caller_shell::ProcArgs>,
    pid: i32,
) -> Option<PathBuf> {
    match args {
        Some(args) => footer::claude_dir_of(
            args.env_var(accounts::HOME_ENV),
            args.env_var(accounts::HOMEDIR_ENV),
            || {
                u32::try_from(pid)
                    .ok()
                    .and_then(aterm_sysprobe::process_cwd)
            },
        ),
        None => None,
    }
}

/// What a process's environment at exec says of its login
/// ([`accounts::ProcessLogin::of_exec_env`]).
fn login_of_args(args: Option<&atpkg::caller_shell::ProcArgs>) -> Option<accounts::ProcessLogin> {
    accounts::ProcessLogin::of_exec_env(&args?.env)
}

/// The value `name` has in exec environment `env` (`KEY=value` entries), where it is set.
fn env_value(env: &[String], name: &str) -> Option<String> {
    env.iter()
        .find_map(|entry| entry.strip_prefix(name)?.strip_prefix('='))
        .map(str::to_owned)
}

/// Reuse an image's auth verdict only for its exact captured environment.
fn exec_env_stamp(env: &[String]) -> [u8; 32] {
    let mut digest = aterm_digest::Sha256::new();
    for entry in env {
        digest.update(entry.len().to_le_bytes());
        digest.update(entry.as_bytes());
    }
    digest.finalize()
}

/// The accounts roster as the resolver last read it ([`accounts::footer_label`]).
#[derive(Default)]
struct RosterMemo {
    /// When `load` was last asked.
    read: Option<std::time::Instant>,
    /// What it answered.
    roster: Option<accounts::Roster>,
}

impl RosterMemo {
    /// The roster; `load` is asked at most every [`ROSTER_RECHECK`].
    fn at(
        &mut self,
        now: std::time::Instant,
        load: impl FnOnce() -> Option<accounts::Roster>,
    ) -> Option<&accounts::Roster> {
        if self
            .read
            .is_none_or(|at| now.saturating_duration_since(at) >= ROSTER_RECHECK)
        {
            self.read = Some(now);
            self.roster = load();
        }
        self.roster.as_ref()
    }
}

/// The roster beside the `aterm.toml` this window loads ([`accounts::default_roster_path`]).
fn live_roster() -> Option<accounts::Roster> {
    if cfg!(test) {
        return None;
    }
    let roster = accounts::Roster::load(&accounts::default_roster_path()?).ok()?;
    if roster.rows.is_empty() {
        return Some(roster);
    }
    let now = aterm_time::unix_secs().and_then(|now| i64::try_from(now).ok())?;
    accounts::Store::at(&crate::harness_host::harness_state()?)
        .bound(&roster, now)
        .ok()
}

/// What a read asks outside Claude Code's own files.
#[derive(Clone, Copy)]
struct Asks {
    args: fn(i32) -> Option<atpkg::caller_shell::ProcArgs>,
    files: fn(u32) -> Option<Vec<PathBuf>>,
    roster: fn() -> Option<accounts::Roster>,
    home: fn() -> Option<PathBuf>,
    stored_login: fn(u32, &[String], &str) -> bool,
    tap: TapAsks,
}

/// What a read asks of the usage tap ([`Asks::tap`]).
#[derive(Clone, Copy)]
struct TapAsks {
    /// The window's seat ([`TapSeat`]), asked at each read whose profile passed its gate.
    seat: fn() -> TapSeat,
    /// Whether the settings chain a relaunch line of the process's argv in its directory reads
    /// admits the tap (`upgrade_drive::tap_settings_admit`), asked once per image beside
    /// [`Asks::stored_login`].
    settings: fn(&[String], &str) -> bool,
    /// Whether aterm's restart in place is offered for process `pid` in tab `tab` now
    /// ([`crate::restart_act::offered_for`]), asked at a read whose footer would promise it.
    restart: fn(&str, u32) -> bool,
}

/// A window with no usage tap: no seat, every settings chain admitting it, every restart offered.
#[cfg(test)]
const NO_TAP: TapAsks = TapAsks {
    seat: TapSeat::default,
    settings: |_, _| true,
    restart: |_, _| true,
};

/// What the usage tap gives a read ([`TapAsks::seat`]).
#[derive(Clone, Debug, Default)]
struct TapSeat {
    /// `[harness] usage_tap` is on and the harness supervises Claude Code
    /// ([`crate::harness_host::usage_tap_on`]).
    on: bool,
    /// An aterm the tap's command can name ([`tap::stable_exe`]).
    named: bool,
    /// Where its readings are ([`tap::rl_dir_in`]).
    dir: Option<PathBuf>,
    /// The harness state whose accounts store holds this machine's own home's weeks and
    /// walls ([`own_stored_week`]) and its lone login (`tap::Armed::lone_login`); `None`
    /// where the home read is not this machine's own.
    store: Option<PathBuf>,
}

/// [`TapSeat`] as this window has it now: its readings in the directory `aterm harness tap`
/// writes them to ([`tap::rl_dir`]).
fn live_tap() -> TapSeat {
    let state = crate::harness_host::harness_state();
    let own_home = aterm_types::dirs::home_dir()
        .is_some_and(|home| aterm_primer::home_dir().as_ref() == Some(&home));
    TapSeat {
        on: crate::harness_host::usage_tap_on(),
        named: tap::stable_exe().is_some(),
        dir: state.as_deref().map(tap::rl_dir_in),
        store: state.filter(|_| own_home),
    }
}

/// The process's current descriptor paths, read in process without a subprocess.
fn process_files(pid: u32) -> Option<Vec<PathBuf>> {
    if cfg!(test) {
        return None;
    }
    aterm_uds::holders::open_files(pid).ok()
}

/// [`process_args`], [`process_files`], [`live_roster`], [`aterm_types::dirs::home_dir`] and
/// [`live_tap`].
const LIVE: Asks = Asks {
    files: process_files,
    args: process_args,
    roster: live_roster,
    home: aterm_types::dirs::home_dir,
    stored_login: crate::harness_netprobe::stored_login_of_agent,
    tap: TapAsks {
        seat: live_tap,
        settings: crate::harness_netprobe::tap_settings_admit_line,
        restart: crate::restart_act::offered_for,
    },
};

/// [`resolve_into`], then wake the loop when the facts moved.
fn resolve_and_post(
    known: &mut HashMap<u64, Identity>,
    tails: &mut ProcessTails,
    fold: &mut footer::FooterCache,
    roster: &mut RosterMemo,
    job: &Job,
    asks: Asks,
) -> Found {
    let (session, timeline, pgid) = (job.session, &job.timeline, job.pgid);
    let started = birth_seconds(pgid);
    let cached = known
        .get(&session)
        .is_some_and(|id| id.pid == pgid && id.started.is_some() && id.started == started)
        .then(|| known.remove(&session))
        .flatten();
    let mut identity = cached.unwrap_or_else(|| Identity {
        pid: pgid,
        started,
        dir: None,
        launch: LaunchFacts::default(),
        launch_for: None,
        login: None,
        argv: Vec::new(),
        task: None,
        login_checked_for: None,
        stored_login: false,
        unoffered: None,
        env_stamp: None,
    });
    if identity.launch_for.is_none() {
        identity.dir = claude_dir_of_args((asks.args)(pgid).as_ref(), pgid);
    }
    let tail = tails.cache(
        session,
        identity.pid,
        identity.started,
        std::time::Instant::now(),
    );
    let (moved, found, logged) = resolve_into(timeline, &mut identity, tail, fold, roster, asks);
    job.wake.after(session, &logged);
    if identity.dir.is_some() {
        known.insert(session, identity);
    }
    if let Some(denied) = moved {
        post_changed(session);
        if let Some(path) = denied
            && let Some(proxy) = PROXY.get()
        {
            let _ = proxy.send_event(Wake::ProtectedRead { session, path });
        }
    }
    found
}

/// A zone's offset from UTC at the instant `unix`.
fn offset_at_cached(zone: Option<&str>, unix: i64) -> Option<i64> {
    static ANSWERS: Mutex<Vec<OffsetAnswer>> = Mutex::new(Vec::new());
    let mut answers = ANSWERS.lock().unwrap_or_else(|p| p.into_inner());
    cached_offset(
        &mut answers,
        zone,
        unix,
        std::time::Instant::now(),
        aterm_agent::supervise::limit::offset_at,
    )
}

/// One remembered offset.
type OffsetAnswer = (Option<String>, i64, std::time::Instant, i64);

/// [`offset_at_cached`] over `answers` at `now`, asking `read` on a miss.
fn cached_offset(
    answers: &mut Vec<OffsetAnswer>,
    zone: Option<&str>,
    unix: i64,
    now: std::time::Instant,
    read: impl FnOnce(Option<&str>, i64) -> Option<i64>,
) -> Option<i64> {
    const FRESH: Duration = Duration::from_secs(3600);
    const ANSWERS_MAX: usize = 64;
    if let Some(hit) = answers
        .iter()
        .find(|(z, t, at, _)| {
            z.as_deref() == zone && *t == unix && now.saturating_duration_since(*at) < FRESH
        })
        .map(|(_, _, _, offset)| *offset)
    {
        return Some(hit);
    }
    let offset = read(zone, unix)?;
    answers.retain(|(z, t, _, _)| !(z.as_deref() == zone && *t == unix));
    if answers.len() >= ANSWERS_MAX {
        answers.remove(0);
    }
    answers.push((zone.map(str::to_owned), unix, now, offset));
    Some(offset)
}

/// Read `identity`'s facts and publish them on `timeline`; the first part is `Some` when they
/// moved.
fn resolve_into(
    timeline: &Arc<Mutex<SessionTimeline>>,
    identity: &mut Identity,
    tail: &mut footer::TailCache,
    fold: &mut footer::FooterCache,
    roster: &mut RosterMemo,
    asks: Asks,
) -> (
    Option<Option<PathBuf>>,
    Found,
    crate::session_log::transcript::Applied,
) {
    // The session log's step tap is armed before the read while the log holds a run that reads
    // a transcript, with that run's watermark and floor, under one short hold of the (leaf)
    // timeline lock released before any file is read. A tap still waiting for its file is armed
    // again, so the run in front now gives it its floor and watermark.
    if !fold.steps_armed() || !fold.steps_bound() {
        let arm = timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .slog()
            .transcript_arm(crate::session_log::transcript::Source::Transcript);
        if let Some(arm) = arm {
            fold.set_floor(arm.floor_ms, arm.given);
        }
    }
    let (moved, found) = resolve_facts(timeline, identity, tail, fold, roster, asks);
    let logged = if fold.steps_armed() {
        let batch = fold.take_steps();
        let applied = crate::session_log::transcript::apply_batch(
            timeline,
            crate::session_log::transcript::Source::Transcript,
            &batch,
            fold.steps_bound(),
        );
        if applied.disarm {
            fold.disarm_steps();
        }
        applied
    } else {
        crate::session_log::transcript::Applied::default()
    };
    (moved, found, logged)
}

/// [`resolve_into`]'s footer half.
fn resolve_facts(
    timeline: &Arc<Mutex<SessionTimeline>>,
    identity: &mut Identity,
    tail: &mut footer::TailCache,
    fold: &mut footer::FooterCache,
    roster: &mut RosterMemo,
    asks: Asks,
) -> (Option<Option<PathBuf>>, Found) {
    let clock = footer::ReadClock {
        read_at: std::time::SystemTime::now(),
        now: aterm_agent::supervise::limit::unix_now(),
        offset_at: &offset_at_cached,
    };
    resolve_facts_at(timeline, identity, tail, fold, roster, asks, &clock)
}

/// The same worker read with an explicit clock.
fn resolve_facts_at(
    timeline: &Arc<Mutex<SessionTimeline>>,
    identity: &mut Identity,
    tail: &mut footer::TailCache,
    fold: &mut footer::FooterCache,
    roster: &mut RosterMemo,
    asks: Asks,
    clock: &footer::ReadClock<'_>,
) -> (Option<Option<PathBuf>>, Found) {
    resolve_facts_at_with_cwd(
        timeline,
        identity,
        tail,
        fold,
        roster,
        (asks, aterm_sysprobe::process_cwd),
        clock,
    )
}

/// [`resolve_facts_at`] with the process-cwd reader passed in.
fn resolve_facts_at_with_cwd(
    timeline: &Arc<Mutex<SessionTimeline>>,
    identity: &mut Identity,
    tail: &mut footer::TailCache,
    fold: &mut footer::FooterCache,
    roster: &mut RosterMemo,
    capabilities: (Asks, impl Fn(u32) -> Option<PathBuf>),
    clock: &footer::ReadClock<'_>,
) -> (Option<Option<PathBuf>>, Found) {
    let (asks, cwd_of) = capabilities;
    let prog = timeline
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .agent()
        .program
        .as_deref()
        .and_then(aterm_phase::program_of);
    let (is_claude, uses_claude_registry) = match prog {
        Some(aterm_phase::Program::Claude) => (true, true),
        Some(aterm_phase::Program::Agy | aterm_phase::Program::Muse) => (false, false),
        Some(
            aterm_phase::Program::Codex
            | aterm_phase::Program::Grok
            | aterm_phase::Program::Kimi
            | aterm_phase::Program::Gemini
            | aterm_phase::Program::Generic,
        )
        | None => (false, true),
    };
    let started = identity.started;
    let mut facts = uses_claude_registry
        .then(|| u32::try_from(identity.pid).ok().zip(identity.dir.clone()))
        .flatten()
        .and_then(|(pid, dir)| {
            let launch = |entry: &footer::SessionEntry| {
                if identity.launch_for != Some(entry.started_at) {
                    let args = (asks.args)(identity.pid);
                    identity.launch = args
                        .as_ref()
                        .map(|a| footer::launch_facts_of(&a.argv, &a.env))
                        .unwrap_or_default();
                    identity.login = login_of_args(args.as_ref());
                    identity.task = args
                        .as_ref()
                        .and_then(|a| crate::survey::task_prompt_of_env(&a.env));
                    if args.is_some() {
                        identity.launch_for = Some(entry.started_at);
                    }
                    identity.env_stamp = args.as_ref().map(|args| exec_env_stamp(&args.env));
                    identity.argv = args.map(|a| a.argv).unwrap_or_default();
                }
                if is_claude
                    && identity
                        .login_checked_for
                        .as_ref()
                        .is_none_or(|(image, cwd)| *image != entry.started_at || *cwd != entry.cwd)
                {
                    identity.stored_login = false;
                    if identity.launch_for == Some(entry.started_at) {
                        let asked = u32::try_from(identity.pid).ok().zip(entry.cwd.to_str());
                        identity.stored_login = asked.is_some_and(|(pid, cwd)| {
                            (asks.stored_login)(pid, &identity.argv, cwd)
                        });
                        let tapped = tap::carried_by(&identity.argv);
                        identity.unoffered = asked
                            .filter(|_| !tapped)
                            .and_then(|(_, cwd)| tap_line_refusal(identity, asks, cwd));
                        identity.login_checked_for = Some((entry.started_at, entry.cwd.clone()));
                    }
                }
                (identity.launch.clone(), identity.argv.clone())
            };
            footer::read_pid(
                &dir,
                pid,
                started,
                launch,
                tail,
                fold,
                clock,
                crate::harness_host::harness_state().as_deref(),
                (asks.home)().as_deref(),
            )
        });
    let mut dirs = facts
        .as_ref()
        .and(fold.dirs())
        .map(|dirs| (Some(dirs.cwd.clone()), dirs.repo_root.clone()));
    let mut generation = facts.as_ref().and(fold.traffic_generation());
    let mut flight_generation = facts.as_ref().and(fold.flight_generation());
    let mut non_claude_excerpts: Option<(aterm_agent::harness::excerpt::Excerpts, u64)> = None;
    if facts.is_none() {
        let (non_registry_prog, prev_sub, prev_ctx, prev_card_model) = {
            let held = timeline.lock().unwrap_or_else(|p| p.into_inner());
            let prog = held
                .agent()
                .program
                .as_deref()
                .and_then(aterm_phase::program_of)
                .filter(|p| matches!(p, aterm_phase::Program::Agy | aterm_phase::Program::Muse));
            let sub = held
                .claude_footer()
                .and_then(|f| f.subscription.clone())
                .unwrap_or_default();
            let ctx = held.claude_footer().and_then(|f| f.context.clone());
            let card_model = held.claude_card().map(|c| c.model.clone());
            (prog, sub, ctx, card_model)
        };
        if let Some(prog) = non_registry_prog {
            let prefix = match prog {
                aterm_phase::Program::Muse => "muse",
                _ => "agy",
            };
            let cwd = u32::try_from(identity.pid).ok().and_then(cwd_of);
            let home = (asks.home)();
            let (branch, repo_read_denied, repo_root) =
                match cwd.as_deref().map(footer::read_git_head) {
                    Some(footer::GitHeadRead::Found(b, root)) => (Some(b), None, Some(root)),
                    Some(footer::GitHeadRead::Denied) => (None, cwd.clone(), None),
                    Some(footer::GitHeadRead::Absent) | None => (None, None, None),
                };
            dirs = Some((cwd.clone(), repo_root));
            generation = None;
            flight_generation = None;
            let args = (asks.args)(identity.pid);
            identity.login = login_of_args(args.as_ref());
            identity.task = args
                .as_ref()
                .and_then(|a| crate::survey::task_prompt_of_env(&a.env));
            let launch = args
                .as_ref()
                .map(|a| footer::launch_facts_of(&a.argv, &a.env))
                .unwrap_or_default();
            let launch_model = launch
                .model
                .as_deref()
                .and_then(footer::footer_model_name)
                .or(launch.model);
            let (model, effort, model_open, effort_open) =
                match (prog, args.as_ref(), home.as_deref()) {
                    (aterm_phase::Program::Agy, Some(args), Some(home)) => {
                        aterm_agent::harness::profile::agy_model_effort_for(
                            &args.argv, &args.env, home,
                        )
                    }
                    (aterm_phase::Program::Muse, Some(args), Some(home)) => {
                        aterm_agent::harness::profile::muse_model_effort_for(
                            &args.argv, &args.env, home,
                        )
                    }
                    _ => (launch_model.clone(), None, launch_model.is_none(), true),
                };
            let model = model.or(launch_model);
            let mut sub = prev_sub;
            if matches!(prog, aterm_phase::Program::Muse | aterm_phase::Program::Agy)
                && muse_weekly_expired(
                    sub.weekly_reset.as_ref().and_then(|reset| reset.at),
                    clock.now,
                )
            {
                sub.weekly = None;
            }
            let files = u32::try_from(identity.pid).ok().and_then(asks.files);
            let mut resume = None;
            if prog == aterm_phase::Program::Muse
                && let (Some(args), Some(home)) = (args.as_ref(), home.as_deref())
            {
                if let Some(read) = aterm_agent::harness::profile::muse_subscription_for(
                    &args.argv, &args.env, home,
                ) {
                    match sub.email.as_deref() {
                        Some(previous) if read.email.as_deref() != Some(previous) => sub = read,
                        _ => sub.email = read.email,
                    }
                }
                let muse_pid = u32::try_from(identity.pid).unwrap_or(0);
                resume = aterm_agent::harness::profile::muse_resume_for(
                    muse_pid,
                    &args.argv,
                    &args.env,
                    home,
                    files.as_deref(),
                );
                non_claude_excerpts = aterm_agent::harness::profile::muse_excerpts_for(
                    muse_pid,
                    &args.argv,
                    &args.env,
                    home,
                    files.as_deref(),
                );
            }
            if prog == aterm_phase::Program::Agy
                && let (Some(args), Some(home)) = (args.as_ref(), home.as_deref())
            {
                if let Some(read) = aterm_agent::harness::profile::agy_subscription_for(
                    &args.argv,
                    &args.env,
                    home,
                    &sub,
                    prev_card_model.as_deref().or(model.as_deref()),
                    clock.now,
                ) {
                    sub = read;
                }
                resume = aterm_agent::harness::profile::agy_resume_for(
                    &args.argv,
                    &args.env,
                    home,
                    files.as_deref(),
                );
                non_claude_excerpts = aterm_agent::harness::profile::agy_excerpts_for(
                    &args.argv,
                    &args.env,
                    home,
                    files.as_deref(),
                );
            }
            facts = Some(FooterFacts {
                model_open,
                model,
                effort,
                effort_open,
                effort_for: None,
                path: cwd
                    .as_deref()
                    .and_then(|dir| footer::home_path(dir, home.as_deref())),
                branch,
                repo_read_denied,
                version: None,
                resume,
                usage: None,
                owner: Some(footer::FactsOwner {
                    floor: started,
                    session_id: format!("{prefix}-{}", identity.pid),
                }),
                upgrade: None,
                account: None,
                top_effort: None,
                off: footer::FactMask::default(),
                subscription: Some(sub),
                plan: None,
                context: prev_ctx,
            });
        }
    }
    if let (Some(facts), Some(who)) = (facts.as_mut(), identity.login.as_ref()) {
        facts.account = roster
            .at(std::time::Instant::now(), asks.roster)
            .and_then(|listed| accounts::footer_label(listed, who))
            .map(str::to_owned);
    }
    let model = is_claude
        .then(|| {
            shown_model(
                &timeline.lock().unwrap_or_else(|p| p.into_inner()),
                facts.as_ref(),
            )
        })
        .flatten();
    let read = is_claude.then(|| {
        (asks.args)(identity.pid).and_then(|args| {
            // A refused auth-source check says nothing, and a registry write
            // missed during an exec cannot borrow the prior image's verdict.
            if !identity.stored_login
                || args.argv != identity.argv
                || Some(exec_env_stamp(&args.env)) != identity.env_stamp
            {
                return None;
            }
            let tab = env_value(&args.env, aterm_types::domain::ENV_PARENT_SESSION_ID);
            aterm_agent::harness::profile::subscription_and_plan_for(
                &args.env,
                &(asks.home)()?,
                identity.dir.as_deref()?,
                roster.at(std::time::Instant::now(), asks.roster),
                clock.now,
                model.as_deref(),
            )
            .map(|(shown, plan)| (shown, plan, tab))
        })
    });
    let plan = read.as_ref().and_then(|read| read.as_ref()?.1);
    // The account gate passed for this process's profile (`gated`): its own figures read
    // below are its account's, and the store's are read only under its own login's label.
    let gated = read
        .as_ref()
        .and_then(|read| read.as_ref())
        .map(|(_, _, tab)| tab.clone());
    let mut subscription = read.map(|read| read.map(|(shown, ..)| shown).unwrap_or_default());
    if let Some(facts) = facts.as_mut()
        && is_claude
    {
        if let Some(sub) = subscription.as_mut() {
            weekly_into(
                sub,
                facts,
                identity,
                gated.as_ref().map(Option::as_deref),
                roster.at(std::time::Instant::now(), asks.roster),
                asks,
                clock.now,
            );
        }
        facts.subscription = subscription.clone();
        facts.plan = plan;
    }
    let denied = facts.as_ref().and_then(|f| f.repo_read_denied.clone());
    let excerpts = facts
        .as_ref()
        .filter(|_| {
            is_claude
                || tail.excerpts().is_some()
                || non_claude_excerpts.is_some()
                || identity.task.is_some()
        })
        .map(|_| crate::survey::ClaudeExcerpts {
            excerpts: tail
                .excerpts()
                .cloned()
                .or_else(|| non_claude_excerpts.as_ref().map(|(e, _)| e.clone()))
                .unwrap_or_default(),
            task: identity.task.clone(),
            transcript_len: tail
                .transcript_len()
                .or_else(|| non_claude_excerpts.as_ref().map(|(_, len)| *len)),
        });
    let side = facts.as_ref().map(|_| {
        let held = timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .claude_side()
            .cloned();
        let (cwd, repo_root) = dirs.take().unwrap_or_default();
        side_after(
            held.as_ref(),
            cwd,
            repo_root,
            SideFold {
                generation,
                traffic: || fold.traffic(),
                flight_generation,
                flights: || fold.flights(),
            },
        )
    });
    let row = facts.is_some();
    if facts.is_none()
        && identity.started.is_some()
        && let Some(sub) = subscription.as_mut()
    {
        // A footer kept over a transient registry miss: its week judged as a fresh read's,
        // read outside the timeline's lock.
        let kept = {
            let held = timeline.lock().unwrap_or_else(|p| p.into_inner());
            (held.claude_footer_identity() == Some((identity.pid, identity.started)))
                .then(|| held.claude_footer().cloned())
                .flatten()
        };
        if let Some(kept) = kept {
            weekly_into(
                sub,
                &kept,
                identity,
                gated.as_ref().map(Option::as_deref),
                roster.at(std::time::Instant::now(), asks.roster),
                asks,
                clock.now,
            );
        }
    }
    let mut timeline = timeline.lock().unwrap_or_else(|p| p.into_inner());
    if facts.is_none()
        && identity.started.is_some()
        && timeline.claude_footer_identity() == Some((identity.pid, identity.started))
    {
        let moved = timeline.claude_footer().cloned().is_some_and(|mut kept| {
            kept.subscription = subscription;
            kept.plan = plan;
            timeline.set_claude_footer(identity.pid, identity.started, Some(kept))
        });
        return (
            moved.then_some(None),
            Found {
                row,
                timed: needs_recheck(&timeline),
            },
        );
    }
    if let Some(f) = facts.as_mut()
        && timeline.claude_footer_identity() == Some((identity.pid, identity.started))
        && let Some(prev_ctx) = timeline
            .claude_footer()
            .and_then(|prev| prev.context.as_ref())
    {
        f.context = Some(prev_ctx.clone());
    }
    timeline.set_claude_excerpts(identity.pid, excerpts);
    timeline.set_claude_side(identity.pid, side);
    if row {
        timeline.note_claude_footer_read(identity.pid, std::time::Instant::now());
    }
    let moved = timeline
        .set_claude_footer(identity.pid, identity.started, facts)
        .then_some(denied);
    (
        moved,
        Found {
            row,
            timed: needs_recheck(&timeline),
        },
    )
}

/// The weekly figure a Claude Code footer shows in `sub`, one age rule for every source
/// ([`WeeklyFigure::words`]). Where the profile read passed its account gate (`gated`, naming
/// the process's tab where its environment does), its own figures (the newer of its usage-tap
/// reading, [`own_tap_figure`], and the `/usage` cache) say it while current; else a 7d wall in
/// the transcript, or one the accounts store holds of its own account ([`own_stored_week`]),
/// says `weekly 0% left`; else the newest figure still shown, its own or the store's; else why
/// none is ([`WeeklyGap`]).
fn weekly_into(
    sub: &mut SubscriptionDisplay,
    facts: &FooterFacts,
    identity: &Identity,
    gated: Option<Option<&str>>,
    roster: Option<&accounts::Roster>,
    asks: Asks,
    now: i64,
) {
    let shown = |figure: Option<WeeklyFigure>| figure.filter(|f| f.words(now).is_some());
    let tapped = tap::carried_by(&identity.argv);
    sub.label = gated.and_then(|_| own_label(identity, roster, asks));
    let seat = gated.map(|_| (asks.tap.seat)());
    let reading = gated
        .zip(seat.as_ref())
        .and_then(|(tab, seat)| tab.filter(|_| tapped).zip(seat.dir.as_deref()))
        .and_then(|(tab, dir)| own_tap_figure(identity, tab, dir));
    let own = gated.and_then(|_| WeeklyFigure::newer(shown(sub.weekly_figure), shown(reading)));
    let walled = facts
        .usage
        .as_ref()
        .and_then(|u| u.wall.as_ref())
        .is_some_and(|w| w.label == "7d");
    let figure = match own.filter(|own| own.current(now)) {
        Some(own) => Some(own),
        None => {
            let store = seat.as_ref().and_then(|seat| {
                own_stored_week(identity, roster, asks, seat.store.as_deref()?, now)
            });
            if walled || store.is_some_and(|week| week.walled) {
                sub.weekly = Some("weekly 0% left".into());
                (sub.weekly_figure, sub.weekly_gap) = (None, None);
                return;
            }
            WeeklyFigure::newer(own, store.and_then(|week| week.figure))
        }
    };
    sub.weekly = figure.and_then(|figure| figure.words(now));
    sub.weekly_figure = figure.filter(|_| sub.weekly.is_some());
    sub.weekly_gap = seat.filter(|_| sub.weekly.is_none()).map(|seat| {
        if tapped {
            let stale = reading.is_some_and(|f| now.saturating_sub(f.at) > USAGE_SHOWN_S);
            if stale {
                WeeklyGap::Stale
            } else {
                WeeklyGap::NoReply
            }
        } else if !seat.on {
            WeeklyGap::TapOff
        } else if !seat.named {
            WeeklyGap::Unoffered(tap::Refusal::Exe)
        } else if let Some(why) = tap_refusal(identity, asks, roster, now) {
            WeeklyGap::Unoffered(why)
        } else if gated
            .flatten()
            .zip(u32::try_from(identity.pid).ok())
            .is_some_and(|(tab, pid)| (asks.tap.restart)(tab, pid))
        {
            WeeklyGap::Untapped
        } else {
            WeeklyGap::Unrestarted
        }
    });
}

/// This process's own usage-tap figure: the week of the reading its tap wrote for tab `tab` in
/// `dir` since the process started ([`tap::Reading::read`]), whatever its age, for its own login
/// in the default home only. A reading names no account, so after a `/login` to another account
/// the week the process last held reads as the new account's until a reply brings the new one's.
fn own_tap_figure(identity: &Identity, tab: &str, dir: &std::path::Path) -> Option<WeeklyFigure> {
    let pid = u32::try_from(identity.pid).ok()?;
    let started = i64::try_from(identity.started?).ok()?;
    let login = identity.login.as_ref().filter(|login| login.default_home)?;
    let (reading, _) = tap::Reading::read(dir, tab, pid, started)?;
    let said = |value: Option<String>| value.filter(|value| !value.is_empty());
    if said(reading.home).is_some() || said(reading.login) != login.login {
        return None;
    }
    let week = reading.seven_day?;
    Some(WeeklyFigure {
        used_x10: week.x10(),
        resets_at: week.resets_at,
        at: reading.at,
    })
}

/// Why no relaunch line would carry the usage tap for this process (`relaunch::tap_of`'s and
/// `tap::gate`'s refusals), judged at each read: a Claude home or `HOME` of its own; a login no
/// line taps as the machine's own (`tap::Armed::lone_for`: not the default one, nor the lone one
/// the store binds `roster`'s one row to, nor, where the machine has no lone one, one `roster`
/// names); else the image's own reason ([`tap_line_refusal`]). `None` where a line would, and
/// where its login was not read.
fn tap_refusal(
    identity: &Identity,
    asks: Asks,
    roster: Option<&accounts::Roster>,
    now: i64,
) -> Option<tap::Refusal> {
    let login = identity.login.as_ref()?;
    let own_home = (asks.home)()
        .zip(identity.dir.as_deref())
        .is_some_and(|(home, dir)| tap::own_records(&home, dir));
    if !login.default_home || !own_home {
        return Some(tap::Refusal::Home);
    }
    if let Some(dir) = login.login.as_deref().filter(|dir| !dir.is_empty()) {
        let lone = roster
            .filter(|roster| roster.rows.len() == 1)
            .and_then(|roster| {
                let store = (asks.tap.seat)().store?;
                accounts::Store::at(&store)
                    .bound(roster, now)
                    .ok()?
                    .lone_login()
                    .map(str::to_owned)
            });
        let listed = || {
            roster.is_some_and(|roster| {
                roster.row_for_login(Some(dir)).is_some() || roster.row_left(Some(dir)).is_some()
            })
        };
        let tapped = match lone.as_deref() {
            Some(lone) => lone == dir,
            None => listed(),
        };
        if !tapped {
            return Some(tap::Refusal::Login);
        }
    }
    identity.unoffered
}

/// Why no relaunch line of this image's argv in `cwd` would carry the usage tap, judged once per
/// image: an option no relaunch line can repeat or its own `--settings` or `--bare`
/// (`tap::line_refusal`), or a settings chain that does not admit it (`TapAsks::settings`).
/// `None` where a line would.
fn tap_line_refusal(identity: &Identity, asks: Asks, cwd: &str) -> Option<tap::Refusal> {
    if let Some(why) = tap::line_refusal(&identity.argv) {
        return Some(why);
    }
    (!(asks.tap.settings)(&identity.argv, cwd)).then_some(tap::Refusal::Settings)
}

/// What the accounts store holds of one account's week ([`store_week`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct StoreWeek {
    /// A weekly wall stands.
    walled: bool,
    /// The newer of its usage-tap figure (not cut by a limit reset since) and its sample's,
    /// while [`WeeklyFigure::words`] still says one.
    figure: Option<WeeklyFigure>,
}

/// The store's figures of `acct`'s week at `now`, whatever their age: its usage-tap figure
/// (`windows["seven_day"]`, not cut by a limit reset since) and its sample's
/// ([`accounts::Sample::week`]), each while its window runs. A sample's figure is dated by its
/// weekly observation time (`week_read_at`), else its `at`; [`store_week`] and
/// `fact_menu::account_menu_rows_from` judge the age.
pub(crate) fn store_figures(acct: &accounts::Account, now: i64) -> [Option<WeeklyFigure>; 2] {
    let window = acct.windows.get(accounts::WEEK_LIMIT);
    let tapped = window
        .filter(|w| w.current(now) && !w.cut())
        .map(|w| WeeklyFigure {
            used_x10: w.pct_x10,
            resets_at: w.reset,
            at: w.at,
        });
    let sampled = acct.sample.as_ref().and_then(|sample| {
        let (pct, resets_at) = sample.week(now)?;
        Some(WeeklyFigure {
            used_x10: pct.saturating_mul(10),
            resets_at,
            at: sample.week_read_at.unwrap_or(sample.at),
        })
    });
    [tapped, sampled]
}

/// [`StoreWeek`] of `acct` at `now`: the store's figures under the same age rule as the
/// footer's own ([`WeeklyFigure::words`]), never only while their window runs.
fn store_week(acct: &accounts::Account, now: i64) -> StoreWeek {
    let shown = |figure: Option<WeeklyFigure>| figure.filter(|f| f.words(now).is_some());
    let [tapped, sampled] = store_figures(acct, now);
    StoreWeek {
        walled: acct
            .walls
            .get(accounts::WEEK_LIMIT)
            .is_some_and(|w| !w.cleared && w.until.is_some_and(|until| until > now)),
        figure: WeeklyFigure::newer(shown(tapped), shown(sampled)),
    }
}

/// [`store_week`] of this process's own account ([`own_label`]) from the accounts store under
/// harness state `store`; `None` where no label is judged, so no other account's week or wall
/// reaches its footer.
fn own_stored_week(
    identity: &Identity,
    roster: Option<&accounts::Roster>,
    asks: Asks,
    store: &std::path::Path,
    now: i64,
) -> Option<StoreWeek> {
    let label = own_label(identity, roster, asks)?;
    let (state, _) = accounts::Store::at(store).load(now).ok()?;
    Some(store_week(state.accounts.get(&label)?, now))
}

/// The roster label of this process's own account, for a process in the machine's default Claude
/// home and `HOME` only (the one whose readings the store keeps): the label its login is judged
/// to (`accounts::judged_label` over `roster`, or the machine's own row where it lists none). The
/// account menu's row it names is the only one the footer's week reaches
/// (`SubscriptionDisplay::label`).
fn own_label(identity: &Identity, roster: Option<&accounts::Roster>, asks: Asks) -> Option<String> {
    let login = identity.login.as_ref().filter(|login| login.default_home)?;
    let home = (asks.home)()?;
    if !tap::own_records(&home, identity.dir.as_deref()?) {
        return None;
    }
    let own = accounts::Roster::own();
    let roster = roster.map(|roster| if roster.rows.is_empty() { &own } else { roster })?;
    let who = accounts::ProcessLogin {
        stored_login: login.stored_login && identity.stored_login,
        ..login.clone()
    };
    accounts::judged_label(roster, &who).map(str::to_owned)
}

/// One known reset occurrence has arrived.
fn muse_weekly_expired(reset_at: Option<i64>, now: i64) -> bool {
    reset_at.is_some_and(|at| at <= now)
}

fn muse_weekly_reset_at(reset: &str, clock: &footer::ReadClock<'_>) -> Option<i64> {
    let spec = aterm_agent::supervise::limit::parse_reset(reset)?;
    let local = (clock.offset_at)(None, clock.now)?;
    Some(aterm_agent::supervise::limit::reset_at_with(
        &spec,
        clock.now,
        local,
        &|zone, unix| (clock.offset_at)(Some(zone), unix),
    ))
}

/// Admit one card observation.
fn muse_weekly_observation<'a>(
    (weekly, reset): (&'a str, Option<&'a str>),
    kept: Option<&aterm_agent::harness::profile::SubscriptionDisplay>,
    clock: &dyn Fn() -> footer::ReadClock<'static>,
) -> (Option<&'a str>, Option<(&'a str, Option<i64>)>) {
    let Some(words) = reset else {
        return (Some(weekly), None);
    };
    if let Some(kept) = kept
        && let Some(reset) = kept
            .weekly_reset
            .as_ref()
            .filter(|reset| reset.words == words)
    {
        return (
            kept.weekly.as_ref().map(|_| weekly),
            Some((words, reset.at)),
        );
    }
    let clock = clock();
    let at = muse_weekly_reset_at(words, &clock);
    (
        (!muse_weekly_expired(at, clock.now)).then_some(weekly),
        Some((words, at)),
    )
}

/// Whether any currently published footer value still needs local clock checks.
fn needs_recheck(timeline: &SessionTimeline) -> bool {
    timeline.claude_footer().is_some_and(|facts| {
        facts
            .usage
            .as_ref()
            .is_some_and(|usage| usage.wall.is_some())
            || facts
                .subscription
                .as_ref()
                .is_some_and(|subscription| subscription.weekly.is_some())
            || facts.plan.is_some()
    })
}

/// The model the footer shows for `facts` ([`FooterFacts::filled_model_effort`]).
fn shown_model(timeline: &SessionTimeline, facts: Option<&FooterFacts>) -> Option<String> {
    let kept = timeline.claude_footer();
    let facts = facts.or(kept)?;
    let card = timeline
        .claude_card()
        .filter(|_| kept.is_some_and(|kept| kept.owner == facts.owner));
    facts.filled_model_effort(card).0.map(str::to_owned)
}

/// The repaint key's fingerprint of `facts` with `card`, the `usage` switch and the hidden facts
/// `off`.
pub(crate) fn fingerprint(
    facts: Option<&FooterFacts>,
    card: Option<&aterm_phase::LaunchCard>,
    usage: bool,
    off: FactMask,
) -> u64 {
    use std::hash::{Hash, Hasher};
    let Some(facts) = facts else {
        return 0;
    };
    let (model, effort) = facts.filled_model_effort(card);
    let FooterFacts {
        model: _,
        effort: _,
        path,
        branch,
        repo_read_denied,
        version,
        resume,
        usage: usage_facts,
        model_open,
        effort_open,
        effort_for,
        owner,
        upgrade,
        account,
        top_effort: _,
        off: _,
        subscription,
        plan: _,
        context,
    } = facts;
    let version = version
        .as_deref()
        .or_else(|| card.map(|c| c.version.as_str()).filter(|v| !v.is_empty()));
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (
        model,
        effort,
        path,
        branch,
        repo_read_denied,
        version,
        resume,
    )
        .hash(&mut h);
    (
        model_open,
        effort_open,
        effort_for,
        owner,
        upgrade,
        account,
        off,
    )
        .hash(&mut h);
    subscription.hash(&mut h);
    context.hash(&mut h);
    if usage {
        usage_facts.hash(&mut h);
    } else {
        usage_facts
            .as_ref()
            .and_then(|u| u.wall.as_ref())
            .hash(&mut h);
    }
    h.finish() | 1
}

/// `facts` as the rule shows them under the `[claude_footer] usage` switch.
fn with_usage_switch(facts: &mut FooterFacts, usage: bool) {
    if !usage {
        facts.usage = facts
            .usage
            .take()
            .and_then(aterm_agent::harness::session_usage::UsageFacts::wall_only);
    }
}

/// Which of the composer rule's pane columns aterm may cover: a plain rule glyph (the cell
/// equal, colours and every attribute, to the rule's own `─`, whichever of the row's first
/// and last `─` styles more of the row) with nothing in the scratch's side channels at its
/// frame column (a cluster, a combining mark, an image, which the undo could not restore).
#[cfg(test)]
fn rule_mask(
    scratch: &RenderInput,
    frame_row: usize,
    col_off: usize,
    pane_cols: usize,
) -> Option<(Vec<bool>, RenderCell)> {
    let mut mask = Vec::new();
    let rule = rule_mask_into(&mut mask, scratch, frame_row, col_off, pane_cols)?;
    Some((mask, rule))
}

/// `rule_mask` into `mask`.
fn rule_mask_into(
    mask: &mut Vec<bool>,
    scratch: &RenderInput,
    frame_row: usize,
    col_off: usize,
    pane_cols: usize,
) -> Option<RenderCell> {
    mask.clear();
    let row = scratch.cells.get(frame_row)?;
    let cells = row.get(col_off..row.len().min(col_off + pane_cols))?;
    let first = *cells.iter().find(|c| c.ch == '\u{2500}')?;
    let last = *cells.iter().rev().find(|c| c.ch == '\u{2500}')?;
    let count = |style: &RenderCell| cells.iter().filter(|c| *c == style).count();
    let rule = if first == last || count(&last) >= count(&first) {
        last
    } else {
        first
    };
    mask.extend((0..pane_cols).map(|c| {
        let col = col_off + c;
        cells.get(c) == Some(&rule)
            && !touches(&scratch.clusters, frame_row, &(col..col + 1))
            && !touches(&scratch.combining, frame_row, &(col..col + 1))
            && !touches(&scratch.images, frame_row, &(col..col + 1))
    }));
    Some(rule)
}

/// The cells aterm writes into `room` cells of the rule for one pane.
fn compose_rule_spans(
    fit: &footer::RuleFit,
    block: Option<&crate::claude_lights::Block>,
    rule: RenderCell,
    blank: RenderCell,
    mark_fg: [u8; 3],
    room: usize,
    lit: Option<FactKind>,
) -> RulePaint {
    let mut out: Vec<RenderCell> = Vec::with_capacity(room);
    let mut lights_at = None;
    let mut title_at = 0..0;
    if let Some(block) = block.filter(|_| fit.chips) {
        let title: &[RenderCell] = match fit.title {
            footer::TitleFit::Full => &block.title,
            footer::TitleFit::Lean(n) => block.lean.get(usize::from(n)).map_or(&[][..], |t| t),
            footer::TitleFit::Short => &block.short_title,
            footer::TitleFit::None => &[],
        };
        out.push(blank);
        lights_at = Some(out.len());
        out.extend_from_slice(&block.lights);
        let gap = title.len().min(2);
        out.extend_from_slice(&title[title.len() - gap..]);
        title_at = out.len()..out.len() + (title.len() - gap);
        out.extend_from_slice(&title[..title.len() - gap]);
        out.push(blank);
    }
    let mut facts: Vec<RenderCell> = Vec::new();
    let mut spans: footer::FactSpans = [None; 6];
    let mut account: Option<(u16, u16)> = None;
    let mut clusters = Vec::new();
    let mut combining = Vec::new();
    if !fit.segments.is_empty() {
        let quiet = |fg: [u8; 3]| RenderCell {
            fg,
            bold: true,
            script: aterm_core::terminal::Script::Sub { nudge: 0 },
            ..blank
        };
        let mark = quiet(visible_on(mix(mark_fg, rule.fg, 0.5), blank.bg));
        let accent = legible_on(mark_fg, blank.bg);
        let words = FactInks::of(rule.fg, blank.fg, blank.bg);
        let value = quiet(words.value);
        let stub = RenderCell {
            bold: false,
            ..quiet(words.stub)
        };
        let lifted = |cell: RenderCell, fg: [u8; 3]| RenderCell { fg, ..cell };
        let set = footer::set_facts(&fit.segments);
        spans = footer::fact_spans(&set, &fit.segments);
        account = footer::account_span(&set, &fit.segments);
        let lit_span = lit.and_then(|kind| {
            spans
                .iter()
                .flatten()
                .find(|span| span.kind == kind)
                .copied()
        });
        let lit_cells = lit_span.map(|span| usize::from(span.cells.0)..usize::from(span.cells.1));
        let mut lit_inks: Vec<(isize, [u8; 3])> = Vec::new();
        facts = vec![blank; set.cells + 2];
        for glyph in &set.glyphs {
            let lit = lit_cells.as_ref().is_some_and(|c| c.contains(&glyph.cell));
            let ink = match (
                glyph.mark || glyph.ch == footer::RESET_MARK,
                glyph.quiet,
                lit,
            ) {
                (true, _, false) => mark,
                (true, _, true) => lifted(mark, accent),
                (false, true, false) => stub,
                (false, true, true) => lifted(stub, value.fg),
                (false, false, false) => value,
                (false, false, true) => lifted(value, words.lit),
            };
            let cell = RenderCell {
                ch: glyph.ch,
                script: aterm_core::terminal::Script::Sub { nudge: glyph.nudge },
                ..ink
            };
            facts[1 + glyph.cell] = cell;
            if glyph.cols == 2 {
                facts[2 + glyph.cell] = RenderCell {
                    ch: ' ',
                    wide: true,
                    ..cell
                };
            }
            if lit {
                let n = isize::from(footer::NUDGES_PER_CELL);
                let middle = glyph.cell as isize * n
                    + isize::from(glyph.nudge)
                    + glyph.cols as isize * n / 2;
                lit_inks.push((middle, cell.fg));
            }
        }
        if let Some(span) = lit_span {
            let n = isize::from(footer::NUDGES_PER_CELL);
            for c in footer::drawn_cells(&set, &span) {
                let middle = c as isize * n + n / 2;
                let ink = lit_inks
                    .iter()
                    .min_by_key(|(at, _)| (at - middle).abs())
                    .map(|&(_, fg)| fg);
                let cell = &mut facts[1 + c];
                cell.underline = aterm_core::terminal::UnderlineStyle::Single;
                cell.underline_color = ink;
            }
        }
        clusters = set.clusters;
        combining = set.combining;
    }
    let mut glass = 0..0;
    if !out.is_empty() {
        let glyphs = room.saturating_sub(out.len() + facts.len());
        glass = out.len()..out.len() + glyphs;
        out.extend(std::iter::repeat_n(rule, glyphs));
    }
    let facts_at = out.len();
    for (col, _) in &mut clusters {
        *col += facts_at + 1;
    }
    for (col, _) in &mut combining {
        *col += facts_at + 1;
    }
    out.extend(facts);
    RulePaint {
        cells: out,
        lights_at,
        title: title_at,
        glass,
        facts_at,
        spans,
        account,
        clusters,
        combining,
    }
}

/// [`compose_rule_spans`]' cells and where the chips start, as the tests
/// read a composition.
#[cfg(test)]
fn compose_rule(
    fit: &footer::RuleFit,
    block: Option<&crate::claude_lights::Block>,
    rule: RenderCell,
    blank: RenderCell,
    mark_fg: [u8; 3],
    room: usize,
) -> (Vec<RenderCell>, Option<usize>) {
    let paint = compose_rule_spans(fit, block, rule, blank, mark_fg, room, None);
    (paint.cells, paint.lights_at)
}

/// One rule's edits for [`write_edits`] ([`compose_rule_spans`]).
fn rule_edits<'a>(
    row: usize,
    col: usize,
    cells: &'a [RenderCell],
    glass: &std::ops::Range<usize>,
    ground: RenderCell,
) -> impl Iterator<Item = (usize, usize, &'a [RenderCell], RenderCell)> {
    let (head, tail) = if glass.is_empty() {
        (cells, cells.len())
    } else {
        (&cells[..glass.start], glass.end)
    };
    [
        (row, col, head, ground),
        (row, col + tail, &cells[tail..], ground),
    ]
    .into_iter()
    .filter(|(_, _, cells, _)| !cells.is_empty())
}

const _: () = assert!(footer::NUDGES_PER_CELL == aterm_core::terminal::Script::NUDGES_PER_CELL);

/// How far the values' ink is lifted from Claude's rule gray toward the pane's text colour.
const VALUE_LIFT: f32 = 1.0 / 3.0;

/// Lifts rule `gray` toward pane foreground `ink` for legible footer fact values.
pub(crate) fn value_fg(gray: [u8; 3], ink: [u8; 3]) -> [u8; 3] {
    mix(gray, ink, VALUE_LIFT)
}

/// `a` moved `t` of the way toward `b`, per channel.
fn mix(a: [u8; 3], b: [u8; 3], t: f32) -> [u8; 3] {
    std::array::from_fn(|i| {
        (f32::from(a[i]) + (f32::from(b[i]) - f32::from(a[i])) * t).round() as u8
    })
}

/// The scratch facts one footer write is valid against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Tokens {
    snapshot_seq: u64,
    engine_fill_seq: u64,
    shifted_fill_seq: u64,
    composed_fill_seq: u64,
    terminal_id: u64,
    extract_gen: u64,
    row_rev_lane: u64,
    row_shift: usize,
    rows: usize,
    cols: usize,
    cell_rows: usize,
}

impl Tokens {
    const fn of(s: &RenderInput) -> Self {
        Self {
            snapshot_seq: s.snapshot_seq,
            engine_fill_seq: s.engine_fill_seq,
            shifted_fill_seq: s.shifted_fill_seq,
            composed_fill_seq: s.composed_fill_seq,
            terminal_id: s.terminal_id,
            extract_gen: s.extract_gen,
            row_rev_lane: s.row_rev_lane,
            row_shift: s.row_shift,
            rows: s.rows,
            cols: s.cols,
            cell_rows: s.cells.len(),
        }
    }
}

/// One row a footer splice wrote.
#[derive(Debug)]
struct RowUndo {
    frame_row: usize,
    col_off: usize,
    /// The row's length before the write.
    len_before: usize,
    /// The cells the write replaced.
    before: Vec<RenderCell>,
    /// The cells the write put there.
    painted: Vec<RenderCell>,
    /// The row's revision before the write, when the write cleared it.
    rev_before: Option<u64>,
    clusters: TextUndo<Box<str>>,
    combining: TextUndo<Box<[char]>>,
}

/// Exact metadata owned by one cell edit, with returned payloads reused next frame.
#[derive(Debug, Default)]
struct TextUndo<T> {
    owned: Vec<(usize, T)>,
    live: usize,
    spare: Vec<T>,
}

impl<T: Clone + Eq> TextUndo<T> {
    fn publish(&mut self, channel: &mut Vec<(usize, T)>, col: usize, value: &T) {
        if self.live == self.owned.len() {
            self.owned.push((col, value.clone()));
        } else {
            let kept = &mut self.owned[self.live];
            kept.0 = col;
            if &kept.1 != value {
                kept.1.clone_from(value);
            }
        }
        self.live += 1;
        if self.spare.capacity() < self.live {
            self.spare.reserve(self.live - self.spare.len());
        }
        let matching = self.spare.iter().position(|payload| payload == value);
        let mut payload = matching
            .map(|index| self.spare.swap_remove(index))
            .or_else(|| self.spare.pop())
            .unwrap_or_else(|| value.clone());
        if &payload != value {
            payload.clone_from(value);
        }
        channel.push((col, payload));
    }

    fn intact(&self, channel: &[(usize, T)], span: &std::ops::Range<usize>) -> bool {
        channel
            .iter()
            .filter(|(col, _)| span.contains(col))
            .eq(self.owned[..self.live].iter())
    }

    fn remove(&mut self, channel: &mut Vec<(usize, T)>, span: &std::ops::Range<usize>) {
        self.spare.extend(
            channel
                .extract_if(.., |(col, _)| span.contains(col))
                .map(|(_, payload)| payload),
        );
    }
}

/// Saved cell state and splice token enabling zero-cost undo of the last footer splice on a
/// window's scratch.
#[derive(Debug, Default)]
pub(crate) struct FooterUndo {
    /// The write on the glass ([`undo`]).
    write: Option<WriteSeal>,
    /// Its rows, `rows[..live]`.
    rows: Vec<RowUndo>,
    live: usize,
    /// The rows of the write whose glass columns are not the grid's ([`GlassColumns`]).
    glass: GlassColumns,
}

/// The tokens a footer write is undone against ([`FooterUndo`]).
#[derive(Debug)]
struct WriteSeal {
    after: Tokens,
    seq_before: u64,
    shifted_before: u64,
    cluster_rows_before: usize,
    combining_rows_before: usize,
}

impl FooterUndo {
    /// Maps glass column `col` on `row` back to its underlying terminal grid column.
    pub(crate) fn grid_col(&self, row: u16, col: u16) -> u16 {
        if self.write.is_none() {
            return col;
        }
        self.glass.grid_col(row, col)
    }

    /// Whether the write on the glass put `(frame_row, col)`'s edit there.
    fn took(&self, frame_row: usize, col: usize) -> bool {
        self.written()
            .iter()
            .any(|r| (r.frame_row, r.col_off) == (frame_row, col))
    }

    /// Iterates `(frame_row, start_col, width)` spans written by the last footer splice.
    #[cfg(test)]
    pub(crate) fn painted_spans(&self) -> impl Iterator<Item = (usize, usize, usize)> + '_ {
        self.written()
            .iter()
            .map(|r| (r.frame_row, r.col_off, r.painted.len()))
    }

    /// Add native text only where this splice actually wrote its cells.
    fn publish_text(
        &mut self,
        scratch: &mut RenderInput,
        row: usize,
        col: usize,
        paint: &RulePaint,
    ) {
        if self.write.is_none() || (paint.clusters.is_empty() && paint.combining.is_empty()) {
            return;
        }
        for (at, text) in &paint.clusters {
            let at = col + at;
            if let Some(edit) = self.rows[..self.live].iter_mut().find(|edit| {
                edit.frame_row == row
                    && (edit.col_off..edit.col_off + edit.painted.len()).contains(&at)
            }) {
                scratch
                    .clusters
                    .resize_with(scratch.clusters.len().max(row + 1), Vec::new);
                edit.clusters.publish(&mut scratch.clusters[row], at, text);
            }
        }
        for (at, marks) in &paint.combining {
            let at = col + at;
            if let Some(edit) = self.rows[..self.live].iter_mut().find(|edit| {
                edit.frame_row == row
                    && (edit.col_off..edit.col_off + edit.painted.len()).contains(&at)
            }) {
                scratch
                    .combining
                    .resize_with(scratch.combining.len().max(row + 1), Vec::new);
                edit.combining
                    .publish(&mut scratch.combining[row], at, marks);
            }
        }
        if let Some(entries) = scratch.clusters.get_mut(row) {
            entries.sort_unstable_by_key(|(col, _)| *col);
        }
        if let Some(entries) = scratch.combining.get_mut(row) {
            entries.sort_unstable_by_key(|(col, _)| *col);
        }
    }

    /// The rows the write on the glass wrote.
    fn written(&self) -> &[RowUndo] {
        if self.write.is_some() {
            &self.rows[..self.live]
        } else {
            &[]
        }
    }

    /// Counts cells overwritten by the last splice with identical cell content.
    #[cfg(test)]
    pub(crate) fn rewritten_as_they_were(&self) -> usize {
        self.written()
            .iter()
            .map(|r| {
                r.before
                    .iter()
                    .zip(&r.painted)
                    .filter(|(was, now)| was == now)
                    .count()
            })
            .sum()
    }

    /// Whether a write is on the glass.
    #[cfg(test)]
    fn is_written(&self) -> bool {
        self.write.is_some()
    }

    /// Frame row `frame_row` of `scratch`, its cells in `cols`, as the program wrote them: each
    /// cell the write on the glass put there read as the cell it covered, and the row ending
    /// where it ended before the write padded it. This is what [`undo`] would put back, read
    /// without writing or allocating.
    #[cfg(any(a11y_tree, test))]
    fn program_cells<'a>(
        &'a self,
        scratch: &'a RenderInput,
        frame_row: usize,
        cols: std::ops::Range<usize>,
    ) -> impl Iterator<Item = &'a RenderCell> + 'a {
        let row: &[RenderCell] = scratch.cells.get(frame_row).map_or(&[], Vec::as_slice);
        let on_row = move || {
            self.written()
                .iter()
                .filter(move |r| r.frame_row == frame_row)
        };
        let glassed = on_row().next().is_some()
            && on_row().all(|r| {
                row.get(r.col_off..r.col_off + r.painted.len())
                    .is_some_and(|span| span == r.painted.as_slice())
            });
        let len = if glassed {
            on_row()
                .next()
                .map_or(row.len(), |r| r.len_before.min(row.len()))
        } else {
            row.len()
        };
        let end = cols.end.min(len);
        (cols.start.min(end)..end).map(move |c| {
            glassed
                .then(|| on_row().find(|r| (r.col_off..r.col_off + r.painted.len()).contains(&c)))
                .flatten()
                .and_then(|r| r.before.get(c - r.col_off))
                .unwrap_or(&row[c])
        })
    }
}

/// View of a window's presented [`RenderInput`] with footer glass writes undone.
#[cfg(any(a11y_tree, test))]
pub(crate) struct ProgramCells<'a> {
    scratch: &'a RenderInput,
    undo: &'a FooterUndo,
}

#[cfg(any(a11y_tree, test))]
impl<'a> ProgramCells<'a> {
    /// Wraps `scratch` with `undo` to read the underlying program cells.
    pub(crate) const fn of(scratch: &'a RenderInput, undo: &'a FooterUndo) -> Self {
        Self { scratch, undo }
    }
}

#[cfg(any(a11y_tree, test))]
impl crate::accessibility::CellRows for ProgramCells<'_> {
    fn cells_in(
        &self,
        r: usize,
        cols: std::ops::Range<usize>,
    ) -> impl Iterator<Item = &RenderCell> {
        self.undo.program_cells(self.scratch, r, cols)
    }
}

/// The pane's exact terminal extraction that supplied its cells to this frame.
fn frame_source<'a>(
    ws: &'a crate::WindowState,
    route: VisibleContentRoute,
    leaf_index: usize,
    leaf: &crate::tab_model::VisibleLeaf,
) -> Option<&'a RenderInput> {
    let source = match route {
        VisibleContentRoute::Terminal { composed: false } => &ws.input_scratch,
        VisibleContentRoute::Terminal { composed: true } if leaf.focused => {
            &ws.composed_focus_scratch
        }
        VisibleContentRoute::Terminal { composed: true } => {
            ws.unfocused_pane_scratch.get(&leaf_index)?
        }
        VisibleContentRoute::Heterogeneous => &ws.leaf_render_cache.get(&leaf.view)?.input,
        VisibleContentRoute::Native { .. } => return None,
    };
    (source.terminal_id != 0).then_some(source)
}

/// Restores cells overwritten by the previous footer splice on `scratch` if the scratch token still
/// matches.
pub(crate) fn undo(slot: &mut FooterUndo, scratch: &mut RenderInput) -> bool {
    slot.glass.clear();
    let Some(seal) = slot.write.take() else {
        return false;
    };
    let rows = &mut slot.rows[..std::mem::take(&mut slot.live)];
    if Tokens::of(scratch) != seal.after {
        return false;
    }
    let intact = rows.iter().all(|r| {
        let span = r.col_off..r.col_off + r.painted.len();
        scratch
            .cells
            .get(r.frame_row)
            .and_then(|row| row.get(r.col_off..r.col_off + r.painted.len()))
            .is_some_and(|span| span == r.painted.as_slice())
            && r.clusters.intact(
                scratch.clusters.get(r.frame_row).map_or(&[], Vec::as_slice),
                &span,
            )
            && r.combining.intact(
                scratch
                    .combining
                    .get(r.frame_row)
                    .map_or(&[], Vec::as_slice),
                &span,
            )
            && !touches(&scratch.images, r.frame_row, &span)
    });
    if !intact {
        return false;
    }
    for r in rows.iter_mut().rev() {
        let span = r.col_off..r.col_off + r.painted.len();
        if let Some(entries) = scratch.clusters.get_mut(r.frame_row) {
            r.clusters.remove(entries, &span);
        }
        if let Some(entries) = scratch.combining.get_mut(r.frame_row) {
            r.combining.remove(entries, &span);
        }
        let row = &mut scratch.cells[r.frame_row];
        row[r.col_off..r.col_off + r.before.len()].copy_from_slice(&r.before);
        row.truncate(r.len_before);
        if let (Some(rev), Some(slot)) = (r.rev_before, scratch.row_rev.get_mut(r.frame_row)) {
            *slot = rev;
        }
    }
    if scratch
        .clusters
        .get(seal.cluster_rows_before..)
        .is_some_and(|rows| rows.iter().all(Vec::is_empty))
    {
        scratch.clusters.truncate(seal.cluster_rows_before);
    }
    if scratch
        .combining
        .get(seal.combining_rows_before..)
        .is_some_and(|rows| rows.iter().all(Vec::is_empty))
    {
        scratch.combining.truncate(seal.combining_rows_before);
    }
    scratch.snapshot_seq = seal.seq_before;
    scratch.shifted_fill_seq = seal.shifted_before;
    true
}

/// Whether a column-keyed side channel holds an entry in `span` of `row`.
fn touches<T>(channel: &[Vec<(usize, T)>], row: usize, span: &std::ops::Range<usize>) -> bool {
    channel
        .get(row)
        .is_some_and(|entries| entries.iter().any(|(col, _)| span.contains(col)))
}

/// Write each `(frame_row, col, cells, ground)` edit into `scratch` and keep in `slot` the record
/// [`undo`] needs.
fn write_edits<'a>(
    slot: &mut FooterUndo,
    scratch: &mut RenderInput,
    edits: impl IntoIterator<Item = (usize, usize, &'a [RenderCell], RenderCell)>,
) {
    let seq_before = scratch.snapshot_seq;
    let shifted_before = scratch.shifted_fill_seq;
    let cluster_rows_before = scratch.clusters.len();
    let combining_rows_before = scratch.combining.len();
    let restamp = scratch.shifted_fill_seq != 0 && scratch.host_prepend_blessing();
    slot.write = None;
    slot.live = 0;
    slot.glass.clear();
    for (frame_row, col_off, painted, ground) in edits {
        let span = col_off..col_off + painted.len();
        if touches(&scratch.clusters, frame_row, &span)
            || touches(&scratch.combining, frame_row, &span)
            || touches(&scratch.images, frame_row, &span)
        {
            continue;
        }
        let Some(cells) = scratch.cells.get_mut(frame_row) else {
            continue;
        };
        if slot.live == slot.rows.len() {
            slot.rows.push(RowUndo {
                frame_row,
                col_off,
                len_before: 0,
                before: Vec::new(),
                painted: Vec::new(),
                rev_before: None,
                clusters: TextUndo::default(),
                combining: TextUndo::default(),
            });
        }
        let row = &mut slot.rows[slot.live];
        slot.live += 1;
        row.frame_row = frame_row;
        row.col_off = col_off;
        row.len_before = cells.len();
        row.clusters.live = 0;
        row.combining.live = 0;
        row.before.clear();
        row.before.extend_from_slice(
            cells
                .get(col_off..row.len_before.min(span.end))
                .unwrap_or(&[]),
        );
        if cells.len() < span.end {
            cells.resize(span.end, ground);
        }
        cells[span].copy_from_slice(painted);
        #[cfg(test)]
        crate::work_counts::footer_cells_written(painted.len());
        row.painted.clear();
        row.painted.extend_from_slice(painted);
        row.rev_before = if restamp {
            scratch
                .row_rev
                .get_mut(frame_row)
                .map(|rev| std::mem::replace(rev, 0))
        } else {
            None
        };
    }
    if slot.live == 0 {
        return;
    }
    scratch.snapshot_seq = scratch.snapshot_seq.wrapping_add(1);
    if restamp {
        scratch.shifted_fill_seq = scratch.snapshot_seq;
    }
    slot.write = Some(WriteSeal {
        after: Tokens::of(scratch),
        seq_before,
        shifted_before,
        cluster_rows_before,
        combining_rows_before,
    });
}

impl App {
    /// Computes the combined repaint fingerprint of footer facts, lights, card, and panel state
    /// for window `wid`.
    pub(crate) fn claude_footer_fp(
        &self,
        wid: WindowId,
        plan: &crate::tab_model::VisibleLeafPlan,
    ) -> u64 {
        let Some(ws) = self.windows.get(&wid) else {
            return 0;
        };
        let mut fp = ws
            .claude_lights
            .fingerprint(crate::claude_lights::lights_now);
        let mut claude = false;
        let usage = self.config.claude_footer_usage();
        let claims = crate::autopilot::ClaimClock::default();
        for (index, leaf) in plan.leaves.iter().enumerate() {
            let Some(session) = self
                .view_store
                .get(leaf.view)
                .copied()
                .and_then(crate::tab_model::View::terminal_session)
            else {
                continue;
            };
            let Some(entry) = self.pool.get(session) else {
                continue;
            };
            let timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
            let facts = timeline.claude_footer();
            claude |= facts.is_some();
            let card = facts
                .filter(|f| f.model_open || f.effort_open)
                .and_then(|_| timeline.claude_card());
            let off = self.fact_mask(session);
            let kept = facts.and_then(|f| {
                ws.claude_footer_texts
                    .borrow()
                    .footers
                    .get(index)?
                    .fingerprint_of(session, f, timeline.claude_card(), usage, off)
            });
            let leaf = kept.unwrap_or_else(|| fingerprint(facts, card, usage, off));
            let shows = facts.is_some();
            let note = timeline
                .upgrade_note()
                .filter(|_| facts.is_some())
                .map_or(0, |note| {
                    use std::hash::{Hash, Hasher};
                    let mut h = std::collections::hash_map::DefaultHasher::new();
                    note.hash(&mut h);
                    h.finish()
                });
            let (agent, group, identified) = (
                crate::harness_host::agent_of(timeline.agent()),
                timeline.agent().program_pgid,
                crate::harness_host::identified_agent(timeline.agent()),
            );
            let key = self.claude_facts.get(&session).map_or(0, |f| {
                u64::from(f.top_seen == Some(group))
                    | u64::from(f.press.as_ref().is_some_and(|p| {
                        matches!(p.slash, crate::claude_slash::Slash::TopEffort(_))
                    })) << 1
                    | u64::from(f.latch.as_ref().is_some_and(|latch| {
                        latch.holds(group, facts.and_then(|f| f.model.as_deref()))
                    })) << 2
            });
            let gate = timeline.card_gate().map_or(0, |gate| gate.key());
            let drawer = !shows && timeline.card_drawer_current();
            drop(timeline);
            let agent = crate::harness_host::admitted_as(&entry.ctx, agent, group);
            let harness = if shows {
                let (word, view, supervised, _) =
                    self.session_autopilot_for(session, agent, &claims);
                let theme = crate::autopilot::harness_theme(
                    view,
                    supervised,
                    word,
                    self.unhosted_glass(agent),
                );
                ws.claude_lights
                    .autopilot_term(session, word, view, self.chip_glass(session))
                    ^ self
                        .claude_card_term(wid, ws, index, session, view, supervised)
                        .rotate_left(37)
                    ^ self.team_term(session).rotate_left(53)
                    ^ theme_term(theme).rotate_left(61)
                    ^ self
                        .claude_panel_term(ws, index, session, theme)
                        .rotate_left(43)
                    ^ ws.claude_lights
                        .switch_term(session, supervised)
                        .rotate_left(59)
            } else if drawer {
                let (_, view, supervised, _) = self.session_autopilot_for(session, agent, &claims);
                self.claude_card_term(wid, ws, index, session, view, supervised)
                    .rotate_left(37)
                    ^ 0xd7a3_5e11
            } else {
                0
            } ^ gate.rotate_left(11);
            let top = match identified {
                Some(prog @ (aterm_phase::Program::Gemini | aterm_phase::Program::Codex)) => {
                    self.bare_top_term(ws, session, prog, agent, &claims)
                }
                _ => 0,
            };
            fp = fp.rotate_left(7)
                ^ leaf
                ^ note.rotate_left(17)
                ^ harness.rotate_left(29)
                ^ key.rotate_left(41)
                ^ top.rotate_left(31);
        }
        if claude {
            fp ^= u64::from(usage).rotate_left(47);
        }
        fp
    }

    /// What aterm draws for `session`'s top-effort key on its top rule ([`PaneFooter::key_typed`]).
    fn key_face(
        &self,
        ws: &crate::WindowState,
        session: u64,
        pgid: i32,
        stated: Option<bool>,
        model: Option<&str>,
        tag: Option<(usize, usize)>,
    ) -> KeyFace {
        let Some(stated) = stated else {
            return KeyFace::default();
        };
        let facts = self.claude_facts.get(&session);
        let pressing = facts
            .and_then(|f| f.press.as_ref())
            .is_some_and(|p| matches!(p.slash, crate::claude_slash::Slash::TopEffort(_)));
        let evidence = stated || facts.and_then(|f| f.top_seen) == Some(pgid) || pressing;
        let latched = facts
            .and_then(|f| f.latch.as_ref())
            .is_some_and(|latch| latch.holds(pgid, model));
        let (hovered, selected) = ws.claude_lights.top_effort_read(session);
        let ghost = (tag.is_none() && evidence && !latched).then_some(hovered);
        let mark = if pressing {
            crate::autopilot::PENDING_GLYPH.chars().next()
        } else {
            selected.then_some('\u{25b8}')
        };
        KeyFace {
            ghost,
            tag,
            mark: mark.filter(|_| ghost.is_some() || tag.is_some()),
        }
    }

    /// The harness switch and the theme Gemini CLI's or Codex's top line (`prog`) wears.
    fn bare_top_harness(
        &self,
        ws: &crate::WindowState,
        session: u64,
        prog: aterm_phase::Program,
        agent: Option<aterm_phase::Program>,
        (word, view, supervised, self_proven): (
            Option<aterm_agent::supervise::Autopilot>,
            crate::autopilot::AutopilotView,
            bool,
            bool,
        ),
    ) -> (
        Option<SwitchPaint>,
        Option<aterm_effects::harness_card::CardPalette>,
    ) {
        let switch = agent
            .and_then(|_| crate::autopilot::HarnessSwitch::of(view))
            .map(|face| {
                let (face, hover) = ws.claude_lights.switch_shown(session, face);
                SwitchPaint {
                    face,
                    hover,
                    version: None,
                }
            });
        let carried = supervised || (prog == aterm_phase::Program::Codex && self_proven);
        let theme =
            crate::autopilot::harness_theme(view, carried, word, self.unhosted_glass(agent));
        (switch, theme.filter(|_| switch.is_some()))
    }

    /// The repaint key's term for Gemini CLI's or Codex's top line ([`Self::bare_top_harness`]).
    fn bare_top_term(
        &self,
        ws: &crate::WindowState,
        session: u64,
        prog: aterm_phase::Program,
        agent: Option<aterm_phase::Program>,
        claims: &crate::autopilot::ClaimClock,
    ) -> u64 {
        let reading = self.session_autopilot_for(session, agent, claims);
        let (word, view, supervised, self_proven) = reading;
        let carried = supervised || (prog == aterm_phase::Program::Codex && self_proven);
        let theme =
            crate::autopilot::harness_theme(view, carried, word, self.unhosted_glass(agent));
        view.code()
            ^ u64::from(agent.is_some()).rotate_left(5)
            ^ theme_term(theme).rotate_left(13)
            ^ ws.claude_lights
                .switch_term(session, carried)
                .rotate_left(23)
    }

    /// Splice one pane's top line ([`top_line`], [`PaneFooter::compose_top`]).
    fn splice_top_line(
        &self,
        ws: &crate::WindowState,
        wid: WindowId,
        texts: &mut PaneTextCache,
        lists: &mut SpliceLists,
        at: &TopLineAt,
    ) {
        let TopLineAt {
            leaf_index,
            session,
            program_pgid,
            program,
            build,
            region,
            pane,
            blank,
            switch,
            palette,
            held,
            cell,
            mark_fg,
        } = *at;
        let Some((top, line)) = texts.top_line(leaf_index, program) else {
            return;
        };
        if self.chrome_owns_terminal_row(wid, region.row_off + top) {
            return;
        }
        let (Some(run), tag) = texts.top_run(
            leaf_index,
            (session, program),
            &ws.input_scratch,
            region,
            top,
            line,
            blank,
        ) else {
            return;
        };
        let ground = match line {
            TopLine::Rule => blank,
            TopLine::Slab => run.rule,
        };
        let mark_fg = legible_on(mark_fg, ground.bg);
        let mark_hover = ws.claude_lights.mark_hovered(session);
        let footer = texts.footer(leaf_index, session);
        let typed = program == aterm_phase::Program::Claude && footer.key_typed();
        let face = self.key_face(
            ws,
            session,
            program_pgid,
            typed.then(|| footer.shown().top_effort.is_some()),
            footer.published().and_then(|f| f.model.as_deref()),
            tag,
        );
        if face.tag.is_some()
            && self.claude_facts.get(&session).and_then(|f| f.top_seen) != Some(program_pgid)
        {
            lists.top_seen.push((session, program_pgid));
        }
        let frame_row = region.strip + region.row_off + top;
        if top_line_selected(&ws.input_scratch, frame_row, region) {
            if let Some(cols) = face.tag {
                lists.top_hits.push(TopHit {
                    frame_row,
                    term_row: region.row_off + top,
                    cols,
                    session,
                    ghost: false,
                });
            }
            return;
        }
        let hide_lockup = bare_while_off(Some(program), switch.map(|s| s.face));
        let key = TopKey {
            frame_row,
            term_row: region.row_off + top,
            line,
            run,
            program: if hide_lockup {
                aterm_phase::Program::Generic
            } else {
                program
            },
            build,
            switch,
            palette: palette.filter(|_| !hide_lockup),
            blank: ground,
            mark_fg,
            face,
            cell,
            clip: pane_grid_px(cell, pane),
            mark_hover,
            title_gen: if mark_hover && held {
                footer.shown_gen
            } else {
                0
            },
        };
        let composed = texts.compose_top(leaf_index, key);
        let hit = |cols: (usize, usize), ghost: bool| TopHit {
            frame_row: key.frame_row,
            term_row: key.term_row,
            cols,
            session,
            ghost,
        };
        if let Some(cols) = composed.ghost {
            lists.top_hits.push(hit(cols, true));
        } else if let Some(cols) = face.tag {
            lists.top_hits.push(hit(cols, false));
        }
        if let (Some(laid), Some(paint)) = (composed.switch.clone(), switch) {
            lists
                .border_toggles
                .push(crate::claude_lights::BorderSwitch {
                    frame_row: key.frame_row,
                    term_row: key.term_row,
                    cols: laid.cols,
                    switch: laid.switch,
                    button: laid.button,
                    session,
                    face: paint.face,
                    tier: laid.tier,
                    program,
                });
        }
        if !composed.cells.is_empty() {
            if !composed.lockup.is_empty() {
                lists.marks.push(MarkWrite {
                    frame_row: key.frame_row,
                    term_row: key.term_row,
                    edit_col: composed.col,
                    cols: composed.lockup.clone(),
                    session,
                });
            }
            lists.tops.push(TopWrite {
                frame_row: key.frame_row,
                col: composed.col,
                leaf_index,
                ground,
                glass: composed.glass.clone(),
            });
        }
    }

    /// Splices the Claude footer.
    pub(crate) fn splice_claude_footer(
        &mut self,
        wid: WindowId,
        plan: &crate::tab_model::VisibleLeafPlan,
        route: VisibleContentRoute,
    ) {
        self.splice_claude_footer_at(wid, plan, route, &|| footer::ReadClock {
            read_at: std::time::SystemTime::now(),
            now: aterm_agent::supervise::limit::unix_now(),
            offset_at: &offset_at_cached,
        });
    }

    /// The same splice with a lazy observation clock.
    fn splice_claude_footer_at(
        &mut self,
        wid: WindowId,
        plan: &crate::tab_model::VisibleLeafPlan,
        route: VisibleContentRoute,
        clock: &dyn Fn() -> footer::ReadClock<'static>,
    ) {
        let Some(mut lists) = self.windows.get_mut(&wid).map(|ws| {
            let strip = ws
                .input_scratch
                .cells
                .len()
                .saturating_sub(usize::from(ws.rows));
            ws.pet_chrome
                .begin(plan.leaves.iter().find(|leaf| leaf.focused).map(|leaf| {
                    (
                        strip + leaf.rect.origin.y.round().max(0.0) as usize,
                        leaf.rect.origin.x.round().max(0.0) as usize,
                        leaf.rect.size.height.round().max(1.0) as usize,
                        leaf.rect.size.width.round().max(1.0) as usize,
                    )
                }));
            std::mem::take(&mut ws.claude_footer_lists)
        }) else {
            return;
        };
        let grid = self.glass_grid(wid);
        self.splice_claude_footer_with(wid, plan, route, &mut lists, clock, grid);
        self.settle_glass_hover(wid, grid);
        for (session, pgid) in lists.top_seen.drain(..) {
            self.claude_facts.entry(session).or_default().top_seen = Some(pgid);
        }
        for session in lists.log_owed.drain(..) {
            match self.proxy.as_ref() {
                Some(proxy) => {
                    let _ = proxy.send_event(Wake::SessionLogMoved { session });
                }
                None => self.on_session_log_moved(session),
            }
        }
        lists.clear();
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.claude_footer_lists = lists;
        }
    }

    /// [`Self::splice_claude_footer`] on the window's lists (`lists`, empty on entry).
    fn splice_claude_footer_with(
        &mut self,
        wid: WindowId,
        plan: &crate::tab_model::VisibleLeafPlan,
        route: VisibleContentRoute,
        lists: &mut SpliceLists,
        clock: &dyn Fn() -> footer::ReadClock<'static>,
        grid: Option<crate::app_mouse::GlassGrid>,
    ) {
        let mut cards = Vec::new();
        if let Some(ws) = self.windows.get_mut(&wid) {
            undo(&mut ws.claude_footer_undo, &mut ws.input_scratch);
            crate::claude_card::strip(&mut ws.claude_card, &mut ws.input_scratch);
            ws.claude_lights.clear_hits();
            ws.claude_footer_texts.borrow_mut().trim(plan.leaves.len());
            cards = ws.claude_card.take_leaves();
        }
        if !self.config.tab_status_or_default() {
            if let Some(ws) = self.windows.get_mut(&wid) {
                ws.claude_footer_texts.borrow_mut().trim(0);
                ws.claude_card.hide_all();
            }
            return;
        }
        let mark_fg = crate::chrome_band::band_colors(self.theme).accent;
        let cell = {
            let (cw, ch) = grid.map_or_else(|| self.win_cell_size(wid), |g| g.cell());
            (
                u16::try_from(cw).unwrap_or(u16::MAX),
                u16::try_from(ch).unwrap_or(u16::MAX),
            )
        };
        let usage = self.config.claude_footer_usage();
        let claims = crate::autopilot::ClaimClock::default();
        {
            let Some(ws) = self.windows.get(&wid) else {
                return;
            };
            let frame_rows = ws.input_scratch.cells.len();
            let strip = frame_rows.saturating_sub(usize::from(ws.rows));
            for (leaf_index, leaf) in plan.leaves.iter().enumerate() {
                let Some(session) = self
                    .view_store
                    .get(leaf.view)
                    .copied()
                    .and_then(crate::tab_model::View::terminal_session)
                else {
                    continue;
                };
                let Some(entry) = self.pool.get(session) else {
                    continue;
                };
                let Some(source) = frame_source(ws, route, leaf_index, leaf) else {
                    continue;
                };
                if source.display_offset != 0 {
                    let agent = {
                        let timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
                        (
                            crate::harness_host::agent_of(timeline.agent()),
                            timeline.agent().program_pgid,
                        )
                    };
                    let agent = crate::harness_host::admitted_as(&entry.ctx, agent.0, agent.1);
                    if let Some(prog @ (aterm_phase::Program::Agy | aterm_phase::Program::Muse)) =
                        agent
                    {
                        let (_, view, supervised, _) =
                            self.session_autopilot_for(session, agent, &claims);
                        let agy_switch_off =
                            bare_while_off(Some(prog), crate::autopilot::HarnessSwitch::of(view));
                        if view.carries() && supervised && !agy_switch_off {
                            let row_off = leaf.rect.origin.y.round().max(0.0) as usize;
                            let col_off = leaf.rect.origin.x.round().max(0.0) as usize;
                            let pane_rows = leaf.rect.size.height.round().max(1.0) as usize;
                            let pane_cols = leaf.rect.size.width.round().max(1.0) as usize;
                            let region = PaneTextRegion {
                                strip,
                                row_off,
                                col_off,
                                rows: pane_rows,
                                cols: pane_cols,
                            };
                            let blank = source.implicit_blank;
                            let rule_cell = RenderCell {
                                fg: mix(blank.bg, blank.fg, 0.35),
                                ..blank
                            };
                            let rows: Vec<String> = (0..region.rows)
                                .map(|r| {
                                    ws.input_scratch
                                        .cells
                                        .get(region.strip + region.row_off + r)
                                        .map(|row| {
                                            let row = row.get(region.col_off..).unwrap_or_default();
                                            row[..row.len().min(region.cols)]
                                                .iter()
                                                .map(|c| c.ch)
                                                .collect()
                                        })
                                        .unwrap_or_default()
                                })
                                .collect();
                            collect_agy_transcript_edits(
                                &ws.input_scratch,
                                prog,
                                &rows,
                                region,
                                None,
                                blank,
                                rule_cell,
                                mark_fg,
                                &mut lists.agy_edits,
                            );
                        }
                    }
                    continue;
                }
                let (held, agent, flights, gate, identified, muse_cards_need_account) = {
                    let mut timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
                    #[cfg(test)]
                    crate::work_counts::footer_timeline_locked();
                    if timeline.claude_footer().is_none()
                        && timeline.agent().program_pgid > 0
                        && let Some(prog @ (aterm_phase::Program::Agy | aterm_phase::Program::Muse)) =
                            timeline
                                .agent()
                                .program
                                .as_deref()
                                .and_then(aterm_phase::program_of)
                    {
                        let pgid = timeline.agent().program_pgid;
                        let prefix = match prog {
                            aterm_phase::Program::Muse => "muse",
                            _ => "agy",
                        };
                        timeline.set_claude_footer(
                            pgid,
                            None,
                            Some(FooterFacts {
                                model_open: true,
                                effort_open: true,
                                owner: Some(footer::FactsOwner {
                                    floor: None,
                                    session_id: format!("{prefix}-{pgid}"),
                                }),
                                subscription: Some(
                                    aterm_agent::harness::profile::SubscriptionDisplay::default(),
                                ),
                                ..FooterFacts::default()
                            }),
                        );
                    }
                    let held = timeline.claude_footer().is_some_and(|facts| {
                        let mut texts = ws.claude_footer_texts.borrow_mut();
                        texts.note_facts(
                            leaf_index,
                            session,
                            facts,
                            timeline.claude_card(),
                            usage,
                            self.fact_mask(session),
                        );
                        texts
                            .footer(leaf_index, session)
                            .note_upgrade(timeline.upgrade_note());
                        true
                    });
                    (
                        held,
                        (
                            crate::harness_host::agent_of(timeline.agent()),
                            timeline.agent().program_pgid,
                        ),
                        timeline.claude_side().and_then(|side| side.flights.clone()),
                        timeline.card_gate(),
                        crate::harness_host::identified_agent(timeline.agent()),
                        timeline.muse_cards_need_account(),
                    )
                };
                let program_pgid = agent.1;
                let row_off = leaf.rect.origin.y.round().max(0.0) as usize;
                let col_off = leaf.rect.origin.x.round().max(0.0) as usize;
                let pane_rows = leaf.rect.size.height.round().max(1.0) as usize;
                let pane_cols = leaf.rect.size.width.round().max(1.0) as usize;
                let pane = crate::claude_card::PaneGeom {
                    strip,
                    row_off,
                    col_off,
                    rows: pane_rows,
                    cols: pane_cols,
                };
                let region = PaneTextRegion {
                    strip,
                    row_off,
                    col_off,
                    rows: pane_rows,
                    cols: pane_cols,
                };
                let blank = source.implicit_blank;
                let bare_top = match identified {
                    Some(prog @ (aterm_phase::Program::Gemini | aterm_phase::Program::Codex)) => {
                        Some(prog)
                    }
                    _ => None,
                };
                if !held && gate.is_none() {
                    // No drawer and no facts: a card this window keeps for the session left
                    // by its identity (Claude stopped or gone) bounds its next show
                    // (`claude_card::RESHOW_MS`). Only a Gemini CLI or Codex top line is read.
                    if ws.claude_card.keeps(session) {
                        cards.push(crate::claude_card::drawerless_leaf(
                            leaf_index, session, pane,
                        ));
                    }
                    if let Some(prog) = bare_top {
                        let agent = crate::harness_host::admitted_as(&entry.ctx, agent.0, agent.1);
                        let reading = self.session_autopilot_for(session, agent, &claims);
                        let (switch, palette) =
                            self.bare_top_harness(ws, session, prog, agent, reading);
                        let mut texts = ws.claude_footer_texts.borrow_mut();
                        texts.rows(leaf_index, session, &ws.input_scratch, region);
                        lists.top_only.push(leaf_index);
                        self.splice_top_line(
                            ws,
                            wid,
                            &mut texts,
                            lists,
                            &TopLineAt {
                                leaf_index,
                                session,
                                program_pgid,
                                program: prog,
                                build: top_build(prog, agent),
                                region,
                                pane,
                                blank,
                                switch,
                                palette,
                                held: false,
                                cell,
                                mark_fg,
                            },
                        );
                    }
                    continue;
                }
                let agent = crate::harness_host::admitted_as(&entry.ctx, agent.0, agent.1);
                let reading = self.session_autopilot_for(session, agent, &claims);
                let (word, view, supervised, _) = reading;
                let mut check = crate::claude_card::GateCheck::new(
                    gate,
                    crate::claude_card::FenceFrame::of(source),
                );
                let mut texts = ws.claude_footer_texts.borrow_mut();
                texts.rows(leaf_index, session, &ws.input_scratch, region);
                let texts = &mut *texts;
                if let Some(footer) = texts.footers.get(leaf_index).filter(|_| held)
                    && let Some(owner) = footer.card_owner()
                    && let Some((program, read)) = texts.footer_card(leaf_index, session)
                {
                    let card = &read.card;
                    let same_as_kept = footer.kept_card().is_some_and(|kept| {
                        kept.model == card.model
                            && kept.effort == card.effort
                            && (kept.version == card.version || card.version.is_empty())
                    });
                    if !same_as_kept
                        && check.admits_reading(
                            texts,
                            leaf_index,
                            session,
                            *program,
                            read.banner,
                            read.status,
                        )
                    {
                        let mut card = card.clone();
                        if card.version.is_empty()
                            && let Some(kept) = footer.kept_card()
                            && !kept.version.is_empty()
                        {
                            card.version.clone_from(&kept.version);
                        }
                        let owner = owner.clone();
                        let mut timeline =
                            entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
                        #[cfg(test)]
                        crate::work_counts::footer_timeline_locked();
                        if let Some((pgid, _)) = timeline.claude_footer_identity()
                            && timeline.note_claude_card(pgid, &owner, card)
                        {
                            // The card's amend of the session log: its owed
                            // wake rings its watchers in this hold, and the
                            // loop follows the log after the frame.
                            if timeline.slog_take_wake() {
                                lists.log_owed.push(session);
                            }
                            if let Some(facts) = timeline.claude_footer() {
                                texts.note_facts(
                                    leaf_index,
                                    session,
                                    facts,
                                    timeline.claude_card(),
                                    usage,
                                    self.fact_mask(session),
                                );
                            }
                        }
                    }
                }
                if held && texts.panes.get(leaf_index).is_some_and(Option::is_some) {
                    let shown = texts.footers[leaf_index].shown();
                    let want_acct = matches!(
                        agent,
                        Some(aterm_phase::Program::Agy | aterm_phase::Program::Muse)
                    ) && shown.subscription.is_some();
                    let want_ctx = agent == Some(aterm_phase::Program::Claude);
                    if want_acct || want_ctx {
                        let screen_acct = want_acct
                            .then(|| texts.screen_account(leaf_index, agent))
                            .flatten();
                        let stale_muse_cards = matches!(
                            agent,
                            Some(aterm_phase::Program::Muse | aterm_phase::Program::Agy)
                        ) && screen_acct.is_some_and(|(email, _)| {
                            shown
                                .subscription
                                .as_ref()
                                .and_then(|sub| sub.email.as_deref())
                                .is_some_and(|current| current != email)
                        });
                        let screen_acct = (!stale_muse_cards).then_some(screen_acct).flatten();
                        let has_live_agy_figure = agent == Some(aterm_phase::Program::Agy)
                            && shown
                                .subscription
                                .as_ref()
                                .and_then(|sub| sub.weekly_figure)
                                .is_some();
                        let screen_weekly = (matches!(
                            agent,
                            Some(aterm_phase::Program::Muse | aterm_phase::Program::Agy)
                        ) && want_acct
                            && !has_live_agy_figure
                            && !stale_muse_cards
                            && !muse_cards_need_account)
                            .then(|| texts.usage_weekly(leaf_index, agent))
                            .flatten()
                            .map(|reading| {
                                muse_weekly_observation(reading, shown.subscription.as_ref(), clock)
                            });
                        let screen_plan = (agent == Some(aterm_phase::Program::Muse)
                            && want_acct
                            && !stale_muse_cards)
                            .then(|| match screen_acct {
                                Some((_, plan)) => plan,
                                None if !muse_cards_need_account => {
                                    texts.subscription_plan(leaf_index)
                                }
                                None => None,
                            })
                            .flatten();
                        let screen_ctx = want_ctx
                            .then(|| texts.context_reading(leaf_index))
                            .flatten();
                        let need_acct = screen_acct.is_some_and(|(email, plan)| {
                            shown.subscription.as_ref().is_some_and(|sub| {
                                sub.email.as_deref() != Some(email) || sub.plan.as_deref() != plan
                            })
                        }) || screen_plan.is_some_and(|plan| {
                            shown
                                .subscription
                                .as_ref()
                                .is_some_and(|sub| sub.plan.as_deref() != Some(plan))
                        }) || screen_weekly.is_some_and(|(weekly, reset)| {
                            shown.subscription.as_ref().is_some_and(|sub| {
                                sub.weekly.as_deref() != weekly
                                    || sub
                                        .weekly_reset
                                        .as_ref()
                                        .map(|reset| (reset.words.as_str(), reset.at))
                                        != reset
                            })
                        });
                        let need_ctx = match screen_ctx {
                            Some(pct) => context_left_of(shown.context.as_deref()) != Some(pct),
                            None => {
                                want_ctx
                                    && shown.context.is_some()
                                    && texts.indicator_gone(leaf_index)
                            }
                        };
                        if need_acct || need_ctx {
                            let mut timeline =
                                entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
                            if let Some((pgid, started)) = timeline.claude_footer_identity()
                                && let Some(mut facts) = timeline.claude_footer().cloned()
                                && Some(&facts) == texts.footers[leaf_index].published()
                                && timeline.muse_cards_need_account() == muse_cards_need_account
                            {
                                if let Some((email, plan)) = screen_acct
                                    && let Some(sub) = facts.subscription.as_mut()
                                {
                                    sub.email = Some(email.to_owned());
                                    sub.plan = plan.map(str::to_owned);
                                }
                                if let Some(plan) = screen_plan
                                    && let Some(sub) = facts.subscription.as_mut()
                                {
                                    sub.plan = Some(plan.to_owned());
                                }
                                if let Some((weekly, reset)) = screen_weekly
                                    && let Some(sub) = facts.subscription.as_mut()
                                {
                                    if weekly.is_some() || sub.weekly_figure.is_none() {
                                        sub.weekly = weekly.map(str::to_owned);
                                    }
                                    sub.weekly_reset = reset.map(|(words, at)| {
                                        aterm_agent::harness::profile::WeeklyReset {
                                            words: words.to_owned(),
                                            at,
                                        }
                                    });
                                }
                                if need_ctx {
                                    facts.context = screen_ctx.map(context_words);
                                }
                                timeline.set_claude_footer(pgid, started, Some(facts));
                                if let Some(facts) = timeline.claude_footer() {
                                    texts.note_facts(
                                        leaf_index,
                                        session,
                                        facts,
                                        timeline.claude_card(),
                                        usage,
                                        self.fact_mask(session),
                                    );
                                }
                            }
                        }
                    }
                }
                let agy_switch_off =
                    bare_while_off(agent, crate::autopilot::HarnessSwitch::of(view));
                cards.push(crate::claude_card::card_leaf(
                    texts,
                    self.config.claude_footer_card() && !agy_switch_off,
                    leaf_index,
                    session,
                    pane,
                    blank,
                    view,
                    supervised,
                    &mut check,
                ));
                if !held {
                    if let Some(prog) = bare_top {
                        let (switch, palette) =
                            self.bare_top_harness(ws, session, prog, agent, reading);
                        lists.top_only.push(leaf_index);
                        self.splice_top_line(
                            ws,
                            wid,
                            texts,
                            lists,
                            &TopLineAt {
                                leaf_index,
                                session,
                                program_pgid,
                                program: prog,
                                build: top_build(prog, agent),
                                region,
                                pane,
                                blank,
                                switch,
                                palette,
                                held: false,
                                cell,
                                mark_fg,
                            },
                        );
                    }
                    continue;
                }
                let version = texts.footers[leaf_index].shown().version.as_deref();
                let Some(readings) = texts.readings(leaf_index) else {
                    continue;
                };
                let read = readings.rule;
                if read.is_some_and(|r| !r.mode_row) {
                    texts.note_drift(leaf_index, session, version);
                }
                let on_glass = read
                    .map(|r| r.rule)
                    .filter(|&r| !self.chrome_owns_terminal_row(wid, row_off + r));
                let rule = on_glass.and_then(|r| {
                    let frame_row = strip + row_off + r;
                    let (run, rule) = texts.rule_run(
                        leaf_index,
                        &ws.input_scratch,
                        frame_row,
                        (col_off, pane_cols),
                        &mut lists.mask,
                    )?;
                    Some((frame_row, row_off + r, run, rule))
                });
                if on_glass.is_some() && rule.is_none() {
                    lists.bare.push(session);
                }
                let harness_on = view.carries() && supervised;
                let switch = crate::autopilot::HarnessSwitch::of(view).map(|face| {
                    let (face, hover) = ws.claude_lights.switch_shown(session, face);
                    let version = (agent == Some(aterm_phase::Program::Agy))
                        .then(|| aterm_phase::Program::Agy.harness_version())
                        .flatten();
                    SwitchPaint {
                        face,
                        hover,
                        version,
                    }
                });
                let unhosted_on = self.unhosted_glass(agent);
                let theme = crate::autopilot::harness_theme(view, supervised, word, unhosted_on);
                let border = theme.filter(|_| switch.is_some() && !agy_switch_off);
                let gate = agent
                    .is_some_and(|prog| relabels_pill(prog, border.is_some()) && !agy_switch_off);
                let mut relabelled = false;
                let top_rule = readings.reach.and_then(|r| r.rules()).map(|(top, _)| top);
                if let Some(
                    prog @ (aterm_phase::Program::Claude
                    | aterm_phase::Program::Agy
                    | aterm_phase::Program::Muse),
                ) = agent
                {
                    let rule_cell = rule.as_ref().map_or_else(
                        || RenderCell {
                            fg: mix(blank.bg, blank.fg, 0.35),
                            ..blank
                        },
                        |(_, _, _, rc)| *rc,
                    );
                    self.splice_top_line(
                        ws,
                        wid,
                        texts,
                        lists,
                        &TopLineAt {
                            leaf_index,
                            session,
                            program_pgid,
                            program: identified.unwrap_or(prog),
                            build: top_build(identified.unwrap_or(prog), agent),
                            region,
                            pane,
                            blank,
                            switch,
                            palette: border,
                            held: true,
                            cell,
                            mark_fg,
                        },
                    );
                    if harness_on
                        && matches!(prog, aterm_phase::Program::Agy | aterm_phase::Program::Muse)
                        && let Some(Some(cached)) = texts.panes.get(leaf_index)
                    {
                        collect_agy_transcript_edits(
                            &ws.input_scratch,
                            prog,
                            &cached.rows,
                            region,
                            top_rule,
                            blank,
                            rule_cell,
                            mark_fg,
                            &mut lists.agy_edits,
                        );
                    }
                    if matches!(
                        prog,
                        aterm_phase::Program::Claude | aterm_phase::Program::Agy
                    ) && (prog == aterm_phase::Program::Claude || harness_on)
                        && !agy_switch_off
                        && let Some(palette) = theme
                        && self.config.claude_footer_agents()
                        && let Some((_, bottom)) = readings.reach.and_then(|r| r.rules())
                        && let Some(read) = texts.agents_panel(
                            leaf_index,
                            session,
                            bottom,
                            &ws.input_scratch,
                            crate::claude_card::PaneGeom {
                                strip,
                                row_off,
                                col_off,
                                rows: pane_rows,
                                cols: pane_cols,
                            },
                            blank,
                        )
                    {
                        lists.panels.push(crate::claude_panel::PanelLeaf {
                            leaf_index,
                            session,
                            pane: crate::claude_card::PaneGeom {
                                strip,
                                row_off,
                                col_off,
                                rows: pane_rows,
                                cols: pane_cols,
                            },
                            blank,
                            palette,
                            read,
                            flights: flights.clone(),
                            skip: 0,
                        });
                    }
                    if gate
                        && let Some(relabel) = readings.relabel
                        && let Some(Some(cached)) = texts.panes.get(leaf_index)
                    {
                        relabelled = collect_pill_relabel(
                            &ws.input_scratch,
                            &cached.rows,
                            region,
                            relabel,
                            |term_row| self.chrome_owns_terminal_row(wid, term_row),
                            blank,
                            &mut lists.relabels,
                            &mut lists.relabel_cells,
                        );
                    }
                }
                lists.seen.push(SeenPane {
                    leaf_index,
                    session,
                    lights: readings.lights,
                });
                let Some((frame_row, term_row, run, rule)) = rule.filter(|_| !agy_switch_off)
                else {
                    continue;
                };
                let footer = texts.footer(leaf_index, session);
                let draft = footer.note.is_some().then(|| {
                    let (screen, empty) = self.claude_composer_now(session)?;
                    (!screen.busy).then_some(!empty)
                });
                footer.lay_upgrade(draft.flatten());
                lists.panes.push(FooterPane {
                    session,
                    leaf_index,
                    frame_row,
                    term_row,
                    col_off,
                    run,
                    rule,
                    blank,
                    autopilot: (word, view),
                    relabelled,
                });
            }
            let seen = &lists.seen;
            let top_only = &lists.top_only;
            ws.claude_footer_texts.borrow_mut().retain_active(
                |index| seen.iter().any(|pane| pane.leaf_index == index),
                |index| cards.iter().any(|card| card.reads(index)),
                |index| top_only.contains(&index),
            );
        }
        for pane in lists.seen.drain(..) {
            self.observe_claude_lights(wid, pane.session, pane.lights);
        }
        for session in lists.bare.drain(..) {
            self.note_claude_footer_row(wid, session, 0);
        }
        self.compose_claude_cards(wid, cards);
        self.compose_claude_panels(wid, &mut lists.panels);
        let lines = self.windows.get(&wid).is_some_and(|ws| {
            ws.claude_card.lines().next().is_some()
                || ws.claude_card.notice_edits().next().is_some()
        });
        if lists.panes.is_empty()
            && !lines
            && lists.agy_edits.is_empty()
            && lists.relabels.is_empty()
            && lists.tops.is_empty()
            && lists.top_hits.is_empty()
            && lists.border_toggles.is_empty()
        {
            return;
        }
        for pane in lists.panes.drain(..) {
            let room = footer::rule_room(&pane.run);
            let Some(chips_room) = self.windows.get(&wid).and_then(|ws| {
                let mut texts = ws.claude_footer_texts.borrow_mut();
                Some(texts.footer_mut(pane.leaf_index)?.chips_room(room))
            }) else {
                continue;
            };
            self.note_claude_footer_row(wid, pane.session, chips_room);
            self.note_claude_autopilot(wid, pane.session, pane.autopilot, pane.relabelled);
            self.note_claude_team(wid, pane.session);
            let block = self.claude_lights_block(
                wid,
                pane.session,
                pane.leaf_index,
                pane.blank,
                chips_room,
            );
            let composed = self.windows.get(&wid).and_then(|ws| {
                let lit = ws.claude_lights.lit_fact(pane.session);
                let mut texts = ws.claude_footer_texts.borrow_mut();
                Some(texts.footer_mut(pane.leaf_index)?.compose(
                    room,
                    block.as_ref(),
                    pane.rule,
                    pane.blank,
                    mark_fg,
                    lit,
                ))
            });
            let Some(Composed {
                cells,
                lights_at,
                title,
                glass,
                facts_at,
                spans,
                account,
            }) = composed.filter(|c| c.cells > 0)
            else {
                if let Some(block) = block {
                    self.recycle_claude_lights_block(wid, block);
                }
                continue;
            };
            let col = pane.col_off + pane.run.end - 1 - cells;
            let edit_col = if glass.is_empty() {
                col
            } else {
                col + glass.end
            };
            let at = col + facts_at + 1;
            lists
                .facts
                .extend(spans.iter().flatten().map(|span| FactWrite {
                    frame_row: pane.frame_row,
                    term_row: pane.term_row,
                    edit_col,
                    session: pane.session,
                    cols: (
                        at + usize::from(span.cells.0),
                        at + usize::from(span.cells.1),
                    ),
                    fact: crate::claude_lights::Fact::of(span.kind),
                    restores: span.restores,
                }));
            if let Some((start, end)) = account {
                lists.facts.push(FactWrite {
                    frame_row: pane.frame_row,
                    term_row: pane.term_row,
                    edit_col,
                    session: pane.session,
                    cols: (at + usize::from(start), at + usize::from(end)),
                    fact: crate::claude_lights::Fact::Account,
                    restores: FactMask::default(),
                });
            }
            match (block, lights_at) {
                (Some(block), Some(at)) => lists.lit.push(Lit {
                    session: pane.session,
                    frame_row: pane.frame_row,
                    col,
                    term_row: pane.term_row,
                    lights_col: col + at,
                    title: col + title.start..col + title.end,
                    block,
                    shown: false,
                }),
                (Some(block), None) => self.recycle_claude_lights_block(wid, block),
                (None, _) => {}
            }
            lists.rules.push(RuleWrite {
                frame_row: pane.frame_row,
                col,
                leaf_index: pane.leaf_index,
                ground: pane.blank,
                glass,
            });
        }
        {
            let Some(ws) = self.windows.get_mut(&wid) else {
                return;
            };
            let crate::WindowState {
                claude_footer_undo,
                input_scratch,
                claude_card,
                claude_footer_texts,
                claude_lights,
                pet_chrome,
                ..
            } = &mut **ws;
            let texts = claude_footer_texts.borrow();
            let card_edits = claude_card
                .lines()
                .map(crate::claude_card::CardLine::edit)
                .chain(claude_card.notice_edits());
            let rules = lists.rules.iter().flat_map(|r| {
                rule_edits(
                    r.frame_row,
                    r.col,
                    texts.composed(r.leaf_index),
                    &r.glass,
                    r.ground,
                )
            });
            let tops = lists.tops.iter().flat_map(|t| {
                rule_edits(
                    t.frame_row,
                    t.col,
                    texts.top_cells(t.leaf_index),
                    &t.glass,
                    t.ground,
                )
            });
            let agy = lists
                .agy_edits
                .iter()
                .map(|e| (e.frame_row, e.col, e.cells.as_slice(), e.ground));
            let relabel_cells = &lists.relabel_cells;
            let relabels = lists.relabels.iter().map(|e| {
                (
                    e.frame_row,
                    e.col,
                    &relabel_cells[e.cells.clone()],
                    e.ground,
                )
            });
            write_edits(
                claude_footer_undo,
                input_scratch,
                card_edits
                    .chain(rules)
                    .chain(tops)
                    .chain(agy)
                    .chain(relabels),
            );
            for e in &lists.relabels {
                if claude_footer_undo.took(e.frame_row, e.col) {
                    claude_footer_undo.glass.push(e.shift);
                }
            }
            for t in &lists.tops {
                if claude_footer_undo.took(t.frame_row, t.col)
                    && let Some(layer) = texts.top_mark(t.leaf_index)
                {
                    claude_card.paint_layer(input_scratch, layer.clone());
                }
            }
            for r in claude_footer_undo.written() {
                let vendor = lists
                    .agy_edits
                    .iter()
                    .map(|e| (e.frame_row, e.col))
                    .chain(lists.relabels.iter().map(|e| (e.frame_row, e.col)))
                    .any(|at| at == (r.frame_row, r.col_off));
                if !vendor {
                    claude_card.paint_marks(input_scratch, r.frame_row, r.col_off, &r.painted);
                }
            }
            for m in lists.marks.drain(..) {
                if claude_footer_undo.took(m.frame_row, m.edit_col) {
                    claude_lights.mark_hits.push(crate::claude_lights::MarkHit {
                        frame_row: m.frame_row,
                        term_row: m.term_row,
                        cols: m.cols,
                        session: m.session,
                    });
                }
            }
            for r in &lists.rules {
                if let Some(footer) = texts.footers.get(r.leaf_index) {
                    claude_footer_undo.publish_text(
                        input_scratch,
                        r.frame_row,
                        r.col,
                        &footer.rule.paint,
                    );
                }
            }
            let rows = claude_footer_undo.written();
            for r in rows {
                if !lists
                    .agy_edits
                    .iter()
                    .any(|e| e.frame_row == r.frame_row && e.col == r.col_off)
                {
                    pet_chrome.note_painted(r.frame_row, r.col_off, &r.painted);
                }
            }
            for (row, col, rows, cols) in claude_card.glass_boxes() {
                pet_chrome.note(row, col, rows, cols);
            }
            let took = |at: (usize, usize)| rows.iter().any(|r| (r.frame_row, r.col_off) == at);
            for l in &mut lists.lit {
                l.shown = took((l.frame_row, l.col));
            }
            for f in lists.facts.drain(..) {
                if took((f.frame_row, f.edit_col)) {
                    claude_lights.fact_hits.push(crate::claude_lights::FactHit {
                        frame_row: f.frame_row,
                        term_row: f.term_row,
                        cols: f.cols,
                        session: f.session,
                        fact: f.fact,
                        restores: f.restores,
                    });
                }
            }
            for h in lists.top_hits.drain(..) {
                let shown = if h.ghost {
                    rows.iter().any(|r| {
                        r.frame_row == h.frame_row
                            && r.col_off <= h.cols.0
                            && r.col_off + r.painted.len() >= h.cols.1
                    })
                } else {
                    input_scratch
                        .cells
                        .get(h.frame_row)
                        .and_then(|row| row.get(h.cols.0..h.cols.1))
                        .is_some_and(|cells| {
                            cells
                                .iter()
                                .map(|c| c.ch)
                                .eq(aterm_primer::CLAUDE_TOP_EFFORT_KEY.chars())
                        })
                };
                if shown {
                    claude_lights.fact_hits.push(crate::claude_lights::FactHit {
                        frame_row: h.frame_row,
                        term_row: h.term_row,
                        cols: h.cols,
                        session: h.session,
                        fact: crate::claude_lights::Fact::TopEffort,
                        restores: FactMask::default(),
                    });
                }
            }
            for b in lists.border_toggles.drain(..) {
                if rows.iter().any(|r| {
                    r.frame_row == b.frame_row
                        && r.col_off <= b.cols.start
                        && r.col_off + r.painted.len() >= b.cols.end
                }) {
                    claude_lights.border_toggles.push(b);
                }
            }
            for l in lists.lit.iter().filter(|l| l.shown && !l.title.is_empty()) {
                claude_lights
                    .title_hits
                    .push(crate::claude_lights::TitleHit {
                        frame_row: l.frame_row,
                        term_row: l.term_row,
                        cols: l.title.clone(),
                        session: l.session,
                    });
            }
            crate::claude_card::ink_lines(claude_card, input_scratch, took);
        }
        for l in lists.lit.drain(..) {
            if l.shown {
                self.note_claude_light_hits(
                    wid,
                    l.session,
                    l.frame_row,
                    l.term_row,
                    l.lights_col,
                    &l.block,
                );
            }
            self.recycle_claude_lights_block(wid, l.block);
        }
    }

    /// Handles `Wake::ClaudeFooter` by redrawing windows displaying `session`.
    pub(crate) fn on_claude_footer_changed(&mut self, session: u64) {
        self.drain_claude_lights();
        self.refresh_fact_readings(session);
        self.refresh_fact_menu(session);
        self.note_claude_build_drift(session);
        self.redraw_claude_footer(session);
    }

    /// Marks windows displaying `session` for redraw when footer-visible state changes.
    pub(crate) fn redraw_claude_footer(&self, session: u64) {
        let mut windows = self.windows_with_focused_session(session);
        for (wid, _) in self.tabs_viewing_session(session) {
            if !windows.contains(&wid) {
                windows.push(wid);
            }
        }
        for wid in windows {
            if let Some(window) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref()) {
                window.request_redraw();
            }
        }
    }
}

/// A pane's footer reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RuleRead {
    rule: usize,
    mode_row: bool,
}

type FooterRead = Option<RuleRead>;

/// The footer's reading of a pane's rows: the composer's bottom rule
/// (`aterm_phase::phase::composer_bottom`) and the mode row under it
/// (`footer::mode_row_under`).
#[cfg(test)]
fn read_rule_row(rows: &[String]) -> FooterRead {
    read_rule_under(rows, aterm_phase::phase::FrameReach::of(rows))
}

/// `read_rule_row` for a frame already found (`reach`).
fn read_rule_under(rows: &[String], reach: aterm_phase::phase::FrameReach) -> FooterRead {
    reach.rules().map(|(_, rule)| RuleRead {
        rule,
        mode_row: footer::mode_row_under(rows, rule).is_some(),
    })
}

/// Log a mode row this aterm cannot read, once per Claude Code build and row head, up to
/// [`DRIFT_ROWS_LOGGED`] in a run.
fn note_vendor_drift(session: u64, version: Option<&str>, rows: &[String]) {
    static LOGGED: Mutex<Vec<String>> = Mutex::new(Vec::new());
    let unread =
        crate::reader_guard::read_or_none(session, "Claude Code drift check", rows, || {
            unread_pill_row(rows)
        })
        .flatten();
    let Some(row) = unread else {
        return;
    };
    let build = version.unwrap_or("(unknown build)");
    let head: String = row.split_whitespace().take(3).collect::<Vec<_>>().join(" ");
    let key = format!("{build}\t{head}");
    let mut logged = LOGGED.lock().unwrap_or_else(|p| p.into_inner());
    if logged.contains(&key) || logged.len() >= DRIFT_ROWS_LOGGED {
        return;
    }
    logged.push(key);
    aterm_log::warn!(
        "claude footer: Claude Code {build} drew a mode row this aterm does not read, so the lights stand down on it: {row:?}"
    );
}

/// How many distinct unreadable rows one run logs, across every build.
const DRIFT_ROWS_LOGGED: usize = 16;

/// The row under the composer that opens with a pill glyph but names no pill this build knows.
fn unread_pill_row(rows: &[String]) -> Option<String> {
    let (_, bottom) = aterm_phase::phase::composer_rules(rows)?;
    let row = rows
        .get(bottom + 1..)?
        .iter()
        .take_while(|r| !r.trim().is_empty())
        .find(|r| footer::opens_with_pill_glyph(r))?;
    (footer::pill_indicator(row).is_none() && !footer::is_cut_pill(row))
        .then(|| row.trim_end().to_owned())
}

/// The harness switch's letter at `t` ([`legible_on`]).
fn brand_letter_fg(
    palette: aterm_effects::harness_card::CardPalette,
    t: f32,
    blank: RenderCell,
) -> [u8; 3] {
    let bg = u32::from_be_bytes([0, blank.bg[0], blank.bg[1], blank.bg[2]]);
    let rgb = |c: u32| [(c >> 16) as u8, (c >> 8) as u8, c as u8];
    if aterm_effects::harness_card::dark_ground(bg) {
        legible_on(mix(rgb(palette.letter(t, true)), blank.fg, 0.25), blank.bg)
    } else {
        rgb(palette.letter_on(t, bg))
    }
}

/// A composer top rule's plain run ([`read_top_run`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TopRun {
    col: usize,
    len: usize,
    rule: RenderCell,
    /// Muse Code's rule at rest is labelled ([`laid_run`]).
    voice: Option<VoiceRule>,
}

/// Muse Code's whole idle voice rule ([`TopRun::voice`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct VoiceRule {
    col: usize,
    len: usize,
    label: usize,
}

/// The run a top line is laid on ([`compose_top`]).
fn laid_run(key: &TopKey) -> (TopRun, usize) {
    let run = key.run;
    let lockup = cli_mark(key.program).map(|mark| mark.lockup_cells(key.build));
    let whole = run.voice.filter(|voice| {
        lay_top(top_span(voice.len, key.face), key.switch, lockup)
            .is_some_and(|(rung, _)| rung.lockup.is_some())
    });
    match whole {
        Some(VoiceRule { col, len, label }) => (TopRun { col, len, ..run }, label),
        None => (run, 0),
    }
}

/// The ghost's first cell in a run of `len` cells under `face`, where it is drawn.
fn ghost_at(len: usize, face: KeyFace) -> Option<usize> {
    face.ghost
        .filter(|_| len >= GHOST_RUN)
        .map(|_| len - GHOST_CELLS - 1)
}

/// The harness's span in a run of `len` cells under `face` ([`ghost_at`]).
fn top_span(len: usize, face: KeyFace) -> usize {
    ghost_at(len, face).unwrap_or(len)
}

/// What a CLI's top line is drawn as ([`top_line`], [`read_top_run`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TopLine {
    /// A `─` rule on the pane's ground.
    Rule,
    /// A tinted pad row: Codex's composer slab's top pad.
    Slab,
}

/// Where `program`'s top line is on a pane's cached rows, and its kind.
fn top_line(program: aterm_phase::Program, cached: &CachedPaneText) -> Option<(usize, TopLine)> {
    use aterm_phase::Program;
    let session = cached.key.session;
    match program {
        Program::Claude | Program::Agy | Program::Muse => cached
            .readings
            .reach
            .and_then(|r| r.rules())
            .map(|(top, _)| (top, TopLine::Rule)),
        Program::Gemini => cached
            .gemini_top
            .get_or_init(|| {
                crate::reader_guard::read_or_none(
                    session,
                    "Gemini CLI top rule",
                    &cached.rows,
                    || aterm_phase::gemini::top_rule(&cached.rows),
                )
                .flatten()
            })
            .map(|row| (row, TopLine::Rule)),
        Program::Codex => cached
            .codex_pad
            .get_or_init(|| {
                crate::reader_guard::read_or_none(
                    session,
                    "Codex composer pad",
                    &cached.rows,
                    || aterm_phase::codex::composer_pad(&cached.rows),
                )
                .flatten()
            })
            .map(|row| (row, TopLine::Slab)),
        Program::Grok | Program::Kimi | Program::Generic => None,
    }
}

/// Whether a selection reaches the pane's columns of frame row `frame_row`
/// ([`App::splice_top_line`]).
fn top_line_selected(scratch: &RenderInput, frame_row: usize, region: PaneTextRegion) -> bool {
    scratch
        .selection_row_span(frame_row)
        .is_some_and(|(lo, hi)| {
            usize::from(lo) < region.col_off + region.cols && usize::from(hi) >= region.col_off
        })
}

/// The rightmost plain run of the composer's top line.
fn read_top_run(
    scratch: &RenderInput,
    region: PaneTextRegion,
    top_rule: usize,
    line: TopLine,
    blank: RenderCell,
    muse: bool,
) -> Option<TopRun> {
    #[cfg(test)]
    crate::work_counts::footer_top_read();
    let frame_row = region.strip + region.row_off + top_rule;
    let frame_cells = scratch.cells.get(frame_row)?;
    let pane_end = frame_cells.len().min(region.col_off + region.cols);
    let pane_slice = frame_cells.get(region.col_off..pane_end)?;
    if line == TopLine::Slab {
        let slab = *pane_slice.first()?;
        let prompt = scratch
            .cells
            .get(frame_row + 1)?
            .get(region.col_off)
            .filter(|_| top_rule + 1 < region.rows)?;
        let span = region.col_off..region.col_off + region.cols;
        let proven = pane_slice.len() == region.cols
            && slab.ch == ' '
            && slab.underline == aterm_core::terminal::UnderlineStyle::None
            && slab.bg != blank.bg
            && prompt.bg == slab.bg
            && pane_slice.iter().all(|c| *c == slab)
            && !touches(&scratch.clusters, frame_row, &span)
            && !touches(&scratch.combining, frame_row, &span);
        let end = scratch
            .images
            .get(frame_row)
            .into_iter()
            .flatten()
            .map(|(col, _)| *col)
            .filter(|col| span.contains(col))
            .min()
            .unwrap_or(span.end);
        let pack = |c: [u8; 3]| u32::from_be_bytes([0, c[0], c[1], c[2]]);
        let reads = |c: [u8; 3]| aterm_effects::color_math::contrast_ratio(pack(c), pack(slab.bg));
        let ink = if reads(blank.bg) > reads(blank.fg) {
            blank.bg
        } else {
            blank.fg
        };
        return (proven && end > region.col_off).then_some(TopRun {
            col: region.col_off,
            len: end - region.col_off,
            rule: RenderCell { fg: ink, ..slab },
            voice: None,
        });
    }
    if pane_slice.is_empty() || pane_slice.iter().any(|c| c.bg != blank.bg) {
        return None;
    }
    let first = pane_slice.iter().find(|c| c.ch == '\u{2500}').copied()?;
    let last = pane_slice
        .iter()
        .rev()
        .find(|c| c.ch == '\u{2500}')
        .copied()
        .unwrap_or(first);
    let count = |style: &RenderCell| pane_slice.iter().filter(|c| *c == style).count();
    let rule = if first == last || count(&last) >= count(&first) {
        last
    } else {
        first
    };
    let mut best = 0..0;
    let mut cur_start = None;
    for (idx, cell) in pane_slice.iter().enumerate() {
        let col = region.col_off + idx;
        let plain = *cell == rule
            && !touches(&scratch.clusters, frame_row, &(col..col + 1))
            && !touches(&scratch.combining, frame_row, &(col..col + 1))
            && !touches(&scratch.images, frame_row, &(col..col + 1));
        if plain {
            let s = *cur_start.get_or_insert(idx);
            if idx + 1 - s >= best.len() {
                best = s..idx + 1;
            }
        } else {
            cur_start = None;
        }
    }
    let untouched = |idx: usize| {
        let col = region.col_off + idx;
        !touches(&scratch.clusters, frame_row, &(col..col + 1))
            && !touches(&scratch.combining, frame_row, &(col..col + 1))
            && !touches(&scratch.images, frame_row, &(col..col + 1))
    };
    let voice = muse
        .then(|| {
            let first = pane_slice.iter().position(|c| c.ch == '\u{2500}')?;
            let last = pane_slice.iter().rposition(|c| c.ch == '\u{2500}')?;
            let ink = pane_slice
                .iter()
                .rposition(|c| c.ch != '\u{2500}' && c.ch != ' ')?;
            let label_end = (ink + 2).min(pane_slice.len());
            let end = (last + 1).max(label_end);
            (first < ink
                && (first..end).all(untouched)
                && aterm_phase::muse::is_idle_voice_rule(
                    &pane_slice.iter().map(|c| c.ch).collect::<String>(),
                ))
            .then_some(VoiceRule {
                col: region.col_off + first,
                len: end - first,
                label: label_end - first,
            })
        })
        .flatten();
    Some(TopRun {
        col: region.col_off + best.start,
        len: best.len(),
        rule,
        voice,
    })
}

/// The harness switch's paint on one pane's composer top rule ([`compose_top`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SwitchPaint {
    face: crate::autopilot::HarnessSwitch,
    hover: bool,
    version: Option<&'static str>,
}

/// How one cell of a harness switch's face is inked ([`switch_face`], [`switch_cell`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SwitchInk {
    /// A glyph of the switch's words and state on a line that wears a theme
    /// ([`SwitchFace::on_arc`]).
    Arc(u8),
    /// The switch's label (`alab harness`), and the face's blanks.
    Label,
    /// The lit state, `● on`: the accent.
    Lit,
    /// The state the switch is in when off, `○ off`.
    Unlit,
    /// A letter of the `on` button.
    Button,
    /// A cap of the button (`▐`, `▌`).
    Cap,
    /// `◐ turning on`: the accent, quieted and floored to 4.5:1 ([`turning_ink`]).
    Turning,
}

/// The cells a switch face may take.
const SWITCH_FACE_MAX: usize = 38;

/// One harness switch face as laid ([`switch_face`]): its cells and where its parts lie in them.
#[derive(Clone, Debug, PartialEq, Eq)]
struct SwitchFace {
    cells: [(char, SwitchInk); SWITCH_FACE_MAX],
    len: usize,
    /// The switch's own cells: its label and its state, a lit pill included.
    switch: std::ops::Range<usize>,
    /// The off face's `on` button ([`push_on_button`]).
    button: Option<std::ops::Range<usize>>,
}

impl SwitchFace {
    fn cells(&self) -> &[(char, SwitchInk)] {
        &self.cells[..self.len]
    }

    /// The face's text, for the tests.
    #[cfg(test)]
    fn text(&self) -> String {
        self.cells().iter().map(|(ch, _)| *ch).collect()
    }

    fn push(&mut self, text: &str, ink: SwitchInk) {
        for ch in text.chars() {
            if self.len < SWITCH_FACE_MAX {
                self.cells[self.len] = (ch, ink);
                self.len += 1;
            }
        }
    }

    /// The face on a line that wears a theme ([`SwitchInk::Arc`], [`compose_top`]):
    /// the arc runs through its words, a lit pill's caps and fill keeping their inks.
    fn on_arc(&mut self) {
        let span = self.switch.clone();
        let arcs = |&(ch, ink): &(char, SwitchInk)| {
            ch != ' ' && !matches!(ink, SwitchInk::Cap | SwitchInk::Button)
        };
        let n = self.cells[span.clone()].iter().filter(|&c| arcs(c)).count();
        let mut k = 0usize;
        for cell in &mut self.cells[span] {
            if !arcs(&*cell) {
                continue;
            }
            let ink = &mut cell.1;
            let stop = if n > 1 { k * 255 / (n - 1) } else { 0 };
            *ink = SwitchInk::Arc(u8::try_from(stop).unwrap_or(u8::MAX));
            k += 1;
        }
    }
}

/// How many forms a switch face has, widest first ([`switch_face`]).
const SWITCH_TIERS: usize = 4;

/// The harness switch's face at `tier`, `None` past its narrowest.
fn switch_face(paint: SwitchPaint, tier: usize) -> Option<SwitchFace> {
    use crate::autopilot::HarnessSwitch;
    let mut f = SwitchFace {
        cells: [(' ', SwitchInk::Label); SWITCH_FACE_MAX],
        len: 0,
        switch: 0..0,
        button: None,
    };
    let label = |f: &mut SwitchFace, words: usize| {
        if words >= 2 {
            f.push("alab harness ", SwitchInk::Label);
            if let Some(ver) = paint.version {
                f.push("v", SwitchInk::Label);
                f.push(ver, SwitchInk::Label);
                f.push(" ", SwitchInk::Label);
            }
        } else if words == 1 {
            f.push("alab ", SwitchInk::Label);
        }
    };
    match paint.face {
        HarnessSwitch::On => {
            let words = [2, 1, 0].get(tier).copied()?;
            f.push(" ", SwitchInk::Label);
            f.switch.start = f.len;
            label(&mut f, words);
            // A versioned face draws its lit state as a pill: the switch's
            // own state, so its cells are the switch's, never the off
            // face's `on` button.
            if paint.version.is_some() {
                f.push("\u{2590}", SwitchInk::Cap);
                f.push(" \u{25cf} on ", SwitchInk::Button);
                f.push("\u{258c}", SwitchInk::Cap);
            } else {
                f.push("\u{25cf} on", SwitchInk::Lit);
            }
            f.switch.end = f.len;
            f.push(" ", SwitchInk::Label);
        }
        HarnessSwitch::Off(_) => {
            let words = [2, 1, 0, 0].get(tier).copied()?;
            f.push(" ", SwitchInk::Label);
            f.switch.start = f.len;
            label(&mut f, words);
            if paint.version.is_some() {
                if tier < 3 {
                    f.push("\u{25cb} off", SwitchInk::Unlit);
                    f.switch.end = f.len;
                    f.push(" ", SwitchInk::Label);
                } else {
                    f.switch.end = f.len;
                }
                let at = f.len;
                f.push("\u{2590}", SwitchInk::Cap);
                f.push(" \u{25cf} on ", SwitchInk::Button);
                f.push("\u{258c}", SwitchInk::Cap);
                f.button = Some(at..f.len);
            } else {
                if tier < 3 {
                    f.push("\u{25cb} off", SwitchInk::Unlit);
                    f.switch.end = f.len;
                    f.push(" ", SwitchInk::Label);
                } else {
                    f.switch.end = f.len;
                }
                let at = f.len;
                push_on_button(&mut f);
                f.button = Some(at..f.len);
            }
            f.push(" ", SwitchInk::Label);
        }
        HarnessSwitch::Turning => {
            let words = [2, 1, 0, 0].get(tier).copied()?;
            f.push(" ", SwitchInk::Label);
            f.switch.start = f.len;
            label(&mut f, words);
            f.push(
                if tier < 3 {
                    "\u{25d0} turning on"
                } else {
                    "\u{25d0} on"
                },
                SwitchInk::Turning,
            );
            f.switch.end = f.len;
            f.push(" ", SwitchInk::Label);
        }
    }
    Some(f)
}

/// The off switch's `on` button ([`switch_face`]).
fn push_on_button(f: &mut SwitchFace) {
    f.push("\u{2590}", SwitchInk::Cap);
    f.push(" on ", SwitchInk::Button);
    f.push("\u{258c}", SwitchInk::Cap);
}

/// The widest of `paint`'s faces that fits `room` cells with two of the rule's own on either
/// side, and its tier: the switch alone, as [`lay_top`]'s lockup-less rungs lay it.
#[cfg(test)]
fn fitted_switch_face(paint: SwitchPaint, room: usize) -> Option<(usize, SwitchFace)> {
    (0..SWITCH_TIERS)
        .filter_map(|tier| Some((tier, switch_face(paint, tier)?)))
        .find(|(_, face)| face.len + 4 <= room)
}

/// A face cell in its ink on `blank`'s ground ([`SwitchInk::Arc`], [`brand_letter_fg`]).
fn switch_cell(
    ch: char,
    ink: SwitchInk,
    paint: SwitchPaint,
    palette: Option<aterm_effects::harness_card::CardPalette>,
    blank: RenderCell,
    mark_fg: [u8; 3],
) -> RenderCell {
    use crate::autopilot::HarnessSwitch;
    let accent = || legible_on(mark_fg, blank.bg);
    let quiet = || legible_on(mix(blank.fg, blank.bg, 0.3), blank.bg);
    let agent = || legible_on(mix(accent(), blank.fg, 0.35), blank.bg);
    let fill = || {
        let accent = accent();
        if paint.hover {
            legible_on(mix(accent, blank.fg, 0.25), blank.bg)
        } else {
            accent
        }
    };
    let on = paint.face.is_on();
    let mut cell = RenderCell { ch, ..blank };
    match ink {
        SwitchInk::Arc(stop) => {
            let letter = brand_letter_fg(
                palette.unwrap_or(aterm_effects::harness_card::CardPalette::Cruise),
                f32::from(stop) / 255.0,
                blank,
            );
            match paint.face {
                HarnessSwitch::On => {
                    cell.fg = letter;
                    cell.bold = true;
                }
                HarnessSwitch::Off(_) => cell.fg = letter,
                HarnessSwitch::Turning => cell.fg = turning_ink(letter, blank.bg),
            }
        }
        SwitchInk::Label => {
            cell.fg = if on { agent() } else { quiet() };
            cell.bold = on;
        }
        SwitchInk::Lit => {
            cell.fg = accent();
            cell.bold = true;
        }
        SwitchInk::Unlit => cell.fg = quiet(),
        SwitchInk::Button => {
            cell.fg = blank.bg;
            cell.bg = fill();
            cell.bold = true;
            if paint.hover && ch != ' ' {
                cell.underline = aterm_core::terminal::UnderlineStyle::Single;
            }
        }
        SwitchInk::Cap => cell.fg = fill(),
        SwitchInk::Turning => cell.fg = turning_ink(accent(), blank.bg),
    }
    if paint.hover
        && on
        && matches!(ink, SwitchInk::Label | SwitchInk::Lit | SwitchInk::Arc(_))
        && ch != ' '
    {
        cell.bold = true;
        cell.underline = aterm_core::terminal::UnderlineStyle::Single;
    }
    cell
}

/// The settling switch's ink (`◐ turning on`, [`switch_cell`]): `ink` quieted 30% toward the
/// ground, then floored back to 4.5:1 against it.
fn turning_ink(ink: [u8; 3], ground: [u8; 3]) -> [u8; 3] {
    legible_on(mix(ink, ground, 0.3), ground)
}

/// Adjusts `ink` along its hue to reach at least 4.5:1 contrast against `ground`.
pub(crate) fn legible_on(ink: [u8; 3], ground: [u8; 3]) -> [u8; 3] {
    floored_on(ink, ground, aterm_effects::cli_marks::NAME_CONTRAST)
}

/// Adjusts `ink` along its hue to reach at least 3:1 contrast against `ground`.
pub(crate) fn visible_on(ink: [u8; 3], ground: [u8; 3]) -> [u8; 3] {
    floored_on(ink, ground, aterm_effects::cli_marks::ICON_CONTRAST)
}

/// `ink` floored along its own hue to read `min`:1 on `ground`.
fn floored_on(ink: [u8; 3], ground: [u8; 3], min: f32) -> [u8; 3] {
    rgb_of(aterm_effects::color_math::floor_contrast_keeping_hue(
        pack_rgb(ink),
        pack_rgb(ground),
        min,
    ))
}

/// The contrast `ink` reads at on `ground` (WCAG).
fn reads_on(ink: [u8; 3], ground: [u8; 3]) -> f32 {
    aterm_effects::color_math::contrast_ratio(pack_rgb(ink), pack_rgb(ground))
}

/// `c` as `0x00RRGGBB`.
const fn pack_rgb(c: [u8; 3]) -> u32 {
    u32::from_be_bytes([0, c[0], c[1], c[2]])
}

/// The facts' word inks on a pane, made from what the pane itself gives the glass.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FactInks {
    /// A value at rest ([`VALUE_LIFT`], [`value_fg`], [`legible_on`]).
    value: [u8; 3],
    /// A lit value: the pane's ink, floored to 4.5:1 ([`legible_on`]).
    lit: [u8; 3],
    /// A word greyed on purpose ([`greyed_on`]).
    stub: [u8; 3],
}

impl FactInks {
    /// The inks under a rule drawn in `gray`, on a pane whose ink is `ink`, laid on `ground`.
    fn of(gray: [u8; 3], ink: [u8; 3], ground: [u8; 3]) -> Self {
        let value = legible_on(value_fg(gray, ink), ground);
        Self {
            value,
            lit: legible_on(ink, ground),
            stub: greyed_on(gray, value, ground),
        }
    }
}

/// [`FactInks::stub`] under a rule drawn in `gray` on `blank`'s pane.
#[cfg(all(test, unix))]
pub(crate) fn greyed_ink(gray: [u8; 3], blank: RenderCell) -> [u8; 3] {
    FactInks::of(gray, blank.fg, blank.bg).stub
}

/// How far a greyed word is quieted from the live value toward the ground where the program's grey
/// reads no quieter than the value ([`greyed_on`]).
const GREYED_QUIET: f32 = 0.3;

/// A word greyed on purpose on `ground` ([`visible_on`]).
fn greyed_on(gray: [u8; 3], value: [u8; 3], ground: [u8; 3]) -> [u8; 3] {
    let ink = if reads_on(gray, ground) < reads_on(value, ground) {
        gray
    } else {
        mix(value, ground, GREYED_QUIET)
    };
    visible_on(ink, ground)
}

/// The mark a program's top line wears ([`aterm_effects::cli_marks`]).
const fn cli_mark(program: aterm_phase::Program) -> Option<CliMark> {
    match program {
        aterm_phase::Program::Claude => Some(CliMark::Claude),
        aterm_phase::Program::Codex => Some(CliMark::Codex),
        aterm_phase::Program::Agy => Some(CliMark::Agy),
        aterm_phase::Program::Muse => Some(CliMark::Muse),
        aterm_phase::Program::Gemini => Some(CliMark::Gemini),
        aterm_phase::Program::Grok | aterm_phase::Program::Kimi | aterm_phase::Program::Generic => {
            None
        }
    }
}

/// The build a top line's `program` runs ([`CliMark::name`]).
const fn top_build(program: aterm_phase::Program, agent: Option<aterm_phase::Program>) -> Build {
    match (program, agent) {
        (aterm_phase::Program::Codex, Some(aterm_phase::Program::Codex)) => Build::Alab,
        _ => Build::Vendor,
    }
}

/// What a top line's lockup shows on a rung of the ladder ([`Rung`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lockup {
    /// The icon and the CLI's name.
    Full,
    /// The icon alone, `[##]` (`cli_marks::ICON_RUN`).
    Icon,
}

/// One rung of the narrow ladder ([`ladder`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Rung {
    tier: Option<usize>,
    lockup: Option<Lockup>,
    lead: usize,
    need: usize,
}

/// The most rungs a ladder has: a full rung per switch tier, then the icon, the icon compact and
/// the switch alone.
const LADDER_MAX: usize = SWITCH_TIERS + 3;

/// The narrow ladder of a top line whose switch is `switch`, widest rung first ([`lay_top`]).
fn ladder(switch: Option<SwitchPaint>, lockup: Option<usize>) -> ([Rung; LADDER_MAX], usize) {
    let none = Rung {
        tier: None,
        lockup: None,
        lead: 0,
        need: usize::MAX,
    };
    let mut rungs = [none; LADDER_MAX];
    let mut n = 0;
    let mut push = |rung: Rung| {
        rungs[n] = rung;
        n += 1;
    };
    let icon = aterm_effects::cli_marks::ICON_RUN;
    match switch {
        Some(paint) => {
            let mut last = None;
            for tier in 0..SWITCH_TIERS {
                let Some(face) = switch_face(paint, tier) else {
                    break;
                };
                match lockup {
                    Some(cells) => push(Rung {
                        tier: Some(tier),
                        lockup: Some(Lockup::Full),
                        lead: 2,
                        need: cells + face.len + 6,
                    }),
                    None => push(Rung {
                        tier: Some(tier),
                        lockup: None,
                        lead: 0,
                        need: face.len + 4,
                    }),
                }
                last = Some((tier, face.len));
            }
            if let (Some((tier, len)), Some(_)) = (last, lockup) {
                push(Rung {
                    tier: Some(tier),
                    lockup: Some(Lockup::Icon),
                    lead: 2,
                    need: icon + len + 6,
                });
                push(Rung {
                    tier: Some(tier),
                    lockup: Some(Lockup::Icon),
                    lead: 1,
                    need: icon + len + 4,
                });
                push(Rung {
                    tier: Some(tier),
                    lockup: None,
                    lead: 0,
                    need: len + 4,
                });
            }
        }
        None => {
            if let Some(cells) = lockup {
                push(Rung {
                    tier: None,
                    lockup: Some(Lockup::Full),
                    lead: 2,
                    need: cells + 4,
                });
                push(Rung {
                    tier: None,
                    lockup: Some(Lockup::Icon),
                    lead: 2,
                    need: icon + 4,
                });
                push(Rung {
                    tier: None,
                    lockup: Some(Lockup::Icon),
                    lead: 1,
                    need: icon + 2,
                });
            }
        }
    }
    (rungs, n)
}

/// The first of `rungs` a span of `span` cells holds.
fn first_fit(span: usize, rungs: &[Rung]) -> Option<Rung> {
    rungs.iter().copied().find(|r| r.need <= span)
}

/// The first rung of [`ladder`] that fits `span` cells, and its switch face.
fn lay_top(
    span: usize,
    switch: Option<SwitchPaint>,
    lockup: Option<usize>,
) -> Option<(Rung, Option<SwitchFace>)> {
    let (rungs, n) = ladder(switch, lockup);
    let rung = first_fit(span, &rungs[..n])?;
    let face = rung
        .tier
        .zip(switch)
        .and_then(|(tier, paint)| switch_face(paint, tier));
    Some((rung, face))
}

/// What a composer top line is composed from ([`compose_top`], [`top_build`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TopKey {
    frame_row: usize,
    term_row: usize,
    line: TopLine,
    run: TopRun,
    program: aterm_phase::Program,
    build: Build,
    switch: Option<SwitchPaint>,
    palette: Option<aterm_effects::harness_card::CardPalette>,
    blank: RenderCell,
    mark_fg: [u8; 3],
    face: KeyFace,
    cell: (u16, u16),
    clip: aterm_core::render::PxRect,
    mark_hover: bool,
    title_gen: u64,
}

/// What aterm draws for the top-effort key on a top rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct KeyFace {
    /// aterm's ` KEY off ` ghost in Claude's slot, the key off.
    ghost: Option<bool>,
    /// Claude's own tag on the row.
    tag: Option<(usize, usize)>,
    /// `◐` while a press of the key is in flight, `▸` while the keyboard selects it.
    mark: Option<char>,
}

/// The ghost's cells.
const GHOST_CELLS: usize = aterm_primer::CLAUDE_TOP_EFFORT_KEY.len() + 6;

/// The shortest run the ghost is drawn in: itself, the `─` after it and the rule's lead of `─`
/// before it.
const GHOST_RUN: usize = GHOST_CELLS + 1 + footer::RULE_LEAD;

/// The ghost's glyphs, in order.
fn ghost_chars() -> impl Iterator<Item = char> {
    std::iter::once(' ')
        .chain(aterm_primer::CLAUDE_TOP_EFFORT_KEY.chars())
        .chain(" off ".chars())
}

/// A composer top rule as [`PaneFooter::compose_top`] composed it: the cells to write at frame
/// column `col` ([`rule_edits`], [`mark_layer`]).
#[derive(Debug, Default)]
struct ComposedTop {
    key: Option<TopKey>,
    cells: Vec<RenderCell>,
    col: usize,
    /// Indices into `cells`: on a bare line, the vendor's own cells the write leaves out
    /// ([`compose_top`]).
    glass: std::ops::Range<usize>,
    switch: Option<LaidSwitch>,
    ghost: Option<(usize, usize)>,
    lockup: std::ops::Range<usize>,
    icon: Option<usize>,
    mark: Option<aterm_core::render::FxLayer>,
}

/// Where [`compose_top`] laid a harness switch ([`switch_face`]).
#[derive(Debug, Clone, PartialEq, Eq)]
struct LaidSwitch {
    tier: usize,
    cols: std::ops::Range<usize>,
    switch: std::ops::Range<usize>,
    button: Option<std::ops::Range<usize>>,
}

/// The context fact's words for an indicator reading `pct` per cent left.
fn context_words(pct: u8) -> String {
    format!("ctx {pct}% left")
}

/// The reading the context fact's words `shown` say ([`context_words`]).
fn context_left_of(shown: Option<&str>) -> Option<u8> {
    shown?
        .strip_prefix("ctx ")?
        .strip_suffix("% left")?
        .parse()
        .ok()
}

/// A `0x00RRGGBB` colour's channels.
const fn rgb_of(c: u32) -> [u8; 3] {
    [(c >> 16) as u8, (c >> 8) as u8, c as u8]
}

/// Compose the CLI's top line ([`top_line`]).
fn compose_top(key: &TopKey, top: &mut ComposedTop, version: Option<&str>) {
    compose_top_in(key, top, version, TITLE_LEAD);
}

/// The `─` the hover title leaves before the switch.
const TITLE_LEAD: usize = 2;

/// [`compose_top`] with the hover title leaving `title_lead` `─` before the switch.
fn compose_top_in(key: &TopKey, top: &mut ComposedTop, version: Option<&str>, title_lead: usize) {
    #[cfg(test)]
    crate::work_counts::footer_top_composed();
    top.cells.clear();
    top.glass = 0..0;
    top.switch = None;
    top.ghost = None;
    top.lockup = 0..0;
    top.icon = None;
    let TopKey {
        line,
        program,
        build,
        switch,
        palette,
        blank,
        mark_fg,
        face,
        ..
    } = *key;
    let (run, label) = laid_run(key);
    top.col = run.col;
    let rule_cell = run.rule;
    let tint = palette.filter(|_| line == TopLine::Rule);
    let ghost_at = ghost_at(run.len, face);
    let mark_at = face.mark.and_then(|_| match ghost_at {
        Some(at) => at.checked_sub(1),
        None => face
            .tag
            .and_then(|(start, _)| start.checked_sub(2)?.checked_sub(run.col))
            .filter(|&at| at < run.len),
    });
    let mark_cell = face.mark.map(|ch| RenderCell {
        ch,
        fg: mark_fg,
        bold: false,
        ..rule_cell
    });
    let ghost_inks = FactInks::of(rule_cell.fg, blank.fg, rule_cell.bg);
    let ghost_cells = |lit: bool| {
        let fg = if lit {
            ghost_inks.value
        } else {
            ghost_inks.stub
        };
        ghost_chars().map(move |ch| RenderCell {
            ch,
            fg,
            bold: false,
            script: aterm_core::terminal::Script::Normal,
            underline: if lit && ch != ' ' {
                aterm_core::terminal::UnderlineStyle::Single
            } else {
                aterm_core::terminal::UnderlineStyle::None
            },
            ..rule_cell
        })
    };
    if let Some(at) = ghost_at {
        top.ghost = Some((run.col + at + 1, run.col + at + GHOST_CELLS - 1));
    }
    let span_len = top_span(run.len, face);
    let mark = cli_mark(program);
    let Some((rung, sface)) = lay_top(span_len, switch, mark.map(|m| m.lockup_cells(build))) else {
        let first = mark_at.or(ghost_at);
        if let Some(first) = first {
            top.col = run.col + first;
            if let (Some(_), Some(mark)) = (mark_at, mark_cell) {
                top.cells.push(mark);
            }
            if let (Some(_), Some(lit)) = (ghost_at, face.ghost) {
                top.cells.extend(ghost_cells(lit));
            }
        }
        return;
    };
    let lockup_at = rung.lockup.zip(mark).map(|(lockup, mark)| {
        let cells = match lockup {
            Lockup::Full => mark.lockup_cells(build),
            Lockup::Icon => aterm_effects::cli_marks::ICON_RUN,
        };
        rung.lead..rung.lead + cells
    });
    let switch_at = sface.as_ref().map(|f| {
        let start = span_len - 2 - f.len;
        start..start + f.len
    });
    if let (Some(f), Some(at), Some(tier)) = (&sface, &switch_at, rung.tier) {
        let col_start = run.col + at.start;
        let laid = |r: &std::ops::Range<usize>| col_start + r.start..col_start + r.end;
        top.switch = Some(LaidSwitch {
            tier,
            cols: col_start..col_start + f.len,
            switch: laid(&f.switch),
            button: f.button.as_ref().map(laid),
        });
    }
    let (lo, hi) = if tint.is_some() {
        (0, span_len)
    } else {
        let firsts = [
            lockup_at.as_ref().map(|r| r.start),
            switch_at.as_ref().map(|r| r.start),
            mark_at,
        ];
        let lasts = [
            lockup_at.as_ref().map(|r| r.end),
            switch_at.as_ref().map(|r| r.end),
            mark_at.map(|m| m + 1),
        ];
        let lo = firsts.into_iter().flatten().min().unwrap_or(0);
        let hi = match ghost_at {
            Some(_) => span_len,
            None => lasts.into_iter().flatten().max().unwrap_or(lo),
        };
        (lo, hi.max(label))
    };
    top.col = run.col + lo;
    let cells = &mut top.cells;
    cells.resize(hi - lo, rule_cell);
    if let Some(palette) = tint {
        let dark = crate::claude_card::dark_ground(blank);
        let gap = switch_at
            .as_ref()
            .map(|s| lockup_at.as_ref().map_or(0, |l| l.end)..s.start);
        for i in 0..span_len {
            let mine =
                |r: &Option<std::ops::Range<usize>>| r.as_ref().is_some_and(|r| r.contains(&i));
            if mine(&lockup_at) || mine(&switch_at) {
                continue;
            }
            let t = match &gap {
                Some(gap) if gap.contains(&i) => {
                    let (j, g) = (i - gap.start, gap.len());
                    if g <= 1 {
                        0.0
                    } else {
                        1.0 - j as f32 / (g - 1) as f32
                    }
                }
                _ => 1.0,
            };
            let lifted = mix(rgb_of(palette.art(t, dark)), blank.fg, 0.25);
            let mut fg = mix(rule_cell.fg, lifted, 0.68);
            if fg == rule_cell.fg {
                fg[0] ^= 1;
            }
            cells[i - lo] = RenderCell {
                ch: '\u{2500}',
                fg,
                ..rule_cell
            };
        }
    }
    if let (Some(at), Some(mark)) = (&lockup_at, mark) {
        let pad = RenderCell { ch: ' ', ..blank };
        for c in at.clone() {
            cells[c - lo] = pad;
        }
        top.lockup = run.col + at.start..run.col + at.end;
        top.icon = Some(run.col + at.start + 1);
        if rung.lockup == Some(Lockup::Full) {
            let ground = crate::claude_card::pack(blank.bg);
            let name_at = at.start + aterm_effects::cli_marks::ICON_RUN;
            let word = mark.name(build);
            let word_len = word.chars().count();
            let limit = switch_at.as_ref().map_or(span_len, |s| s.start);
            let room = limit.saturating_sub(name_at + 1 + title_lead);
            let title = key
                .mark_hover
                .then(|| hover_title(program.product(), version, room))
                .flatten()
                .filter(|t| t.len >= word_len);
            match title {
                Some(t) => {
                    let end = name_at + t.len;
                    if cells.len() <= end - lo {
                        cells.resize(end + 1 - lo, rule_cell);
                    }
                    let letter = |k: usize| {
                        if t.len <= 1 {
                            0
                        } else {
                            (k * (word_len - 1) + (t.len - 1) / 2) / (t.len - 1)
                        }
                    };
                    for (k, ch) in t.chars().enumerate() {
                        cells[name_at + k - lo] = if ch == ' ' {
                            pad
                        } else {
                            RenderCell {
                                ch,
                                fg: rgb_of(aterm_effects::cli_marks::name_fg(
                                    mark,
                                    letter(k),
                                    ground,
                                )),
                                bold: true,
                                ..blank
                            }
                        };
                    }
                    cells[end - lo] = pad;
                    top.lockup.end = run.col + end + 1;
                }
                None => {
                    for (k, ch) in word.chars().enumerate() {
                        cells[name_at + k - lo] = RenderCell {
                            ch,
                            fg: rgb_of(aterm_effects::cli_marks::name_fg(mark, k, ground)),
                            bold: true,
                            ..blank
                        };
                    }
                }
            }
        }
    }
    if let (Some(mut f), Some(at), Some(paint)) = (sface, &switch_at, switch) {
        if palette.is_some() {
            f.on_arc();
        }
        for (k, &(ch, ink)) in f.cells().iter().enumerate() {
            cells[at.start + k - lo] = switch_cell(ch, ink, paint, palette, blank, mark_fg);
        }
    }
    if let (Some(at), Some(mark)) = (mark_at, mark_cell) {
        cells[at - lo] = mark;
    }
    if let (Some(_), Some(lit)) = (ghost_at, face.ghost) {
        cells.extend(ghost_cells(lit));
    }
    if tint.is_none() && !top.lockup.is_empty() {
        let from = (top.lockup.end - run.col).max(label);
        let next = [
            switch_at.as_ref().map(|r| r.start),
            mark_at,
            ghost_at.map(|_| span_len),
        ]
        .into_iter()
        .flatten()
        .min();
        if let Some(next) = next.filter(|&next| from < next && next <= hi) {
            top.glass = from - lo..next - lo;
        }
    }
}

/// The hover title's words ([`hover_title`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HoverTitle<'a> {
    product: &'a str,
    version: Option<&'a str>,
    len: usize,
}

impl HoverTitle<'_> {
    /// The title's glyphs.
    fn chars(&self) -> impl Iterator<Item = char> + '_ {
        self.product.chars().chain(
            self.version
                .into_iter()
                .flat_map(|v| std::iter::once(' ').chain(v.chars())),
        )
    }
}

/// The widest hover title whose words fit `room` cells.
fn hover_title<'a>(
    product: &'a str,
    version: Option<&'a str>,
    room: usize,
) -> Option<HoverTitle<'a>> {
    let cells = product.chars().count();
    version
        .map(|v| HoverTitle {
            product,
            version: Some(v),
            len: cells + 1 + v.chars().count(),
        })
        .filter(|t| t.len <= room)
        .or_else(|| {
            (cells <= room).then_some(HoverTitle {
                product,
                version: None,
                len: cells,
            })
        })
}

/// The contents of the marks' sprites ([`aterm_effects::cli_marks::content`]).
#[derive(Debug, Default)]
struct MarkContents(Vec<MarkContent>);

/// One kept sprite content.
type MarkContent = (
    CliMark,
    aterm_effects::cli_marks::Fit,
    u32,
    Arc<aterm_core::render::FxContent>,
);

impl MarkContents {
    /// How many contents are kept.
    const KEPT: usize = 2 * CliMark::ALL.len();

    /// `mark`'s content at `fit` on `ground` ([`Self::KEPT`]).
    fn content(
        &mut self,
        mark: CliMark,
        fit: aterm_effects::cli_marks::Fit,
        ground: u32,
    ) -> Arc<aterm_core::render::FxContent> {
        if let Some((.., kept)) = self
            .0
            .iter()
            .find(|(m, f, g, _)| (*m, *f, *g) == (mark, fit, ground))
        {
            return Arc::clone(kept);
        }
        let content = aterm_effects::cli_marks::content(mark, fit, ground);
        if self.0.len() >= Self::KEPT {
            self.0.remove(0);
        }
        self.0.push((mark, fit, ground, Arc::clone(&content)));
        content
    }
}

/// Pane `pane`'s box in grid pixels for a window cell of `cell` device pixels.
fn pane_grid_px(
    cell: (u16, u16),
    pane: crate::claude_card::PaneGeom,
) -> aterm_core::render::PxRect {
    let px = |cells: usize, side: u16| {
        i32::try_from(cells)
            .unwrap_or(i32::MAX)
            .saturating_mul(i32::from(side))
    };
    let top = pane.strip + pane.row_off;
    aterm_core::render::PxRect::new(
        px(pane.col_off, cell.0),
        px(top, cell.1),
        px(pane.col_off + pane.cols, cell.0),
        px(top + pane.rows, cell.1),
    )
}

/// Computes the stable fx layer ID for `session`'s top composer mark.
pub(crate) const fn top_mark_id(session: u64) -> u64 {
    let mut z = session ^ 0x746F_702D_6D61_726B;
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// The mark's sprite for a top rule composed from `key` into `top`.
fn mark_layer(
    key: &TopKey,
    top: &ComposedTop,
    session: u64,
    marks: &mut MarkContents,
) -> Option<aterm_core::render::FxLayer> {
    let col = top.icon?;
    let mark = cli_mark(key.program)?;
    let (cw, ch) = key.cell;
    let fit = aterm_effects::cli_marks::fit(mark, cw, ch)?;
    let content = marks.content(mark, fit, crate::claude_card::pack(key.blank.bg));
    let px = |cells: usize, side: u16| {
        i32::try_from(cells)
            .ok()
            .and_then(|c| c.checked_mul(i32::from(side)))
    };
    Some(aterm_effects::cli_marks::layer(
        top_mark_id(session),
        content,
        (px(col, cw)?, px(key.frame_row, ch)?),
        key.clip,
    ))
}

/// Returns the accessibility label strings for border switch `b` and its optional action button.
#[cfg(a11y_tree)]
pub(crate) fn switch_words(b: &crate::claude_lights::BorderSwitch) -> (String, Option<String>) {
    let paint = SwitchPaint {
        face: b.face,
        hover: false,
        version: (b.program == aterm_phase::Program::Agy)
            .then(|| b.program.harness_version())
            .flatten(),
    };
    let Some(face) = switch_face(paint, b.tier) else {
        return (String::new(), None);
    };
    let words = |range: &std::ops::Range<usize>| -> String {
        let text: String = face.cells[range.clone()]
            .iter()
            .map(|(ch, _)| *ch)
            .collect();
        text.split_whitespace()
            .filter(|w| w.chars().any(char::is_alphanumeric))
            .collect::<Vec<_>>()
            .join(" ")
    };
    (words(&face.switch), face.button.as_ref().map(words))
}

/// Whether `row` is an agy tool-call header.
fn is_agy_tool_call_row(row: &str) -> bool {
    let Some(rest) = row.strip_prefix("● ") else {
        return false;
    };
    if [
        "Ran ",
        "Explored ",
        "Edited ",
        "Created ",
        "Deleted ",
        "Searched ",
        "Viewed ",
        "Listed ",
    ]
    .iter()
    .any(|verb| rest.starts_with(verb))
    {
        return true;
    }
    let Some((name, _)) = rest.split_once('(') else {
        return false;
    };
    !name.is_empty()
        && name.chars().next().is_some_and(|c| c.is_ascii_uppercase())
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && rest.trim_end().ends_with(')')
}

/// Render inline markdown.
fn push_agy_inline_markdown(
    text: &str,
    base: RenderCell,
    code_fg: [u8; 3],
    code_bg: [u8; 3],
    link_fg: [u8; 3],
    max_cols: usize,
    out: &mut Vec<RenderCell>,
) -> bool {
    let mut changed = false;
    let mut rest = text;
    while !rest.is_empty() && out.len() < max_cols {
        if let Some(after_open) = rest.strip_prefix("***")
            && !after_open.starts_with(' ')
            && let Some((inner, after_close)) = after_open.split_once("***")
            && !inner.is_empty()
            && !inner.ends_with(' ')
        {
            changed = true;
            let bold_italic_cell = RenderCell {
                bold: true,
                italic: true,
                ..base
            };
            push_agy_inline_markdown(
                inner,
                bold_italic_cell,
                code_fg,
                code_bg,
                link_fg,
                max_cols,
                out,
            );
            rest = after_close;
            continue;
        }
        if let Some(after_open) = rest.strip_prefix("**")
            && !after_open.starts_with('*')
            && let Some((inner, after_close)) = after_open.split_once("**")
            && !inner.is_empty()
        {
            changed = true;
            let bold_cell = RenderCell { bold: true, ..base };
            push_agy_inline_markdown(inner, bold_cell, code_fg, code_bg, link_fg, max_cols, out);
            rest = after_close;
            continue;
        }
        if let Some(after_open) = rest.strip_prefix('`')
            && let Some((inner, after_close)) = after_open.split_once('`')
            && !inner.is_empty()
        {
            changed = true;
            let code_cell = RenderCell {
                fg: code_fg,
                bg: code_bg,
                underline: aterm_core::terminal::UnderlineStyle::None,
                ..base
            };
            for ch in inner.chars() {
                if out.len() >= max_cols {
                    break;
                }
                out.push(RenderCell { ch, ..code_cell });
            }
            rest = after_close;
            continue;
        }
        if let Some(after_open) = rest.strip_prefix('[')
            && let Some((label, after_label)) = after_open.split_once("](")
            && !label.is_empty()
            && !label.contains(['[', ']'])
            && let Some((url, after_url)) = after_label.split_once(')')
            && !url.is_empty()
            && !url.contains(' ')
        {
            changed = true;
            let link_cell = RenderCell {
                fg: link_fg,
                underline: aterm_core::terminal::UnderlineStyle::Single,
                ..base
            };
            push_agy_inline_markdown(label, link_cell, code_fg, code_bg, link_fg, max_cols, out);
            rest = after_url;
            continue;
        }
        if let Some(after_open) = rest.strip_prefix('*')
            && !after_open.starts_with(['*', ' '])
            && let Some((inner, after_close)) = after_open.split_once('*')
            && !inner.is_empty()
            && !inner.ends_with(' ')
        {
            changed = true;
            let italic_cell = RenderCell {
                italic: true,
                ..base
            };
            push_agy_inline_markdown(inner, italic_cell, code_fg, code_bg, link_fg, max_cols, out);
            rest = after_close;
            continue;
        }
        let mut chars = rest.chars();
        if let Some(ch) = chars.next() {
            out.push(RenderCell { ch, ..base });
            rest = chars.as_str();
        }
    }
    changed
}

/// The inks agy's and Muse Code's transcript is rewritten in ([`render_agy_markdown_row`],
/// [`collect_agy_transcript_edits`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct MarkdownInks {
    /// The accent as a graphic.
    mark: [u8; 3],
    /// The accent as letters.
    number: [u8; 3],
    /// An H1 heading: primary accent lifted toward the pane's ink.
    h1: [u8; 3],
    /// An H2 heading: crisp cyan-teal lifted toward the pane's ink.
    h2: [u8; 3],
    /// Inline code: warm amber-gold lifted toward the pane's ink, on [`Self::code_bg`].
    code: [u8; 3],
    /// Inline code's pill.
    code_bg: [u8; 3],
    /// A link: sky-blue lifted toward the pane's ink.
    link: [u8; 3],
    /// An H3 or H4 heading.
    h3: [u8; 3],
    /// A quiet glyph, an unchecked box's `☐` and a nested bullet's `◦` ([`VALUE_LIFT`]).
    quiet: [u8; 3],
    /// A quote's words.
    quote: [u8; 3],
    /// The rule's grey.
    rule: [u8; 3],
    /// A sent user message in `agy` (replacing raw VGA `#5c5cff`).
    user: [u8; 3],
}

impl MarkdownInks {
    /// The inks on `blank`'s pane, under a rule drawn in `rule` and the instance's `accent`.
    fn of(blank: RenderCell, rule: RenderCell, accent: [u8; 3]) -> Self {
        let ground = blank.bg;
        let code_bg = mix(ground, rule.fg, 0.16);
        let toward_ink = |t: f32| mix(accent, blank.fg, t);
        let h2_tint = mix([0x56, 0xb6, 0xc2], blank.fg, 0.35);
        let h3_tint = mix([0xb4, 0x8e, 0xad], blank.fg, 0.45);
        let code_tint = mix([0xe5, 0xc0, 0x7b], blank.fg, 0.32);
        let link_tint = mix([0x61, 0xaf, 0xef], blank.fg, 0.28);
        let user_tint = mix([0x7a, 0xa2, 0xf7], blank.fg, 0.35);
        Self {
            mark: visible_on(accent, ground),
            number: legible_on(accent, ground),
            h1: legible_on(toward_ink(0.22), ground),
            h2: legible_on(h2_tint, ground),
            code: legible_on(code_tint, code_bg),
            code_bg,
            link: legible_on(link_tint, ground),
            h3: legible_on(h3_tint, ground),
            quiet: visible_on(value_fg(rule.fg, blank.fg), ground),
            quote: legible_on(value_fg(rule.fg, blank.fg), ground),
            rule: rule.fg,
            user: legible_on(user_tint, ground),
        }
    }
}

/// Format one non-fenced row of an assistant reply.
fn render_agy_markdown_row(
    row: &str,
    max_cols: usize,
    orig_width: usize,
    blank: RenderCell,
    inks: &MarkdownInks,
) -> Option<Vec<RenderCell>> {
    let trimmed_end = row.trim_end_matches(' ');
    if trimmed_end.is_empty() {
        return None;
    }
    let MarkdownInks {
        mark: mark_fg,
        number: number_fg,
        h1: h1_fg,
        h2: h2_fg,
        code: code_fg,
        code_bg,
        link: link_fg,
        h3: h3_fg,
        quiet: quiet_fg,
        quote: quote_fg,
        rule: rule_fg,
        user: _,
    } = *inks;
    let (lead, body, lead_glyph) = if let Some(rest) = trimmed_end.strip_prefix("● ") {
        ("● ", rest, Some('●'))
    } else if let Some(rest) = trimmed_end.strip_prefix("\u{25c6} ") {
        ("\u{25c6} ", rest, Some('\u{25c6}'))
    } else {
        let spaces = trimmed_end.bytes().take_while(|&b| b == b' ').count();
        (&trimmed_end[..spaces], &trimmed_end[spaces..], None)
    };
    let mut out = Vec::with_capacity(orig_width);
    let mut changed = lead_glyph.is_some();
    if let Some(glyph) = lead_glyph {
        out.push(RenderCell {
            ch: glyph,
            fg: mark_fg,
            bold: true,
            ..blank
        });
        out.push(blank);
    } else {
        for _ in 0..lead.len().min(max_cols) {
            out.push(blank);
        }
    }
    let heading_level = body.bytes().take_while(|&b| b == b'#').count();
    if (1..=4).contains(&heading_level)
        && let Some(heading_text) = body[heading_level..].strip_prefix(' ')
    {
        changed = true;
        let heading_base = match heading_level {
            1 => RenderCell {
                fg: h1_fg,
                bold: true,
                underline: aterm_core::terminal::UnderlineStyle::Single,
                ..blank
            },
            2 => RenderCell {
                fg: h2_fg,
                bold: true,
                ..blank
            },
            _ => RenderCell {
                fg: h3_fg,
                bold: true,
                italic: true,
                ..blank
            },
        };
        push_agy_inline_markdown(
            heading_text,
            heading_base,
            code_fg,
            code_bg,
            link_fg,
            max_cols,
            &mut out,
        );
    } else if body.len() >= 3
        && (body.bytes().all(|b| b == b'-') || body.bytes().all(|b| b == b'*'))
    {
        changed = true;
        let dash_cell = RenderCell {
            ch: '\u{2500}',
            fg: rule_fg,
            ..blank
        };
        let target = orig_width.max(12).min(max_cols);
        while out.len() < target {
            out.push(dash_cell);
        }
    } else if let Some(list_item) = body.strip_prefix("- ").or_else(|| body.strip_prefix("* ")) {
        changed = true;
        let (marker, marker_fg, marker_bold, item_rest) =
            if let Some(unchecked) = list_item.strip_prefix("[ ] ") {
                ('\u{2610}', quiet_fg, false, unchecked)
            } else if let Some(checked) = list_item
                .strip_prefix("[x] ")
                .or_else(|| list_item.strip_prefix("[X] "))
            {
                ('\u{2713}', mark_fg, true, checked)
            } else if lead_glyph.is_none() && lead.len() >= 4 {
                ('\u{25e6}', quiet_fg, false, list_item)
            } else {
                ('\u{2022}', mark_fg, true, list_item)
            };
        if out.len() < max_cols {
            out.push(RenderCell {
                ch: marker,
                fg: marker_fg,
                bold: marker_bold,
                ..blank
            });
        }
        if out.len() < max_cols {
            out.push(blank);
        }
        push_agy_inline_markdown(
            item_rest, blank, code_fg, code_bg, link_fg, max_cols, &mut out,
        );
    } else if let digits = body.bytes().take_while(|b| b.is_ascii_digit()).count()
        && digits > 0
        && digits <= 3
        && let Some(item_rest) = body[digits..].strip_prefix(". ")
    {
        changed = true;
        for ch in body[..digits + 1].chars() {
            if out.len() < max_cols {
                out.push(RenderCell {
                    ch,
                    fg: number_fg,
                    bold: true,
                    ..blank
                });
            }
        }
        if out.len() < max_cols {
            out.push(blank);
        }
        push_agy_inline_markdown(
            item_rest, blank, code_fg, code_bg, link_fg, max_cols, &mut out,
        );
    } else if let Some(quote_item) = body.strip_prefix("> ") {
        changed = true;
        if out.len() < max_cols {
            out.push(RenderCell {
                ch: '▎',
                fg: rule_fg,
                ..blank
            });
        }
        if out.len() < max_cols {
            out.push(blank);
        }
        let quote_base = RenderCell {
            fg: quote_fg,
            italic: true,
            ..blank
        };
        push_agy_inline_markdown(
            quote_item, quote_base, code_fg, code_bg, link_fg, max_cols, &mut out,
        );
    } else {
        changed |=
            push_agy_inline_markdown(body, blank, code_fg, code_bg, link_fg, max_cols, &mut out);
    }
    if !changed {
        return None;
    }
    while out.len() < orig_width.min(max_cols) {
        out.push(blank);
    }
    Some(out)
}

/// Whether Claude's mode pill is relabelled on this pane's glass ([`compose_top`]).
fn relabels_pill(program: aterm_phase::Program, bordered: bool) -> bool {
    program == aterm_phase::Program::Claude && bordered
}

/// The repaint key's term for a pane's harness theme.
fn theme_term(theme: Option<aterm_effects::harness_card::CardPalette>) -> u64 {
    theme.map_or(0, |p| 1 + p as u64)
}

/// One relabelled pill ([`collect_pill_relabel`]).
struct RelabelWrite {
    frame_row: usize,
    col: usize,
    cells: std::ops::Range<usize>,
    ground: RenderCell,
    shift: GlassShift,
}

/// Column-shift mapping for a single window row whose mode pill was relabelled (`YOLO on`/`off`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct GlassShift {
    term_row: usize,
    /// The pane's column 0.
    col_off: usize,
    relabel: Option<footer::Relabel>,
}

impl GlassShift {
    fn grid_col(&self, row: usize, col: usize) -> Option<usize> {
        let relabel = self.relabel?;
        if row != self.term_row || col < self.col_off {
            return None;
        }
        Some(self.col_off + relabel.grid_col(col - self.col_off))
    }
}

/// The most rows of shifted columns a window keeps.
const GLASS_SHIFTS: usize = 8;

/// Per-window collection of [`GlassShift`] mappings from the last footer splice.
#[derive(Debug, Default)]
pub(crate) struct GlassColumns {
    len: usize,
    rows: [GlassShift; GLASS_SHIFTS],
}

impl GlassColumns {
    const fn clear(&mut self) {
        self.len = 0;
    }

    /// Keep `shift`; a window full of them keeps the first [`GLASS_SHIFTS`].
    fn push(&mut self, shift: GlassShift) {
        if let Some(slot) = self.rows.get_mut(self.len) {
            *slot = shift;
            self.len += 1;
        }
    }

    /// Maps glass column `col` on `row` back to its terminal grid column across relabelled runs.
    pub(crate) fn grid_col(&self, row: u16, col: u16) -> u16 {
        self.rows[..self.len]
            .iter()
            .find_map(|s| s.grid_col(usize::from(row), usize::from(col)))
            .and_then(|c| u16::try_from(c).ok())
            .unwrap_or(col)
    }
}

/// Collect the relabel of Claude's mode pill `relabel` under pane rows `rows`' bottom rule
/// ([`GLASS_SHIFTS`]).
#[allow(clippy::too_many_arguments)]
fn collect_pill_relabel(
    scratch: &RenderInput,
    rows: &[String],
    region: PaneTextRegion,
    relabel: footer::Relabel,
    chrome_owned: impl Fn(usize) -> bool,
    blank: RenderCell,
    out: &mut Vec<RelabelWrite>,
    cells: &mut Vec<RenderCell>,
) -> bool {
    let term_row = region.row_off + relabel.row;
    let frame_row = region.strip + term_row;
    if out.len() >= GLASS_SHIFTS
        || relabel.run_end > region.cols
        || chrome_owned(term_row)
        || scratch.any_selection()
        || (scratch.cursor_visible && scratch.cursor_row == frame_row)
        || !scratch.visual_to_logical_row(frame_row).is_empty()
    {
        return false;
    }
    let from = region.col_off + relabel.start;
    let written = from..region.col_off + relabel.run_end;
    if touches(&scratch.clusters, frame_row, &written)
        || touches(&scratch.combining, frame_row, &written)
        || touches(&scratch.images, frame_row, &written)
    {
        return false;
    }
    let Some(span) = scratch
        .cells
        .get(frame_row)
        .and_then(|row| row.get(written))
    else {
        return false;
    };
    let text = rows[relabel.row].chars().skip(relabel.start);
    let pill = relabel.end - relabel.start;
    let plain = span.iter().zip(text).enumerate().all(|(k, (c, ch))| {
        c.ch == ch
            && !c.wide
            && !c.emoji_presentation
            && !c.text_presentation
            && (k >= pill || c.bg == blank.bg)
    });
    if !plain || span.len() != relabel.run_end - relabel.start {
        return false;
    }
    let style = span[relabel.words - relabel.start];
    let at = cells.len();
    cells.extend(relabel.label.chars().map(|ch| RenderCell { ch, ..style }));
    cells.extend_from_slice(&span[relabel.end - relabel.start..]);
    cells.extend(std::iter::repeat_n(blank, relabel.shift()));
    out.push(RelabelWrite {
        frame_row,
        col: from,
        cells: at..cells.len(),
        ground: blank,
        shift: GlassShift {
            term_row,
            col_off: region.col_off,
            relabel: Some(relabel),
        },
    });
    true
}

/// Whether row 0 of `rows[..limit]` begins inside a fenced code block whose opening fence scrolled
/// off above the top of the viewport.
fn starts_in_code_block(
    rows: &[String],
    limit: usize,
    is_muse: bool,
    card_range: &Option<std::ops::Range<usize>>,
) -> bool {
    let mut fences: Vec<(usize, bool)> = Vec::new();
    let mut seg_end = limit;
    for (r, row_str) in rows.iter().enumerate().take(limit) {
        if card_range.as_ref().is_some_and(|rng| rng.contains(&r)) {
            seg_end = r;
            break;
        }
        if is_muse {
            if row_str == "\u{276f}"
                || row_str.starts_with("\u{276f} ")
                || row_str == "  \u{276f}"
                || row_str.starts_with("  \u{276f} ")
                || row_str.starts_with("\u{25c6} ")
            {
                seg_end = r;
                break;
            }
        } else {
            if r > 0 && (row_str == ">" || row_str.starts_with("> ")) {
                let prev = rows[r - 1].trim_end_matches(' ');
                if prev.chars().count() >= 10 && prev.chars().all(|c| c == '\u{2500}') {
                    seg_end = r;
                    break;
                }
            }
            if row_str.starts_with("● ") {
                seg_end = r;
                break;
            }
        }
        let body = row_str
            .strip_prefix("● ")
            .or_else(|| row_str.strip_prefix("\u{25c6} "))
            .unwrap_or(row_str)
            .trim_start();
        if let Some(after) = body.strip_prefix("```") {
            fences.push((r, !after.trim().is_empty()));
        }
    }
    if fences.is_empty() {
        return false;
    }
    if let Some(first_tagged) = fences.iter().position(|&(_, has_tag)| has_tag) {
        return !first_tagged.is_multiple_of(2);
    }
    if fences.len().is_multiple_of(2) {
        return false;
    }
    if seg_end < limit {
        return true;
    }
    let f0 = fences[0].0;
    let f1 = fences.get(1).map_or(limit, |&(r, _)| r);
    let is_md_block = |row: &str| {
        let trimmed = row.trim_start();
        trimmed.starts_with("## ")
            || trimmed.starts_with("### ")
            || trimmed.starts_with("#### ")
            || trimmed.starts_with("- ")
            || trimmed.starts_with("* ")
            || trimmed.starts_with("+ ")
            || trimmed.starts_with("> ")
            || trimmed.find(". ").is_some_and(|d| {
                d > 0 && d <= 3 && trimmed[..d].bytes().all(|b| b.is_ascii_digit())
            })
    };
    let before_f0 = rows[..f0].iter().filter(|r| is_md_block(r)).count();
    let after_f0 = rows[f0 + 1..f1].iter().filter(|r| is_md_block(r)).count();
    before_f0 <= after_f0
}

/// Find the top row of a bottom `agy` approval or question dialog when the composer rules are
/// absent.
fn agy_bottom_dialog_top(rows: &[String], max_rows: usize, cols: usize) -> Option<usize> {
    let n = rows.len().min(max_rows);
    if let Some(prompt) = aterm_phase::agy::prompt(&rows[..n]) {
        return Some(prompt.span.0);
    }
    if cols < 40 {
        return None;
    }
    for r in (1..n).rev() {
        let trimmed = rows[r].trim_end_matches(' ');
        let width = trimmed.chars().count();
        if width == cols && trimmed.chars().all(|c| c == '\u{2500}') {
            let prev = rows[r - 1].trim_end_matches(' ');
            let is_box_title = matches!(
                prev,
                "Command" | "Create file" | "Pending edit" | "Question"
            );
            let has_later_turn = (r + 1..n).any(|i| {
                rows[i].starts_with("● ")
                    || ((rows[i] == ">" || rows[i].starts_with("> ")) && {
                        let rule = rows[i - 1].trim_end_matches(' ');
                        rule.chars().count() >= 10 && rule.chars().all(|c| c == '\u{2500}')
                    })
            });
            if is_box_title && !has_later_turn {
                return Some(r - 1);
            }
        }
    }
    None
}

/// Append `text` in bold rainbow spectrum ink (`CardPalette::Cruise`) floored to >= 4.5:1 contrast.
fn push_rainbow_text(
    text: &str,
    phase_offset: f32,
    max_cols: usize,
    blank: RenderCell,
    out: &mut Vec<RenderCell>,
) {
    let total = text.chars().count().max(1);
    for (idx, ch) in text.chars().enumerate() {
        if out.len() >= max_cols {
            break;
        }
        if ch == ' ' {
            out.push(blank);
            continue;
        }
        let t = if total <= 1 {
            phase_offset.fract()
        } else {
            (phase_offset + idx as f32 / (total - 1) as f32).fract()
        };
        let fg = brand_letter_fg(aterm_effects::harness_card::CardPalette::Cruise, t, blank);
        out.push(RenderCell {
            ch,
            fg,
            bold: true,
            ..blank
        });
    }
}

/// Whether `s` is a compact elapsed duration token (`4m36s`, `8s`, `1h2m`).
fn is_compact_agy_elapsed(s: &str) -> bool {
    let t = s.trim();
    !t.is_empty()
        && t.len() <= 12
        && t.bytes().next().is_some_and(|b| b.is_ascii_digit())
        && t.bytes()
            .last()
            .is_some_and(|b| matches!(b, b's' | b'm' | b'h'))
        && t.bytes()
            .all(|b| b.is_ascii_digit() || matches!(b, b's' | b'm' | b'h' | b'.'))
}

/// Unwrap `Agent(self: Role)` -> `Role` and humanize `subagent(s)` / `task(s)` in `s`.
fn humanize_agy_subagent_phrase(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(at) = rest.find("Agent(") {
        out.push_str(&rest[..at]);
        let from_agent = &rest[at..];
        if let Some((unwrapped, after)) = aterm_phase::agy::split_agent_token(from_agent) {
            out.push_str(unwrapped);
            rest = after;
        } else {
            out.push_str("Agent(");
            rest = &from_agent["Agent(".len()..];
        }
    }
    out.push_str(rest);
    let mut cleaned = String::with_capacity(out.len());
    let mut tail = out.as_str();
    while let Some(pos) = tail.find("(s)") {
        let prefix = &tail[..pos];
        cleaned.push_str(prefix);
        let singular = prefix.rsplit_once(' ').is_some_and(|(before_word, _)| {
            before_word.ends_with("1") && !before_word.ends_with("11")
        });
        if !singular {
            cleaned.push('s');
        }
        tail = &tail[pos + 3..];
    }
    cleaned.push_str(tail);
    cleaned
}

/// Split optional ` · <elapsed>` and ` (ctrl+o to expand)` suffixes from the end of `s`.
fn split_agy_subagent_suffixes(s: &str) -> (&str, Option<&str>, Option<&str>) {
    let mut body = s.trim_end();
    let mut elapsed = None;
    if let Some((before, cand)) = body.rsplit_once(" \u{00B7} ")
        && is_compact_agy_elapsed(cand)
    {
        elapsed = Some(cand.trim());
        body = before.trim_end();
    }
    let mut expand = None;
    for hint in [" (ctrl+o to expand)", " (ctrl+b to expand)"] {
        if let Some(before) = body.strip_suffix(hint) {
            expand = Some(hint);
            body = before.trim_end();
            break;
        }
    }
    (body, expand, elapsed)
}

/// Render an `agy` subagent status or tree row in the transcript, its name in rainbow ink.
fn render_agy_subagent_transcript_row(
    row: &str,
    row_idx: usize,
    max_cols: usize,
    orig_width: usize,
    blank: RenderCell,
    inks: &MarkdownInks,
) -> Option<Vec<RenderCell>> {
    let trimmed_end = row.trim_end_matches(' ');
    if trimmed_end.is_empty() {
        return None;
    }
    let phase = (row_idx as f32) * 0.19;
    let quiet_cell = |ch| RenderCell {
        ch,
        fg: inks.quiet,
        ..blank
    };
    let quote_cell = |ch| RenderCell {
        ch,
        fg: inks.quote,
        ..blank
    };
    if let Some(rest) = trimmed_end.strip_prefix("● ") {
        let (core, expand, elapsed) = split_agy_subagent_suffixes(rest);
        let is_action = [
            "Spawned ",
            "Spawning ",
            "Messaged ",
            "Messaging ",
            "Killed ",
            "Killing ",
            "Defined ",
            "Defining ",
            "Listed ",
            "Listing ",
            "Invoked ",
            "Invoking ",
            "Sent message to ",
            "Sending message to ",
        ]
        .iter()
        .any(|v| core.starts_with(v))
            && (core.contains("subagent") || core.contains("Agent("));
        let is_agent_head = aterm_phase::agy::split_agent_token(core).is_some();
        let is_alert = (core.contains(" needs approval for ") || core.ends_with(" has a question"))
            && (core.starts_with("Agent(") || core.contains("subagent"));
        if !is_action && !is_agent_head && !is_alert {
            return None;
        }
        let mut out = Vec::with_capacity(orig_width.min(max_cols));
        out.push(RenderCell {
            ch: '●',
            fg: inks.mark,
            bold: true,
            ..blank
        });
        if out.len() < max_cols {
            out.push(blank);
        }
        let clean_core = core.strip_suffix("...").unwrap_or(core);
        if is_alert
            && let Some((subj, alert_tail)) = clean_core
                .split_once(" needs approval for ")
                .map(|(a, b)| (a, format!(" needs approval for {b}")))
                .or_else(|| {
                    clean_core
                        .strip_suffix(" has a question")
                        .map(|a| (a, " has a question".to_owned()))
                })
        {
            let human_subj = humanize_agy_subagent_phrase(subj);
            push_rainbow_text(&human_subj, phase, max_cols, blank, &mut out);
            for ch in alert_tail.chars() {
                if out.len() >= max_cols {
                    break;
                }
                out.push(RenderCell {
                    ch,
                    fg: inks.h1,
                    bold: true,
                    ..blank
                });
            }
        } else if let Some((unwrapped, after_tok_raw)) =
            aterm_phase::agy::split_agent_token(clean_core)
        {
            push_rainbow_text(unwrapped, phase, max_cols, blank, &mut out);
            let after_tok = after_tok_raw.trim_start();
            if !after_tok.is_empty() {
                let detail = after_tok
                    .strip_prefix(": ")
                    .or_else(|| after_tok.strip_prefix("\u{00B7} "))
                    .unwrap_or(after_tok);
                let detail = detail.strip_suffix("...").unwrap_or(detail);
                let human_detail = humanize_agy_subagent_phrase(detail);
                for ch in " \u{00B7} ".chars() {
                    if out.len() < max_cols {
                        out.push(quiet_cell(ch));
                    }
                }
                for ch in human_detail.chars() {
                    if out.len() < max_cols {
                        out.push(quote_cell(ch));
                    }
                }
            }
        } else {
            let human = humanize_agy_subagent_phrase(clean_core);
            push_rainbow_text(&human, phase, max_cols, blank, &mut out);
        }
        if let Some(exp) = expand {
            for ch in exp.chars() {
                if out.len() < max_cols {
                    out.push(quiet_cell(ch));
                }
            }
        }
        if let Some(el) = elapsed {
            for ch in " \u{00B7} ".chars().chain(el.chars()) {
                if out.len() < max_cols {
                    out.push(quiet_cell(ch));
                }
            }
        }
        while out.len() < orig_width.min(max_cols) {
            out.push(blank);
        }
        return Some(out);
    }

    let lead_spaces = trimmed_end.bytes().take_while(|&b| b == b' ').count();
    if lead_spaces >= 2 {
        let after_lead = &trimmed_end[lead_spaces..];
        let branch = ["└ ", "├ ", "⎿  ", "⎿ "]
            .into_iter()
            .find_map(|p| after_lead.strip_prefix(p).map(|r| (p, r)));
        if let Some((conn, branch_rest)) = branch
            && (branch_rest.contains("Agent(") || branch_rest.contains("subagent"))
        {
            let (core, expand, elapsed) = split_agy_subagent_suffixes(branch_rest);
            let mut out = Vec::with_capacity(orig_width.min(max_cols));
            for _ in 0..lead_spaces.min(max_cols) {
                out.push(blank);
            }
            for ch in conn.chars() {
                if out.len() < max_cols {
                    out.push(if ch == ' ' { blank } else { quiet_cell(ch) });
                }
            }
            if let Some(at) = core.find("Agent(")
                && let Some((unwrapped, after_tok_raw)) =
                    aterm_phase::agy::split_agent_token(&core[at..])
            {
                let before_tok = &core[..at];
                let head_phrase = format!("{before_tok}{unwrapped}");
                push_rainbow_text(&head_phrase, phase, max_cols, blank, &mut out);
                let tail = after_tok_raw.trim_start();
                if !tail.is_empty() {
                    let detail = tail
                        .strip_prefix(": ")
                        .or_else(|| tail.strip_prefix("\u{00B7} "))
                        .unwrap_or(tail);
                    let detail = detail.strip_suffix("...").unwrap_or(detail);
                    let human_detail = humanize_agy_subagent_phrase(detail);
                    for _ in 0..2 {
                        if out.len() < max_cols {
                            out.push(blank);
                        }
                    }
                    for ch in human_detail.chars() {
                        if out.len() < max_cols {
                            out.push(quote_cell(ch));
                        }
                    }
                }
            } else {
                let clean_core = core.strip_suffix("...").unwrap_or(core);
                let human = humanize_agy_subagent_phrase(clean_core);
                push_rainbow_text(&human, phase, max_cols, blank, &mut out);
            }
            if let Some(exp) = expand {
                for ch in exp.chars() {
                    if out.len() < max_cols {
                        out.push(quiet_cell(ch));
                    }
                }
            }
            if let Some(el) = elapsed {
                for ch in " \u{00B7} ".chars().chain(el.chars()) {
                    if out.len() < max_cols {
                        out.push(quiet_cell(ch));
                    }
                }
            }
            while out.len() < orig_width.min(max_cols) {
                out.push(blank);
            }
            return Some(out);
        }
    }
    None
}

/// Render an open `agy` background panel item row (`  ● Agent(...)` or `  ● [hh:mm:ss] ...`).
fn render_agy_panel_item_row(
    row: &str,
    panel_idx: usize,
    max_cols: usize,
    orig_width: usize,
    blank: RenderCell,
    inks: &MarkdownInks,
) -> Option<Vec<RenderCell>> {
    let trimmed = row.trim_end_matches(' ');
    let rest = trimmed.strip_prefix("  ● ")?;
    let mut out = Vec::with_capacity(orig_width.min(max_cols));
    for _ in 0..2usize.min(max_cols) {
        out.push(blank);
    }
    if out.len() < max_cols {
        out.push(RenderCell {
            ch: '●',
            fg: inks.mark,
            bold: true,
            ..blank
        });
    }
    if out.len() < max_cols {
        out.push(blank);
    }
    if let Some((unwrapped, after_agent)) = aterm_phase::agy::split_agent_panel_item(rest) {
        // Columns 4..4+unwrapped.chars().count() are `agents_panel::agy_agent_row`'s `name`
        // span, whose `agy_agent` cells `claude_panel::make_look` leaves alone, so this rainbow
        // stays while `claude_panel` animates the status dot at column 2.
        push_rainbow_text(
            unwrapped,
            (panel_idx as f32) * 0.23,
            max_cols,
            blank,
            &mut out,
        );
        if !after_agent.is_empty() {
            let (raw_act, _, elapsed) = split_agy_subagent_suffixes(after_agent);
            let clean_act = raw_act.strip_suffix("...").unwrap_or(raw_act).trim();
            if !clean_act.is_empty() {
                for _ in 0..2 {
                    if out.len() < max_cols {
                        out.push(blank);
                    }
                }
                let is_passive = aterm_phase::agy::is_subagent_idle_state(clean_act)
                    || clean_act == "Processing";
                let is_approval = aterm_phase::agy::is_subagent_approval_state(clean_act);
                let act_fg = if is_approval {
                    inks.h1
                } else if is_passive {
                    inks.quiet
                } else {
                    inks.quote
                };
                let human_act = humanize_agy_subagent_phrase(clean_act);
                for ch in human_act.chars() {
                    if out.len() < max_cols {
                        out.push(RenderCell {
                            ch,
                            fg: act_fg,
                            bold: is_approval,
                            ..blank
                        });
                    }
                }
            }
            if let Some(el) = elapsed {
                for ch in " \u{00B7} ".chars().chain(el.chars()) {
                    if out.len() < max_cols {
                        out.push(RenderCell {
                            ch,
                            fg: inks.quiet,
                            ..blank
                        });
                    }
                }
            }
        }
        while out.len() < orig_width.min(max_cols) {
            out.push(blank);
        }
        return Some(out);
    }
    if rest.starts_with('[')
        && let Some(close_bracket) = rest.find(']')
    {
        let stamp = &rest[..=close_bracket];
        for ch in stamp.chars() {
            if out.len() < max_cols {
                out.push(RenderCell {
                    ch,
                    fg: inks.quiet,
                    ..blank
                });
            }
        }
        let after_stamp = rest[close_bracket + 1..].trim_start();
        if !after_stamp.is_empty() {
            if out.len() < max_cols {
                out.push(blank);
            }
            let (cmd_part, status_part) = after_stamp
                .rsplit_once("  ")
                .map(|(a, b)| (a.trim_end(), Some(b.trim())))
                .unwrap_or((after_stamp, None));
            for ch in cmd_part.chars() {
                if out.len() < max_cols {
                    out.push(RenderCell {
                        ch,
                        fg: inks.code,
                        ..blank
                    });
                }
            }
            if let Some(status) = status_part.filter(|s| !s.is_empty()) {
                for _ in 0..2 {
                    if out.len() < max_cols {
                        out.push(blank);
                    }
                }
                for ch in status.chars() {
                    if out.len() < max_cols {
                        out.push(RenderCell {
                            ch,
                            fg: inks.quote,
                            ..blank
                        });
                    }
                }
            }
        }
        while out.len() < orig_width.min(max_cols) {
            out.push(blank);
        }
        return Some(out);
    }
    None
}

/// Collect glass-only undoable markdown edits for an `agy` or `muse` pane.
#[allow(clippy::too_many_arguments)]
fn collect_agy_transcript_edits(
    scratch: &RenderInput,
    program: aterm_phase::Program,
    rows: &[String],
    region: PaneTextRegion,
    top_rule: Option<usize>,
    blank: RenderCell,
    rule: RenderCell,
    mark_fg: [u8; 3],
    out: &mut Vec<AgyRowEdit>,
) {
    if scratch.any_selection() {
        return;
    }
    let is_muse = program == aterm_phase::Program::Muse;
    let max_rows = rows.len().min(region.rows);
    let limit = top_rule
        .or_else(|| {
            if is_muse {
                aterm_phase::muse::composer_rules(rows).map(|(t, _)| t)
            } else {
                aterm_phase::agy::composer_rules(rows)
                    .map(|(t, _)| t)
                    .or_else(|| agy_bottom_dialog_top(rows, max_rows, region.cols))
            }
        })
        .unwrap_or(max_rows)
        .min(max_rows);
    let card_range = if is_muse {
        let (_, muse_place) = aterm_phase::muse::launch_card_scan(&rows[..limit]);
        muse_place.map(|p| p.top..p.top + 2)
    } else {
        let (_, agy_place) = aterm_phase::agy::launch_card_scan(&rows[..limit]);
        agy_place.map(|p| p.top..p.top + 5)
    };
    let cursor_in_pane = scratch.cursor_visible
        && (region.col_off..region.col_off + region.cols).contains(&scratch.cursor_col);
    let mut in_user_msg = false;
    let mut in_muse_reply = false;
    let mut in_code_block = starts_in_code_block(rows, limit, is_muse, &card_range);
    let inks = MarkdownInks::of(blank, rule, mark_fg);

    for (r, row_str) in rows.iter().enumerate().take(limit) {
        let body = row_str
            .strip_prefix("● ")
            .or_else(|| row_str.strip_prefix("\u{25c6} "))
            .unwrap_or(row_str)
            .trim_start();
        if let Some(after_fence) = body.strip_prefix("```") {
            let has_tag = !after_fence.trim().is_empty();
            in_code_block = if has_tag { true } else { !in_code_block };
            continue;
        }
        if in_code_block {
            continue;
        }
        if card_range.as_ref().is_some_and(|rng| rng.contains(&r)) {
            in_user_msg = false;
            in_muse_reply = false;
            in_code_block = false;
            continue;
        }
        let orig_width = row_str
            .trim_end_matches(' ')
            .chars()
            .count()
            .min(region.cols);
        if orig_width == 0 {
            in_user_msg = false;
            continue;
        }
        if is_muse {
            if row_str == "\u{276f}"
                || row_str.starts_with("\u{276f} ")
                || row_str == "  \u{276f}"
                || row_str.starts_with("  \u{276f} ")
            {
                in_user_msg = true;
                in_muse_reply = false;
                in_code_block = false;
                continue;
            }
            if row_str.starts_with("\u{25c6} ") {
                in_user_msg = false;
                in_muse_reply = true;
            } else if in_muse_reply {
                let trimmed_lead = row_str.trim_start();
                if !row_str.starts_with("  ")
                    || trimmed_lead.starts_with([
                        '\u{256d}', '\u{2502}', '\u{2570}', '\u{25c7}', '\u{25c8}', '└', '⎿', '↑',
                        '\u{276f}', '/',
                    ])
                    || (row_str.starts_with("  ")
                        && !row_str.starts_with("   ")
                        && row_str.matches(" \u{00B7} ").count() >= 2)
                {
                    in_muse_reply = false;
                }
            }
        } else if r > 0 && (row_str == ">" || row_str.starts_with("> ")) {
            let prev = rows[r - 1].trim_end_matches(' ');
            if prev.chars().count() >= 10 && prev.chars().all(|c| c == '\u{2500}') {
                in_user_msg = true;
            }
        }
        let frame_row = region.strip + region.row_off + r;
        if cursor_in_pane && scratch.cursor_row == frame_row {
            continue;
        }
        let Some(frame_cells) = scratch.cells.get(frame_row) else {
            continue;
        };
        let pane_slice = frame_cells
            .get(region.col_off..frame_cells.len().min(region.col_off + region.cols))
            .unwrap_or(&[]);
        if !scratch.visual_to_logical_row(frame_row).is_empty()
            || pane_slice
                .iter()
                .any(|c| c.bg != blank.bg || c.wide || c.emoji_presentation || c.text_presentation)
        {
            continue;
        }
        let is_agy_raw_user_cell = !is_muse
            && pane_slice
                .get(..orig_width.min(pane_slice.len()))
                .is_some_and(|s| {
                    s.iter()
                        .any(|c| c.ch != ' ' && c.bold && c.fg == [0x5c, 0x5c, 0xff])
                });
        if is_agy_raw_user_cell {
            in_user_msg = true;
            in_code_block = false;
            let mut cells: Vec<RenderCell> = pane_slice[..orig_width.min(pane_slice.len())]
                .iter()
                .map(|c| {
                    if c.fg == [0x5c, 0x5c, 0xff] {
                        RenderCell {
                            fg: if c.ch == '>' { inks.mark } else { inks.user },
                            ..*c
                        }
                    } else {
                        *c
                    }
                })
                .collect();
            while cells.len() < orig_width.min(region.cols) {
                cells.push(blank);
            }
            out.push(AgyRowEdit {
                frame_row,
                col: region.col_off,
                cells,
                ground: blank,
            });
            continue;
        }
        if in_user_msg {
            if row_str.starts_with("  ") || row_str == ">" || row_str.starts_with("> ") {
                continue;
            }
            in_user_msg = false;
        }
        if !is_muse
            && let Some(cells) = render_agy_subagent_transcript_row(
                row_str,
                r,
                region.cols,
                orig_width,
                blank,
                &inks,
            )
        {
            out.push(AgyRowEdit {
                frame_row,
                col: region.col_off,
                cells,
                ground: blank,
            });
            continue;
        }
        let is_reply_row = if is_muse {
            in_muse_reply
        } else {
            (row_str.starts_with("  ")
                && !row_str
                    .trim_start()
                    .starts_with(['└', '⎿', '↑', '◆', '\u{256d}', '\u{2502}', '\u{2570}']))
                || (row_str.starts_with("● ") && !is_agy_tool_call_row(row_str))
        };
        if !is_reply_row {
            in_code_block = false;
            continue;
        }
        if let Some(cells) = render_agy_markdown_row(row_str, region.cols, orig_width, blank, &inks)
        {
            out.push(AgyRowEdit {
                frame_row,
                col: region.col_off,
                cells,
                ground: blank,
            });
        } else if !is_muse
            && blank.fg != [0xd0, 0xd0, 0xd0]
            && !row_str.contains("^[")
            && pane_slice
                .get(..orig_width.min(pane_slice.len()))
                .is_some_and(|s| s.iter().any(|c| c.ch != ' ' && c.fg == [0xd0, 0xd0, 0xd0]))
        {
            let cells: Vec<RenderCell> = row_str
                .trim_end_matches(' ')
                .chars()
                .take(region.cols)
                .map(|ch| RenderCell { ch, ..blank })
                .collect();
            out.push(AgyRowEdit {
                frame_row,
                col: region.col_off,
                cells,
                ground: blank,
            });
        }
    }
    if !is_muse
        && top_rule.is_some()
        && scratch.display_offset == 0
        && let Some((_, bottom)) = aterm_phase::agy::composer_rules(rows)
    {
        let panel_bounds = aterm_phase::agy::panel_bounds_under(rows, bottom);
        if let Some((panel_first, panel_close)) = panel_bounds {
            for r in panel_first..panel_close.min(region.rows) {
                let Some(row_str) = rows.get(r) else {
                    continue;
                };
                let frame_row = region.strip + region.row_off + r;
                let orig_width = row_str
                    .trim_end_matches(' ')
                    .chars()
                    .count()
                    .min(region.cols);
                if orig_width == 0
                    || (cursor_in_pane && scratch.cursor_row == frame_row)
                    || !scratch.visual_to_logical_row(frame_row).is_empty()
                {
                    continue;
                }
                let Some(frame_cells) = scratch.cells.get(frame_row) else {
                    continue;
                };
                let pane_slice = frame_cells
                    .get(region.col_off..frame_cells.len().min(region.col_off + region.cols))
                    .unwrap_or(&[]);
                if !pane_slice.iter().all(|c| {
                    c.bg == blank.bg && !c.wide && !c.emoji_presentation && !c.text_presentation
                }) {
                    continue;
                }
                if let Some(cells) = render_agy_panel_item_row(
                    row_str,
                    r - panel_first,
                    region.cols,
                    orig_width,
                    blank,
                    &inks,
                ) {
                    out.push(AgyRowEdit {
                        frame_row,
                        col: region.col_off,
                        cells,
                        ground: blank,
                    });
                }
            }
        }
        let footer_r = panel_bounds.map_or(bottom + 1, |(_, close)| close + 1);
        if let Some(row_str) = rows.get(footer_r)
            && footer_r < region.rows
        {
            let frame_row = region.strip + region.row_off + footer_r;
            let orig_width = row_str
                .trim_end_matches(' ')
                .chars()
                .count()
                .min(region.cols);
            if orig_width > 0
                && !(cursor_in_pane && scratch.cursor_row == frame_row)
                && scratch.visual_to_logical_row(frame_row).is_empty()
                && let Some(frame_cells) = scratch.cells.get(frame_row)
            {
                let pane_slice = frame_cells
                    .get(region.col_off..frame_cells.len().min(region.col_off + region.cols))
                    .unwrap_or(&[]);
                if pane_slice.iter().all(|c| {
                    c.bg == blank.bg && !c.wide && !c.emoji_presentation && !c.text_presentation
                }) {
                    let trimmed = row_str.trim_end_matches(' ');
                    let lead_spaces = trimmed.bytes().take_while(|&b| b == b' ').count();
                    let after_lead = &trimmed[lead_spaces..];
                    let has_shortcuts = after_lead.starts_with("? for shortcuts");
                    let mut subagents_badge: Option<String> = None;
                    let mut tasks_badge: Option<String> = None;
                    let right_col = trimmed.rfind("  ").and_then(|sep| {
                        let right = trimmed[sep + 2..].trim_start();
                        let start_byte = trimmed.len() - right.len();
                        let parts: Vec<&str> = right.split(" \u{00B7} ").map(str::trim).collect();
                        let mut recognized = false;
                        for part in &parts {
                            if part.ends_with(" subagent(s)")
                                || part.ends_with(" subagents")
                                || part.ends_with(" subagent")
                            {
                                subagents_badge = Some(humanize_agy_subagent_phrase(part));
                                recognized = true;
                            } else if part.ends_with(" task(s)")
                                || part.ends_with(" tasks")
                                || part.ends_with(" task")
                            {
                                tasks_badge = Some(humanize_agy_subagent_phrase(part));
                                recognized = true;
                            } else if matches!(
                                part.to_ascii_lowercase().as_str(),
                                "low" | "medium" | "high" | "xhigh" | "max"
                            ) {
                                recognized = true;
                            }
                        }
                        let first = parts.first().copied().unwrap_or("");
                        if !first.is_empty()
                            && first != "(initializing...)"
                            && !first.starts_with('?')
                            && recognized
                        {
                            Some(trimmed[..start_byte].chars().count())
                        } else {
                            None
                        }
                    });
                    if has_shortcuts
                        || right_col.is_some()
                        || subagents_badge.is_some()
                        || tasks_badge.is_some()
                    {
                        let style = pane_slice
                            .iter()
                            .find(|c| c.ch != ' ')
                            .copied()
                            .unwrap_or(blank);
                        let keep_until = right_col.unwrap_or(orig_width).min(region.cols);
                        let left_part: String = trimmed.chars().take(keep_until).collect();
                        let left_trimmed = left_part.trim_end_matches(' ');
                        let mut cells = Vec::with_capacity(orig_width);
                        if has_shortcuts && lead_spaces <= 2 && left_trimmed.len() >= lead_spaces {
                            cells.extend(std::iter::repeat_n(blank, lead_spaces));
                            cells.extend(
                                "YOLO on \u{00B7} "
                                    .chars()
                                    .map(|ch| RenderCell { ch, ..style }),
                            );
                            cells.extend(
                                left_trimmed[lead_spaces..]
                                    .chars()
                                    .map(|ch| RenderCell { ch, ..style }),
                            );
                        } else {
                            for (idx, ch) in left_trimmed.chars().enumerate() {
                                let base = pane_slice.get(idx).copied().unwrap_or(style);
                                cells.push(RenderCell { ch, ..base });
                            }
                        }
                        if subagents_badge.is_some() || tasks_badge.is_some() {
                            let mut badge_cells = Vec::new();
                            if let Some(tasks) = &tasks_badge {
                                for ch in tasks.chars() {
                                    badge_cells.push(RenderCell {
                                        ch,
                                        fg: inks.quote,
                                        ..blank
                                    });
                                }
                            }
                            if let Some(subs) = &subagents_badge {
                                if !badge_cells.is_empty() {
                                    for ch in " \u{00B7} ".chars() {
                                        badge_cells.push(RenderCell {
                                            ch,
                                            fg: inks.quiet,
                                            ..blank
                                        });
                                    }
                                }
                                badge_cells.push(RenderCell {
                                    ch: '●',
                                    fg: inks.mark,
                                    bold: true,
                                    ..blank
                                });
                                badge_cells.push(blank);
                                push_rainbow_text(subs, 0.08, region.cols, blank, &mut badge_cells);
                            }
                            let target_width = orig_width.min(region.cols);
                            if cells.len() + 2 + badge_cells.len() <= target_width {
                                let pad_to = target_width - badge_cells.len();
                                while cells.len() < pad_to {
                                    cells.push(blank);
                                }
                                cells.extend(badge_cells);
                            }
                        }
                        while cells.len() < orig_width {
                            cells.push(blank);
                        }
                        cells.truncate(region.cols);
                        out.push(AgyRowEdit {
                            frame_row,
                            col: region.col_off,
                            cells,
                            ground: blank,
                        });
                    }
                }
            }
        }
    }
}

/// `read` over pane `session`'s rows inside the reader's panic fence (`crate::reader_guard`).
fn read_footer(
    session: u64,
    rows: &[String],
    read: impl FnOnce(&[String]) -> FooterRead,
) -> FooterRead {
    crate::reader_guard::read_or_none(session, "Claude Code footer", rows, || read(rows)).flatten()
}

/// Reconstructs the display string of `row` on the glass.
#[cfg(test)]
pub(crate) fn read_glass(row: &[RenderCell]) -> String {
    use aterm_core::terminal::Script;
    let glyph = |c: &RenderCell| matches!(c.script, Script::Sub { .. }) && !c.wide && c.ch != ' ';
    let in_run =
        |c: &RenderCell| matches!(c.script, Script::Sub { .. }) || (c.ch == ' ' && !c.wide);
    let step = 1.0 / f32::from(Script::NUDGES_PER_CELL);
    let mut out = String::new();
    let mut c = 0;
    while c < row.len() {
        if !glyph(&row[c]) {
            out.push(row[c].ch);
            c += 1;
            continue;
        }
        let mut end = c;
        let mut k = c;
        while k < row.len() && in_run(&row[k]) {
            if glyph(&row[k]) || row[k].wide {
                end = k + 1;
            }
            k += 1;
        }
        let mut last: Option<(f32, usize)> = None;
        for (k, cell) in row.iter().enumerate().take(end).skip(c) {
            let Script::Sub { nudge } = cell.script else {
                continue;
            };
            if !glyph(cell) {
                continue;
            }
            let cols = if row.get(k + 1).is_some_and(|n| n.wide) {
                2
            } else {
                1
            };
            let at = k as f32 + cols as f32 / 2.0 + f32::from(nudge) * step;
            if let Some((before, before_cols)) = last {
                let room = (at - before) / footer::FACT_PITCH - (before_cols + cols) as f32 / 2.0;
                out.extend(std::iter::repeat_n(' ', room.round().max(0.0) as usize));
            }
            out.push(cell.ch);
            last = Some((at, cols));
        }
        c = end;
    }
    out
}

/// Benchmark and cost-test fixture for a single-pane footer pass.
#[cfg(any(test, feature = "bench-support"))]
#[path = "claude_footer_probe.rs"]
pub(crate) mod probe;

#[cfg(test)]
#[path = "claude_footer_cost_tests.rs"]
mod cost_tests;

#[cfg(all(test, unix))]
#[path = "claude_footer_weekly_tests.rs"]
mod weekly_tests;

#[cfg(test)]
#[path = "claude_footer_top_tests.rs"]
mod top_tests;

#[cfg(test)]
#[path = "claude_footer_top_lines_tests.rs"]
mod top_lines_tests;

#[cfg(test)]
#[path = "claude_footer_floor_tests.rs"]
mod floor_tests;

#[cfg(all(test, unix))]
#[path = "claude_footer_yolo_tests.rs"]
mod yolo_tests;

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
#[path = "claude_footer_registry_tests.rs"]
mod registry_tests;

/// Test harness for resolving footer facts from a scratch Claude home directory.
#[cfg(test)]
pub(crate) mod rig {
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};

    use aterm_agent::harness::footer;

    use super::{Asks, Identity, LaunchFacts, LogWake, NO_TAP, RosterMemo};
    use crate::session_log::transcript::Applied;
    use crate::session_timeline::SessionTimeline;

    /// Default session UUID written into test registry fixtures.
    pub(crate) const SESSION_ID: &str = "00000000-0000-4000-8000-000000000a01";

    /// Returns a hermetic test home directory under `test_root`.
    pub(crate) fn scratch_home() -> Option<PathBuf> {
        aterm_types::dirs::test_root().map(|root| root.join("home"))
    }

    /// Returns a short temporary directory path for test fixture `name`.
    pub(crate) fn home_root(name: &str) -> PathBuf {
        crate::test_scratch::short_process_root("aterm-cf-").join(name)
    }

    /// Creates a scratch Claude home directory and session registry file for `pid`.
    pub(crate) fn claude_home(name: &str, pid: i32, started_at: u64) -> (PathBuf, PathBuf) {
        let root = home_root(name);
        let _ = std::fs::remove_dir_all(&root);
        claude_home_at(root, pid, started_at)
    }

    /// Creates a Claude registry fixture within a caller-owned root.
    pub(crate) fn claude_home_at(root: PathBuf, pid: i32, started_at: u64) -> (PathBuf, PathBuf) {
        let (cwd, claude) = (root.join("work"), root.join("claude"));
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::create_dir_all(claude.join("sessions")).unwrap();
        std::fs::write(
            claude.join(format!("sessions/{pid}.json")),
            format!(
                r#"{{"pid":{pid},"sessionId":"{SESSION_ID}","cwd":{},"startedAt":{},"version":"2.1.287"}}"#,
                aterm_json::to_string(&cwd.display().to_string()).unwrap(),
                started_at * 1000 + 684
            ),
        )
        .unwrap();
        (root, claude)
    }

    /// Returns the transcript JSONL path for a [`claude_home`] fixture.
    pub(crate) fn transcript_of(root: &Path, claude: &Path) -> PathBuf {
        let dir = claude
            .join("projects")
            .join(footer::project_slug(&root.join("work")));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(format!("{SESSION_ID}.jsonl"))
    }

    /// Executes a single synchronous footer-fact read for `pid` into `timeline`.
    pub(crate) fn read(
        timeline: &Arc<Mutex<SessionTimeline>>,
        pid: i32,
        claude: &Path,
        fold: &mut footer::FooterCache,
        tail: &mut footer::TailCache,
        session: u64,
        wake: &LogWake,
    ) -> Applied {
        let mut identity = Identity {
            pid,
            started: None,
            dir: Some(claude.to_path_buf()),
            launch: LaunchFacts::default(),
            launch_for: None,
            login: None,
            argv: Vec::new(),
            task: None,
            login_checked_for: None,
            stored_login: false,
            unoffered: None,
            env_stamp: None,
        };
        let asks = Asks {
            files: |_| None,
            tap: NO_TAP,
            args: |_| {
                Some(atpkg::caller_shell::ProcArgs {
                    argv: vec!["claude".into()],
                    env: vec!["HOME=/home/someone".into()],
                    ..Default::default()
                })
            },
            roster: || None,
            home: scratch_home,
            stored_login: |_, _, _| true,
        };
        let (_, _, applied) = super::resolve_into(
            timeline,
            &mut identity,
            tail,
            fold,
            &mut RosterMemo::default(),
            asks,
        );
        wake.after(session, &applied);
        applied
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_switch_faces_give_words_up_before_the_switch() {
        use crate::autopilot::{HarnessSwitch, SwitchOff};
        let paint = |face| SwitchPaint {
            face,
            hover: false,
            version: None,
        };
        let faces = |p: SwitchPaint| -> Vec<SwitchFace> {
            (0..SWITCH_TIERS)
                .filter_map(|t| switch_face(p, t))
                .collect()
        };
        let texts =
            |p: SwitchPaint| -> Vec<String> { faces(p).iter().map(SwitchFace::text).collect() };
        assert_eq!(
            texts(paint(HarnessSwitch::On)),
            [
                " alab harness \u{25cf} on ",
                " alab \u{25cf} on ",
                " \u{25cf} on "
            ]
        );
        assert_eq!(
            texts(SwitchPaint {
                face: HarnessSwitch::On,
                hover: false,
                version: Some("0.1"),
            }),
            [
                " alab harness v0.1 \u{2590} \u{25cf} on \u{258c} ",
                " alab \u{2590} \u{25cf} on \u{258c} ",
                " \u{2590} \u{25cf} on \u{258c} "
            ]
        );
        for off in [
            SwitchOff::Session,
            SwitchOff::Master,
            SwitchOff::Agent(aterm_phase::Program::Claude),
        ] {
            let p = paint(HarnessSwitch::Off(off));
            assert_eq!(
                texts(p),
                [
                    " alab harness \u{25cb} off \u{2590} on \u{258c} ",
                    " alab \u{25cb} off \u{2590} on \u{258c} ",
                    " \u{25cb} off \u{2590} on \u{258c} ",
                    " \u{2590} on \u{258c} ",
                ],
                "{off:?}"
            );
            for f in faces(p) {
                let button = f.button.clone().expect("every off form has its button");
                let drawn: String = f.cells()[button].iter().map(|c| c.0).collect();
                assert_eq!(drawn, "\u{2590} on \u{258c}");
            }
        }
        assert_eq!(
            texts(paint(HarnessSwitch::Turning)),
            [
                " alab harness \u{25d0} turning on ",
                " alab \u{25d0} turning on ",
                " \u{25d0} turning on ",
                " \u{25d0} on ",
            ]
        );
        let versions = std::iter::once(None).chain(
            aterm_phase::Program::ALL
                .into_iter()
                .map(|p| p.harness_version()),
        );
        for version in versions {
            for face in [
                HarnessSwitch::On,
                HarnessSwitch::Off(SwitchOff::Session),
                HarnessSwitch::Turning,
            ] {
                let p = SwitchPaint {
                    face,
                    hover: false,
                    version,
                };
                for f in faces(p) {
                    assert!(f.len < SWITCH_FACE_MAX, "{face:?}: {:?}", f.text());
                }
            }
        }
        let off = paint(HarnessSwitch::Off(SwitchOff::Session));
        for (tier, face) in faces(off).into_iter().enumerate() {
            let room = face.len + 4;
            assert_eq!(
                fitted_switch_face(off, room).map(|(t, _)| t),
                Some(tier),
                "{tier} fits its room"
            );
            assert_eq!(
                fitted_switch_face(off, room - 1).map(|(t, _)| t),
                (tier + 1 < SWITCH_TIERS).then_some(tier + 1),
                "CONTROL: a cell short of {tier}"
            );
        }
    }

    /// A versioned lit face's pill is the switch's own cells in every form, never the off face's
    /// `on` button, and keeps its caps and fill on a themed line; every off form keeps its button.
    #[test]
    fn a_lit_pill_is_the_switchs_own_state_not_an_on_button() {
        use crate::autopilot::{HarnessSwitch, SwitchOff};
        let pill = "\u{2590} \u{25cf} on \u{258c}";
        let paint = |face| SwitchPaint {
            face,
            hover: false,
            version: Some("0.1"),
        };
        let lit: Vec<SwitchFace> = (0..SWITCH_TIERS)
            .filter_map(|t| switch_face(paint(HarnessSwitch::On), t))
            .collect();
        let texts: Vec<String> = lit.iter().map(SwitchFace::text).collect();
        assert_eq!(lit.len(), 3, "{texts:?}");
        for f in &lit {
            let switch: String = f.cells()[f.switch.clone()].iter().map(|c| c.0).collect();
            assert!(switch.ends_with(pill), "{:?}: {switch:?}", f.text());
            assert_eq!(f.button, None, "{:?}", f.text());
        }
        let mut arced = lit[0].clone();
        arced.on_arc();
        let glyphs: Vec<SwitchInk> = arced.cells()[arced.switch.clone()]
            .iter()
            .filter(|c| c.0 != ' ')
            .map(|c| c.1)
            .collect();
        let (words, caps) = glyphs.split_at(glyphs.len() - 5);
        assert_eq!(
            caps,
            [
                SwitchInk::Cap,
                SwitchInk::Button,
                SwitchInk::Button,
                SwitchInk::Button,
                SwitchInk::Cap
            ],
            "the pill keeps its inks on a themed line"
        );
        assert_eq!(
            (words.first(), words.last()),
            (Some(&SwitchInk::Arc(0)), Some(&SwitchInk::Arc(u8::MAX))),
            "the arc runs through the words"
        );
        let off = paint(HarnessSwitch::Off(SwitchOff::Session));
        for f in (0..SWITCH_TIERS).filter_map(|t| switch_face(off, t)) {
            let button = f.button.clone().expect("CONTROL: every off form's button");
            assert!(f.switch.end <= button.start, "CONTROL: {:?}", f.text());
        }
    }

    #[test]
    fn the_switch_arc_reads_on_paper() {
        use aterm_effects::color_math::relative_luminance;
        use aterm_effects::harness_card::CardPalette;
        use aterm_types::scheme::{Appearance, builtin, builtin_names};
        let contrast = |a: [u8; 3], b: [u8; 3]| {
            let l = |c: [u8; 3]| relative_luminance(u32::from_be_bytes([0, c[0], c[1], c[2]]));
            let (la, lb) = (l(a), l(b));
            (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
        };
        let rgb = |c: aterm_types::Rgb| [c.r, c.g, c.b];
        let px = |c: u32| [(c >> 16) as u8, (c >> 8) as u8, c as u8];
        let (mut light, mut washed, mut quieted) = (0, f32::MAX, f32::MAX);
        let turning = SwitchPaint {
            face: crate::autopilot::HarnessSwitch::Turning,
            hover: false,
            version: None,
        };
        for name in builtin_names() {
            let Some(scheme) = builtin(name) else {
                continue;
            };
            let blank = RenderCell {
                fg: rgb(scheme.foreground),
                bg: rgb(scheme.background),
                ..RenderCell::default()
            };
            let accent = rgb(scheme.ansi[4]);
            let bare = switch_cell('\u{25d0}', SwitchInk::Turning, turning, None, blank, accent);
            let k = contrast(bare.fg, blank.bg);
            assert!(k >= 4.5, "{name}: the bare settling switch at {k:.2}:1");
            for palette in [
                aterm_effects::harness_card::CardPalette::Cruise,
                aterm_effects::harness_card::CardPalette::Land,
                aterm_effects::harness_card::CardPalette::Off,
            ] {
                for stop in 0..255u8 {
                    let t = f32::from(stop) / 255.0;
                    let settling = switch_cell(
                        't',
                        SwitchInk::Arc(stop),
                        turning,
                        Some(palette),
                        blank,
                        accent,
                    );
                    let k = contrast(settling.fg, blank.bg);
                    assert!(
                        k >= 4.5 && !settling.bold,
                        "{name} {palette:?} settling at {t:.3}: {k:.2}:1"
                    );
                    if scheme.appearance == Appearance::Light {
                        let raw = mix(brand_letter_fg(palette, t, blank), blank.bg, 0.3);
                        quieted = quieted.min(contrast(raw, blank.bg));
                    }
                }
                for stop in 0..255 {
                    let t = stop as f32 / 254.0;
                    let fg = brand_letter_fg(palette, t, blank);
                    let what = format!("{name} {palette:?} at {t:.3}: {fg:?}");
                    if scheme.appearance == Appearance::Light {
                        light += 1;
                        let k = contrast(fg, blank.bg);
                        assert!(k >= 4.5, "{what} at {k:.2}:1");
                        if palette == CardPalette::Off {
                            let spread =
                                fg.iter().max().unwrap_or(&0) - fg.iter().min().unwrap_or(&0);
                            assert!(spread <= 32, "{what} is not a silver");
                        } else {
                            assert!(fg.contains(&0), "{what} is not a pure hue");
                        }
                        let c = aterm_effects::spectrum::SPECTRUM_LUT[stop];
                        washed = washed.min(contrast(mix(px(c), blank.fg, 0.25), blank.bg));
                    } else {
                        assert_eq!(
                            fg,
                            mix(px(palette.letter(t, true)), blank.fg, 0.25),
                            "{what}: dark, the palette's letter lifted"
                        );
                    }
                }
            }
        }
        assert!(light >= 3 * 7 * 255, "{light} light letters");
        assert!(washed < 2.0, "CONTROL: {washed:.2}");
        assert!(quieted < 3.0, "CONTROL: the quieting alone, {quieted:.2}");
    }

    #[test]
    fn the_footer_is_drawn_for_every_name_the_host_supervises_as_claude_code() {
        use crate::session_timeline::AgentPublication;
        for name in [
            "claude",
            "claude-code",
            "Claude",
            "CLAUDE-CODE",
            "agy",
            "AGY",
            "muse",
            "MUSE",
            "muse-code",
            "codex",
            "node",
            "bun",
            "zsh",
            "sh",
            "claudette",
            "claude.exe",
            "",
        ] {
            let publication = AgentPublication {
                program: Some(name.to_string()),
                ..AgentPublication::default()
            };
            let host = crate::harness_host::agent_of(&publication).is_some_and(|p| match p {
                aterm_phase::Program::Claude
                | aterm_phase::Program::Agy
                | aterm_phase::Program::Muse => true,
                aterm_phase::Program::Codex
                | aterm_phase::Program::Grok
                | aterm_phase::Program::Kimi
                | aterm_phase::Program::Gemini
                | aterm_phase::Program::Generic => false,
            });
            assert_eq!(
                drawn_for(Some(name)),
                host,
                "{name:?}: the footer and the host must agree"
            );
        }
        assert!(
            drawn_for(Some("claude-code")) && drawn_for(Some("agy")) && drawn_for(Some("muse")),
            "claude-code, agy, and muse argv0s are supervised, so they get the footer"
        );
        assert!(!drawn_for(Some("codex")) && !drawn_for(None));
    }

    #[test]
    fn the_chips_keep_a_blank_cell_on_each_side_at_every_width() {
        let blank = cell(' ', [200, 200, 200]);
        let rule = cell('\u{2500}', [90, 90, 90]);
        let cruise = format!(
            "autopilot: cruise \u{2014} {} \u{b7} click: land  ",
            crate::autopilot::mode_words(aterm_agent::supervise::Autopilot::Cruise).chip
        );
        for facts in [facts(), facts_with_usage(), FooterFacts::default()] {
            for chips in [1usize, 3, 6, 8, 10, 14] {
                for (title, short) in [
                    ("", ""),
                    ("Permission mode: plan  ", "plan  "),
                    (cruise.as_str(), "autopilot: cruise  "),
                ] {
                    let block = crate::claude_lights::Block::of_chips(
                        row_of(&"\u{25CB}".repeat(chips), [150, 150, 150]),
                        row_of(title, [150, 150, 150]),
                        row_of(short, [150, 150, 150]),
                        false,
                    );
                    let widths = footer::LightsWidth {
                        chips,
                        title: title.chars().count(),
                        lean: [0; footer::LEAN_FORMS],
                        short: short.chars().count(),
                        reason: false,
                    };
                    for room in 0usize..160 {
                        let fit = footer::fit_rule(&facts, room, Some(widths));
                        if !fit.chips {
                            assert!(
                                chips > footer::chips_room(&facts, room),
                                "{room}/{chips}: the chips fit, yet were not drawn"
                            );
                            continue;
                        }
                        let (cells, at) =
                            compose_rule(&fit, Some(&block), rule, blank, [0, 128, 255], room);
                        let at = at.expect("drawn chips have a column");
                        let row = text_of(&cells);
                        assert!(cells.len() <= room, "{room}: {row:?}");
                        assert_eq!(cells[at - 1].ch, ' ', "{room}/{chips} before: {row:?}");
                        assert_eq!(cells[at + chips].ch, ' ', "{room}/{chips} after: {row:?}");
                    }
                }
            }
        }
    }

    fn watch_model_projection(schedule: &WatchSchedule) -> i64 {
        match schedule.watched.get(&7).map(|watch| watch.job.pgid) {
            None => 0,
            Some(42) => 1,
            Some(43) => 2,
            other => panic!("unexpected foreground group: {other:?}"),
        }
    }

    #[test]
    fn stopped_footer_watch_has_no_followup_or_idle_reads() {
        let model = aterm_models_harness::claude_footer_watch_model();
        let mut state = model.init_state();
        let now = std::time::Instant::now();
        let timeline = Arc::new(Mutex::new(SessionTimeline::default()));
        let weak = Arc::downgrade(&timeline);
        let mut schedule = WatchSchedule::default();
        schedule.watch(
            Job {
                session: 7,
                timeline,
                pgid: 42,
                wake: LogWake::default(),
            },
            now,
            false,
        );
        assert!(model.fire("AskOld", &mut state));
        assert_eq!(state["watch"], watch_model_projection(&schedule));
        assert_eq!(
            model.action_enabled("IdleRead", &state),
            schedule.due_idle(now + IDLE_RECHECK).is_some()
        );
        assert!(schedule.watched[&7].awaiting);
        assert_eq!(schedule.next(now), Some(now + FOLLOW_UPS[0]));
        let Accepted {
            latest: immediate,
            stopped,
            ..
        } = schedule.accept([Command::Stop {
            session: 7,
            pgid: None,
        }]);
        assert!(model.fire("StopSession", &mut state));
        assert!(immediate.is_empty());
        assert_eq!(stopped, [7]);
        assert_eq!(state["watch"], watch_model_projection(&schedule));
        assert!(schedule.next(now).is_none());
        assert!(schedule.due_followups(now + FOLLOW_UPS[1]).is_empty());
        assert_eq!(
            model.action_enabled("IdleRead", &state),
            schedule.due_idle(now + IDLE_RECHECK).is_some()
        );
        assert!(weak.upgrade().is_none(), "the watch released its timeline");
    }

    #[test]
    fn stale_group_stop_preserves_replacement_footer_watch() {
        let model = aterm_models_harness::claude_footer_watch_model();
        let mut state = model.init_state();
        let now = std::time::Instant::now();
        let timeline = Arc::new(Mutex::new(SessionTimeline::default()));
        let mut schedule = WatchSchedule::default();
        schedule.watch(
            Job {
                session: 7,
                timeline: Arc::clone(&timeline),
                pgid: 42,
                wake: LogWake::default(),
            },
            now,
            true,
        );
        assert!(model.fire("AskOld", &mut state));
        let Accepted {
            latest: mut immediate,
            stopped,
            ..
        } = schedule.accept([
            Command::Request(Job {
                session: 7,
                timeline,
                pgid: 43,
                wake: LogWake::default(),
            }),
            Command::Stop {
                session: 7,
                pgid: Some(42),
            },
        ]);
        assert!(model.fire("AskNew", &mut state));
        assert!(model.fire("StopOld", &mut state));
        assert_eq!(stopped, [7]);
        assert_eq!(immediate.len(), 1);
        let replacement = immediate.pop().unwrap();
        assert_eq!(replacement.pgid, 43);
        schedule.watch(replacement, now, true);
        assert_eq!(state["watch"], watch_model_projection(&schedule));
        let Accepted {
            latest: immediate,
            stopped,
            ..
        } = schedule.accept([Command::Stop {
            session: 7,
            pgid: Some(42),
        }]);
        assert!(immediate.is_empty());
        assert!(stopped.is_empty());
        let due = schedule
            .due_idle(now + IDLE_RECHECK)
            .expect("active Claude keeps its idle refresh");
        assert_eq!(due.pgid, 43);
        assert!(model.action_enabled("IdleRead", &state));
    }

    #[test]
    fn a_watch_lapse_keeps_the_processs_pin() {
        use std::time::{Instant, UNIX_EPOCH};
        const START: u64 = 1_790_607_848; // 2026-09-28T15:04:08Z
        const S: &str = "00000000-0000-4000-8000-000000000c01";
        const A: &str = "00000000-0000-4000-8000-000000000c02";
        const B: &str = "00000000-0000-4000-8000-000000000c03";
        let root = super::rig::home_root("footer-lapse");
        let _ = std::fs::remove_dir_all(&root);
        let cwd = root.join("work");
        let claude = root.join("claude");
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::create_dir_all(claude.join("sessions")).unwrap();
        let project = claude.join("projects").join(footer::project_slug(&cwd));
        std::fs::create_dir_all(&project).unwrap();
        let register = |session: &str| {
            std::fs::write(
                claude.join("sessions/4242.json"),
                format!(
                    r#"{{"pid":4242,"sessionId":"{session}","cwd":{},"procStart":"{}","startedAt":{},"version":"2.1.284"}}"#,
                    aterm_json::to_string(&cwd.display().to_string()).unwrap(),
                    footer::lstart_utc(START),
                    START * 1000 + 684
                ),
            )
            .unwrap();
        };
        let answer = |session: &str, model: &str, at: &str| {
            std::fs::write(
                project.join(format!("{session}.jsonl")),
                format!(
                    "{{\"type\":\"assistant\",\"timestamp\":\"{at}\",\"effort\":\"high\",\"message\":{{\"model\":\"{model}\"}}}}\n"
                ),
            )
            .unwrap();
        };
        answer(S, "claude-opus-5-5", "2026-09-28T15:05:00.000Z");
        answer(A, "claude-fable-5-1", "2026-09-27T10:00:00.000Z");
        answer(B, "claude-sonnet-5", "2026-09-26T10:00:00.000Z");
        let read = |tails: &mut ProcessTails, started: Option<u64>, secs: u64, now: Instant| {
            let entry = footer::session_of_pid(&claude, 4242, started).expect("the registry");
            footer::facts_for_entry_at(
                &claude,
                4242,
                started,
                &entry,
                &LaunchFacts::default(),
                tails.cache(7, 4242, started, now),
                UNIX_EPOCH + Duration::from_secs(START + secs),
            )
            .model
        };
        let fable = Some("Fable 5.1".to_owned());
        let run = |started: Option<u64>, how: &str, lives: bool| {
            let t0 = Instant::now();
            let mut schedule = WatchSchedule::default();
            schedule.watch(
                Job {
                    session: 7,
                    timeline: Arc::new(Mutex::new(SessionTimeline::default())),
                    pgid: 4242,
                    wake: LogWake::default(),
                },
                t0,
                true,
            );
            let mut tails = ProcessTails::default();
            register(S);
            assert_eq!(
                read(&mut tails, started, 120, t0).as_deref(),
                Some("Opus 5.5")
            );
            register(A);
            assert_eq!(read(&mut tails, started, 180, t0), fable, "restored");
            let later = t0 + WATCH_FOR;
            match how {
                "lapse" => schedule.prune(later, |_, _| false),
                "stop" => {
                    let Accepted { stopped, .. } = schedule.accept([Command::Stop {
                        session: 7,
                        pgid: Some(4242),
                    }]);
                    assert_eq!(stopped, [7]);
                }
                other => panic!("{other}"),
            }
            assert!(schedule.watched.is_empty(), "the watch is gone");
            tails.retain(
                &schedule,
                |pgid, born| lives && (pgid, born) == (4242, START),
                later,
            );
            register(B);
            read(&mut tails, started, 900, later)
        };
        assert_eq!(
            run(Some(START), "lapse", true),
            fable,
            "the pin outlived the lapse"
        );
        assert_eq!(run(Some(START), "stop", true), fable, "and the stop");
        assert_eq!(run(Some(START), "lapse", false), None);
        assert_eq!(run(None, "lapse", true), None);
        let t0 = Instant::now();
        let mut tails = ProcessTails::default();
        tails.cache(7, 4242, Some(START), t0);
        for n in 0..MAX_KEPT {
            let n64 = u64::try_from(n).unwrap();
            tails.cache(
                8 + n64,
                5000 + i32::try_from(n).unwrap(),
                Some(1),
                t0 + Duration::from_secs(1 + n64),
            );
        }
        assert_eq!(tails.kept.len(), MAX_KEPT);
        assert!(!tails.kept.contains_key(&(4242, Some(START))));
        assert!(tails.kept.contains_key(&(5000, Some(1))));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn pane_text_reuses_only_the_same_scratch_revision_and_pane() {
        let mut scratch = RenderInput::empty();
        scratch.rows = 4;
        scratch.cols = 4;
        scratch.cells = vec![
            row_of("chrome", [0, 0, 0]),
            row_of("abcd", [0, 0, 0]),
            row_of("efgh", [0, 0, 0]),
            row_of("ijkl", [0, 0, 0]),
        ];
        scratch.snapshot_seq = 5;
        let mut cache = PaneTextCache::default();
        let full = PaneTextRegion {
            strip: 1,
            row_off: 0,
            col_off: 0,
            rows: 3,
            cols: 4,
        };
        let first = cache.rows(0, 7, &scratch, full);
        assert_eq!(&**first, &["abcd", "efgh", "ijkl"]);
        let again = cache.rows(0, 7, &scratch, full);
        assert!(Arc::ptr_eq(&first, &again));

        let other_pane = cache.rows(
            1,
            7,
            &scratch,
            PaneTextRegion {
                strip: 1,
                row_off: 1,
                col_off: 1,
                rows: 2,
                cols: 2,
            },
        );
        assert_eq!(&**other_pane, &["fg", "jk"]);
        assert!(!Arc::ptr_eq(&first, &other_pane));
        let other_session = cache.rows(0, 8, &scratch, full);
        assert_eq!(&**other_session, &**first);
        assert!(!Arc::ptr_eq(&first, &other_session));

        scratch.cells[1][0].ch = 'z';
        scratch.snapshot_seq += 1;
        let changed = cache.rows(0, 8, &scratch, full);
        assert_eq!(&changed[0], "zbcd");
        assert!(!Arc::ptr_eq(&other_session, &changed));
        cache.retain_active(|index| index == 1, |_| false, |_| false);
        assert!(cache.panes[0].is_none());
        assert!(cache.panes[1].is_some());
        cache.trim(1);
        assert_eq!(cache.panes.len(), 1);
        cache.trim(0);
        assert!(cache.panes.is_empty());
    }

    #[test]
    fn changed_claude_frame_reuses_free_row_buffers_but_preserves_held_snapshots() {
        let mut scratch = RenderInput::empty();
        scratch.rows = 2;
        scratch.cols = 4;
        scratch.cells = vec![row_of("abcd", [0, 0, 0]), row_of("efgh", [0, 0, 0])];
        scratch.snapshot_seq = 1;
        let region = PaneTextRegion {
            strip: 0,
            row_off: 0,
            col_off: 0,
            rows: 2,
            cols: 4,
        };
        let mut cache = PaneTextCache::default();
        let first = cache.rows(0, 7, &scratch, region);
        let vec_ptr = first.as_ref().as_ptr();
        let row_ptrs = [first[0].as_ptr(), first[1].as_ptr()];
        drop(first);

        scratch.cells[0][0].ch = 'z';
        scratch.snapshot_seq += 1;
        let reused = cache.rows(0, 7, &scratch, region);
        assert_eq!(&**reused, &["zbcd", "efgh"]);
        assert_eq!(reused.as_ref().as_ptr(), vec_ptr);
        assert_eq!([reused[0].as_ptr(), reused[1].as_ptr()], row_ptrs);

        let held = Arc::clone(&reused);
        scratch.cells[0][1].ch = 'y';
        scratch.snapshot_seq += 1;
        let next = cache.rows(0, 7, &scratch, region);
        assert_eq!(&**held, &["zbcd", "efgh"]);
        assert_eq!(&**next, &["zycd", "efgh"]);
        assert!(!Arc::ptr_eq(&held, &next));

        let rebuild = |cache: &PaneTextCache| cache.panes[0].as_ref().unwrap().text_gen;
        let before = rebuild(&cache);
        scratch.cells[1][0].fg = [9, 9, 9];
        scratch.snapshot_seq += 1;
        let (same, allocations) =
            crate::work_counts::allocations_in(|| cache.rows(0, 7, &scratch, region));
        assert!(Arc::ptr_eq(&same, &next), "the held snapshot, shared");
        assert_eq!(allocations, 0);
        assert_ne!(rebuild(&cache), before, "a rebuild all the same");
    }

    fn vendor_frame(scratch: &mut RenderInput) {
        let bottom = scratch.rows - 1;
        let rule = "\u{2500}".repeat(scratch.cols);
        scratch.cells[bottom - 3] = row_of(&rule, [40, 40, 40]);
        scratch.cells[bottom - 2] = row_of("\u{276F} ", [40, 40, 40]);
        scratch.cells[bottom - 1] = row_of(&rule, [40, 40, 40]);
        scratch.cells[bottom] = row_of(
            "  \u{23F5}\u{23F5} bypass permissions on (shift+tab to cycle) \u{00B7} \u{2190} for agents",
            [90, 90, 90],
        );
        scratch.snapshot_seq = scratch.snapshot_seq.wrapping_add(1);
    }

    fn cell(ch: char, fg: [u8; 3]) -> RenderCell {
        RenderCell {
            ch,
            fg,
            ..RenderCell::default()
        }
    }

    fn row_of(text: &str, fg: [u8; 3]) -> Vec<RenderCell> {
        text.chars().map(|c| cell(c, fg)).collect()
    }

    fn text_of(row: &[RenderCell]) -> String {
        read_glass(row)
    }

    fn cells_for(reading: &str) -> usize {
        let segments: Vec<footer::Segment> = reading
            .trim()
            .split("   ")
            .map(|fact| {
                let (mark, text) = fact.split_once(' ').expect("<mark> <text>");
                footer::Segment::plain(mark.chars().next().expect("a mark"), text)
            })
            .collect();
        footer::segments_width(&segments) + 2
    }

    fn unplaced(cell: RenderCell) -> RenderCell {
        match cell.script {
            aterm_core::terminal::Script::Sub { .. } => RenderCell {
                script: aterm_core::terminal::Script::Sub { nudge: 0 },
                ..cell
            },
            aterm_core::terminal::Script::Normal => cell,
        }
    }

    fn facts() -> FooterFacts {
        FooterFacts {
            model: Some("Opus 5.5".into()),
            effort: Some("xhigh".into()),
            path: Some("~/aterm".into()),
            branch: Some("main".into()),
            ..FooterFacts::default()
        }
    }

    #[test]
    fn a_reader_panic_paints_no_footer_row() {
        let rows = vec![
            "\u{2500}".repeat(60),
            "\u{276F} ".to_string(),
            "\u{2500}".repeat(60),
            "  \u{23F5}\u{23F5} bypass permissions on (shift+tab to cycle)".to_string(),
        ];
        assert_eq!(
            read_footer(9, &rows, read_rule_row),
            Some(RuleRead {
                rule: 2,
                mode_row: true
            }),
            "the real reader finds it"
        );
        let boom = |_: &[String]| -> FooterRead { panic!("stand-in reader panic") };
        assert!(read_footer(9, &rows, boom).is_none());
        assert!(read_footer(9, &rows, boom).is_none(), "every frame");
    }

    #[test]
    fn the_rule_carries_the_owners_footer() {
        let blank = cell(' ', [200, 200, 200]);
        let rule = cell('\u{2500}', [60, 60, 60]);
        let fit = footer::fit_rule(&facts(), 76, None);
        let (cells, lights) = compose_rule(&fit, None, rule, blank, [0, 128, 255], 76);
        assert_eq!(lights, None);
        assert_eq!(
            text_of(&cells),
            " \u{25C6} Opus 5.5 xhigh   \u{2302} ~/aterm   \u{2387} main "
        );
        let o = cells.iter().position(|c| c.ch == 'O').expect("the model");
        assert!(
            [1, o].iter().all(|&c| cells[c].bold
                && matches!(cells[c].script, aterm_core::terminal::Script::Sub { .. })),
            "the facts are drawn small, low and bold"
        );
        assert_eq!(cells[0].script, aterm_core::terminal::Script::Normal);
        assert_eq!(cells.len(), footer::segments_width(&fit.segments) + 2);
        let none = footer::fit_rule(&FooterFacts::default(), 76, None);
        assert!(
            compose_rule(&none, None, rule, blank, [1, 1, 1], 76)
                .0
                .is_empty()
        );
        let harness = crate::claude_lights::Block::of_autopilot(
            crate::autopilot::AutopilotView::Cruise,
            blank,
            [0, 128, 255],
        );
        let widths = footer::LightsWidth {
            chips: harness.lights.len(),
            title: harness.title.len(),
            lean: [0; footer::LEAN_FORMS],
            short: harness.short_title.len(),
            reason: harness.reason,
        };
        let fit = footer::fit_rule(&facts(), 76, Some(widths));
        let (cells, lights) = compose_rule(&fit, Some(&harness), rule, blank, [0, 128, 255], 76);
        assert_eq!(lights, Some(1));
        assert_eq!(cells.len(), 76, "the rule's glyphs run on to the facts");
        let text = text_of(&cells);
        assert!(text.starts_with(" \u{221e} \u{2500}"), "{text:?}");
        assert!(
            text.ends_with(" \u{25C6} Opus 5.5 xhigh   \u{2302} ~/aterm   \u{2387} main "),
            "{text:?}"
        );
        assert_eq!(cells[1].fg, [0, 128, 255], "cruise in the accent");
        assert_eq!(cells[1].script, aterm_core::terminal::Script::Normal);
    }

    #[test]
    fn the_facts_are_painted_where_they_are_set() {
        let blank = cell(' ', [200, 200, 200]);
        let rule = cell('\u{2500}', [60, 60, 60]);
        let fit = footer::fit_rule(&facts(), 76, None);
        let (cells, _) = compose_rule(&fit, None, rule, blank, [0, 128, 255], 76);
        let set = footer::set_facts(&fit.segments);
        assert_eq!(cells.len(), set.cells + 2);
        let mut glyph_cells = std::collections::HashSet::new();
        for glyph in &set.glyphs {
            let painted = cells[1 + glyph.cell];
            assert_eq!(painted.ch, glyph.ch);
            assert_eq!(
                painted.script,
                aterm_core::terminal::Script::Sub { nudge: glyph.nudge },
                "{glyph:?}"
            );
            glyph_cells.insert(1 + glyph.cell);
        }
        for (c, painted) in cells.iter().enumerate() {
            if !glyph_cells.contains(&c) {
                assert_eq!(*painted, blank, "cell {c} is blank");
            }
        }
        let reading = " \u{25C6} Opus 5.5 xhigh   \u{2302} ~/aterm   \u{2387} main ";
        assert_eq!(text_of(&cells), reading);
        assert!(
            cells.len() < reading.chars().count(),
            "CONTROL: {}",
            cells.len()
        );
    }

    #[test]
    fn a_lit_facts_underline_lies_under_its_ink() {
        use aterm_core::terminal::{Script, UnderlineStyle};
        let blank = cell(' ', [200, 200, 200]);
        let rule = cell('\u{2500}', [60, 60, 60]);
        let mark_fg = [0, 128, 255];
        let fit = footer::RuleFit {
            segments: vec![
                footer::Segment::plain(footer::ACCOUNT_MARK, "account unavailable"),
                footer::Segment::plain(footer::WEEKLY_MARK, "weekly unavailable"),
                footer::Segment {
                    split: Some("Opus 5.5 ".len()),
                    ..footer::Segment::plain(footer::MODEL_MARK, "Opus 5.5 xhigh")
                },
                footer::Segment::plain(footer::PATH_MARK, "/private/tmp/at.SnYH/proj"),
                footer::Segment::plain(footer::BRANCH_MARK, "main"),
            ],
            title: footer::TitleFit::None,
            chips: false,
        };
        let room = 120;
        let rest = compose_rule_spans(&fit, None, rule, blank, mark_fg, room, None);
        assert!(
            rest.cells
                .iter()
                .all(|c| c.underline == UnderlineStyle::None),
            "CONTROL: nothing underlined at rest"
        );
        let step = 1.0 / f32::from(Script::NUDGES_PER_CELL);
        for kind in [
            FactKind::Model,
            FactKind::Effort,
            FactKind::Path,
            FactKind::Branch,
        ] {
            let paint = compose_rule_spans(&fit, None, rule, blank, mark_fg, room, Some(kind));
            let span = paint
                .spans
                .iter()
                .flatten()
                .find(|span| span.kind == kind)
                .copied()
                .expect("the fact is on the line");
            let at = paint.facts_at + 1;
            let own = at + usize::from(span.cells.0)..at + usize::from(span.cells.1);
            let mut drawn: Vec<(f32, f32, [u8; 3])> = Vec::new();
            for c in own.clone() {
                let cell = paint.cells[c];
                if cell.ch == ' ' {
                    continue;
                }
                let Script::Sub { nudge } = cell.script else {
                    panic!("{kind:?}: cell {c} is no set glyph");
                };
                let cols = if paint.cells.get(c + 1).is_some_and(|n| n.wide) {
                    2.0
                } else {
                    1.0
                };
                let left = c as f32 + f32::from(nudge) * step;
                drawn.push((left, left + cols, cell.fg));
                assert_ne!(cell.fg, rest.cells[c].fg, "{kind:?}: cell {c} is lifted");
            }
            let left = drawn.iter().map(|d| d.0).fold(f32::MAX, f32::min);
            let right = drawn.iter().map(|d| d.1).fold(f32::MIN, f32::max);
            for (c, cell) in paint.cells.iter().enumerate() {
                let middle = c as f32 + 0.5;
                let under = (left..right).contains(&middle);
                assert_eq!(
                    cell.underline == UnderlineStyle::Single,
                    under,
                    "{kind:?}: cell {c} ({:?}), the ink {left}..{right}",
                    cell.ch
                );
                if under {
                    let nearest = drawn
                        .iter()
                        .min_by(|a, b| {
                            let d = |g: &(f32, f32, [u8; 3])| ((g.0 + g.1) / 2.0 - middle).abs();
                            d(a).total_cmp(&d(b))
                        })
                        .map(|g| g.2);
                    assert_eq!(cell.underline_color, nearest, "{kind:?}: cell {c}'s ink");
                }
                if !own.contains(&c) {
                    assert_eq!(
                        (cell.ch, cell.fg, cell.script),
                        (rest.cells[c].ch, rest.cells[c].fg, rest.cells[c].script),
                        "{kind:?}: cell {c} is as at rest"
                    );
                }
            }
            if kind == FactKind::Path {
                assert!(
                    left - own.start as f32 > 1.5 && own.end as f32 - right > 1.5,
                    "NEGATIVE CONTROL: the path's own cells {own:?} are not its ink {left}..{right}"
                );
            }
        }
    }

    #[test]
    fn the_rule_names_the_account_between_the_model_and_the_path() {
        let blank = cell(' ', [200, 200, 200]);
        let rule = cell('\u{2500}', [60, 60, 60]);
        let listed = FooterFacts {
            account: Some("work-2".into()),
            ..facts()
        };
        let paint = |facts: &FooterFacts, room: usize| {
            let fit = footer::fit_rule(facts, room, None);
            compose_rule(&fit, None, rule, blank, [0, 128, 255], room).0
        };
        let plain = " \u{25C6} Opus 5.5 xhigh   \u{2302} ~/aterm   \u{2387} main ";
        let named =
            " \u{25C6} Opus 5.5 xhigh   \u{2386} work-2   \u{2302} ~/aterm   \u{2387} main ";
        let cells = paint(&listed, 76);
        assert_eq!(text_of(&cells), named);
        let at = cells
            .iter()
            .position(|c| c.ch == footer::ACCOUNT_MARK)
            .expect("the account's mark");
        assert_eq!(
            unplaced(cells[at]),
            unplaced(RenderCell {
                ch: footer::ACCOUNT_MARK,
                ..cells[1]
            }),
            "the mark takes the marks' ink"
        );
        let w = at
            + cells[at..]
                .iter()
                .position(|c| c.ch == 'w')
                .expect("the label");
        let o = cells.iter().position(|c| c.ch == 'O').expect("the model");
        assert_eq!(
            unplaced(cells[w]),
            unplaced(RenderCell {
                ch: 'w',
                ..cells[o]
            }),
            "the label takes the values' ink"
        );
        assert_eq!(text_of(&paint(&facts(), 76)), plain);

        let (mut app, wid, session) = glass(80);
        let rows = claude_rows("footer-bypass-idle", 80);
        let bottom = aterm_phase::phase::composer_bottom(&rows).expect("the composer");
        feed_rows(&app, session, &rows);
        let shown = |app: &mut App, facts: FooterFacts| {
            publish(app, session, facts);
            frame(app, wid);
            text_of(&app.windows[&wid].input_scratch.cells[strip(app, wid) + bottom])
        };
        let glass = shown(&mut app, listed);
        assert!(
            glass.ends_with(&format!("\u{2500}{named}\u{2500}")),
            "{glass:?}"
        );
        let glass = shown(&mut app, facts());
        assert!(
            glass.ends_with(&format!("\u{2500}{plain}\u{2500}")),
            "{glass:?}"
        );
    }

    #[test]
    fn wide_names_take_two_cells() {
        let wide = FooterFacts {
            branch: Some("\u{6F22}\u{5B57}".into()),
            ..FooterFacts::default()
        };
        let blank = cell(' ', [1, 1, 1]);
        let fit = footer::fit_rule(&wide, 20, None);
        let (row, _) = compose_rule(
            &fit,
            None,
            cell('\u{2500}', [2, 2, 2]),
            blank,
            [3, 3, 3],
            20,
        );
        let lead = row.iter().position(|c| c.ch == '\u{6F22}').unwrap();
        assert!(
            !row[lead].wide && row[lead + 1].wide,
            "lead, then its right half"
        );
        assert_eq!(
            row[lead + 2..].iter().find(|c| c.ch != ' ').map(|c| c.ch),
            Some('\u{5B57}')
        );
        assert_eq!(row.len(), footer::segments_width(&fit.segments) + 2);
        assert_eq!(cells_for(" \u{2387} \u{6F22}\u{5B57} "), 7);
        assert!(!footer::fit_rule(&wide, 7, None).segments.is_empty());
        assert!(footer::fit_rule(&wide, 6, None).segments.is_empty());
    }

    fn facts_with_usage() -> FooterFacts {
        use aterm_agent::harness::session_usage::{ModelTokens, UsageFacts, Wall};
        FooterFacts {
            usage: UsageFacts::of(
                (
                    vec![ModelTokens {
                        label: "opus".into(),
                        input: 48_000_000,
                        output: 310_000,
                    }],
                    aterm_agent::harness::session_usage::More::default(),
                ),
                Some(Wall {
                    label: "5h",
                    resets: "3pm".into(),
                }),
                None,
            ),
            ..facts()
        }
    }

    #[test]
    fn the_wall_and_usage_marks_take_the_marks_ink() {
        let blank = cell(' ', [200, 200, 200]);
        let rule = cell('\u{2500}', [60, 60, 60]);
        let facts = facts_with_usage();
        let room = 200;
        let fit = footer::fit_rule(&facts, room, None);
        let cells = compose_rule(&fit, None, rule, blank, [0, 128, 255], room).0;
        let ink = |mark: char| unplaced(cells[cells.iter().position(|c| c.ch == mark).unwrap()]);
        for mark in ['\u{29D7}', footer::RESET_MARK, '\u{03A3}'] {
            assert_eq!(
                ink(mark),
                RenderCell {
                    ch: mark,
                    ..ink('\u{25C6}')
                }
            );
        }
    }

    #[test]
    fn the_offset_cache_keeps_answers_and_asks_again_after_a_failure() {
        let mut answers = Vec::new();
        let t0 = std::time::Instant::now();
        let asked = std::cell::Cell::new(0);
        let mut ask = |at: std::time::Instant, answer: Option<i64>| {
            cached_offset(&mut answers, Some("UTC"), 100, at, |_, _| {
                asked.set(asked.get() + 1);
                answer
            })
        };
        assert_eq!(ask(t0, None), None, "the read failed");
        assert_eq!(ask(t0, Some(0)), Some(0), "asked again, not the failure");
        assert_eq!(asked.get(), 2);
        assert_eq!(ask(t0 + Duration::from_secs(60), Some(7)), Some(0), "kept");
        assert_eq!(
            asked.get(),
            2,
            "an answer is not read again within the hour"
        );
        assert_eq!(
            ask(t0 + Duration::from_secs(3600), Some(7)),
            Some(7),
            "an hour on, read afresh"
        );
    }

    #[test]
    fn a_walled_live_session_stays_watched_on_the_slow_clock() {
        let asked = std::time::Instant::now();
        let within = asked + WATCH_FOR - Duration::from_secs(1);
        let past = asked + WATCH_FOR + Duration::from_secs(1);
        assert_eq!(recheck_after(within, asked), IDLE_RECHECK);
        assert_eq!(recheck_after(past, asked), WALL_RECHECK);
        assert!(still_watched(within, asked, || panic!("not asked within")));
        assert!(still_watched(past, asked, || true));
        assert!(!still_watched(past, asked, || false));
        let mut walled = HashSet::new();
        note_wall(&mut walled, 7, true);
        assert!(walled.contains(&7));
        note_wall(&mut walled, 7, false);
        assert!(walled.is_empty());
        let timeline = Arc::new(Mutex::new(SessionTimeline::default()));
        let mut schedule = WatchSchedule::default();
        schedule.watch(
            Job {
                session: 7,
                timeline,
                pgid: 42,
                wake: LogWake::default(),
            },
            asked,
            true,
        );
        let _ = schedule.due_followups(past);
        schedule.read(7, past, true);
        schedule.prune(past, |session, job| session == 7 && job.pgid == 42);
        assert_eq!(schedule.next(past), Some(past + WALL_RECHECK));
        assert!(schedule.due_idle(past + IDLE_RECHECK).is_none());
        assert!(schedule.due_idle(past + WALL_RECHECK).is_some());
        schedule.prune(past, |_, _| false);
        assert!(schedule.watched.is_empty(), "no wall: the watch ends");
    }

    #[test]
    fn forgetting_a_session_drops_its_fold_and_watch() {
        let timeline = Arc::new(Mutex::new(SessionTimeline::default()));
        let job = |session| Job {
            session,
            timeline: Arc::clone(&timeline),
            pgid: 1,
            wake: LogWake::default(),
        };
        let now = std::time::Instant::now();
        let mut schedule = WatchSchedule::default();
        let mut known: HashMap<u64, Identity> = HashMap::new();
        let mut folds: HashMap<u64, footer::FooterCache> = HashMap::new();
        let mut walled: HashSet<u64> = HashSet::new();
        for session in [1, 2] {
            schedule.watch(job(session), now, true);
            known.insert(
                session,
                Identity {
                    pid: 1,
                    started: None,
                    dir: None,
                    launch: LaunchFacts::default(),
                    launch_for: None,
                    login: None,
                    argv: Vec::new(),
                    task: None,
                    login_checked_for: None,
                    stored_login: false,
                    unoffered: None,
                    env_stamp: None,
                },
            );
            folds.insert(session, footer::FooterCache::default());
            walled.insert(session);
        }
        let Accepted {
            latest: immediate,
            stopped,
            ..
        } = schedule.accept([Command::Stop {
            session: 1,
            pgid: None,
        }]);
        assert!(immediate.is_empty());
        assert_eq!(stopped, [1]);
        for session in stopped {
            forget(session, &mut known, &mut folds, &mut walled);
        }
        let due = schedule.due_followups(now + FOLLOW_UPS[FOLLOW_UPS.len() - 1]);
        assert_eq!(
            due.len(),
            FOLLOW_UPS.len(),
            "the other session's follow-ups stay owed"
        );
        assert!(
            due.iter().all(|job| job.session == 2),
            "no follow-up owed for the retired session"
        );
        for held in [
            known.contains_key(&1),
            folds.contains_key(&1),
            walled.contains(&1),
            schedule.watched.contains_key(&1),
        ] {
            assert!(!held);
        }
        assert!(known.contains_key(&2) && folds.contains_key(&2) && walled.contains(&2));
        assert!(schedule.watched.contains_key(&2));
    }

    #[test]
    fn a_group_leaving_the_foreground_releases_its_fold() {
        let timeline = Arc::new(Mutex::new(SessionTimeline::default()));
        let now = std::time::Instant::now();
        let mut schedule = WatchSchedule::default();
        let mut known: HashMap<u64, Identity> = HashMap::new();
        let mut folds: HashMap<u64, footer::FooterCache> = HashMap::new();
        let mut walled: HashSet<u64> = HashSet::new();
        schedule.watch(
            Job {
                session: 1,
                timeline: Arc::clone(&timeline),
                pgid: 40,
                wake: LogWake::default(),
            },
            now,
            true,
        );
        folds.insert(1, footer::FooterCache::default());
        walled.insert(1);
        let mut stop = |pgid| {
            let Accepted {
                latest: immediate,
                stopped,
                ..
            } = schedule.accept([Command::Stop {
                session: 1,
                pgid: Some(pgid),
            }]);
            assert!(immediate.is_empty());
            for session in &stopped {
                forget(*session, &mut known, &mut folds, &mut walled);
            }
            (stopped, folds.contains_key(&1), walled.contains(&1))
        };
        assert_eq!(stop(39), (vec![], true, true), "an older group's stop");
        assert_eq!(
            stop(40),
            (vec![1], false, false),
            "the watched group's stop"
        );
    }

    fn two_accounts() -> accounts::Roster {
        let row = |label: &str, dir: Option<&str>| accounts::Row {
            label: label.into(),
            class: accounts::Class::Dedicated,
            dir: dir.map(str::to_owned),
            ..accounts::Row::default()
        };
        accounts::Roster {
            rows: vec![row("main", None), row("work-2", Some("/logins/work-2"))],
        }
    }

    fn args_of(env: &[&str]) -> atpkg::caller_shell::ProcArgs {
        atpkg::caller_shell::ProcArgs {
            argv: vec!["claude".into()],
            env: env.iter().map(|kv| (*kv).to_owned()).collect(),
            ..Default::default()
        }
    }

    fn login_under(dir: &str) -> Option<accounts::ProcessLogin> {
        let login = format!("{}={dir}", accounts::LOGIN_ENV);
        login_of_args(Some(&args_of(&["HOME=/home/someone", &login])))
    }

    use super::rig::{claude_home, scratch_home};

    #[test]
    fn an_environment_that_was_not_read_names_no_login() {
        assert_eq!(login_of_args(None), None, "no read");
        assert_eq!(login_of_args(Some(&args_of(&[]))), None, "reads empty");
        let roster = two_accounts();
        let default = login_of_args(Some(&args_of(&["HOME=/home/someone"]))).expect("read");
        assert_eq!(accounts::footer_label(&roster, &default), Some("main"));
        let named = login_under("/logins/work-2").expect("read");
        assert_eq!(accounts::footer_label(&roster, &named), Some("work-2"));
        let own_home = login_of_args(Some(&args_of(&[
            "HOME=/home/someone",
            "CLAUDE_CONFIG_DIR=/elsewhere",
        ])))
        .expect("read");
        assert_eq!(accounts::footer_label(&roster, &own_home), None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn unread_exec_directory_is_retried_for_the_same_process() {
        fn recovered_args(_: i32) -> Option<atpkg::caller_shell::ProcArgs> {
            let home = super::rig::home_root("footer-exec-context");
            Some(atpkg::caller_shell::ProcArgs {
                argv: vec!["claude".to_string()],
                env: vec![
                    format!("HOME={}", home.display()),
                    format!("CLAUDE_CONFIG_DIR={}", home.join("claude").display()),
                ],
                ..Default::default()
            })
        }

        let pid = i32::try_from(std::process::id()).unwrap();
        let started = birth_seconds(pid).expect("this test process's kernel start");
        assert_eq!(claude_dir_of_args(None, pid), None);
        let (root, claude) = claude_home("footer-exec-context", pid, started);
        let profile =
            aterm_agent::harness::profile::path_in(&root, Some(&claude.to_string_lossy()), &claude);
        std::fs::write(
            profile,
            r#"{"oauthAccount":{"accountUuid":"recovered-account","emailAddress":"recovered@example.test","planDisplayName":"Pro"}}"#,
        )
        .unwrap();
        let timeline = Arc::new(Mutex::new(SessionTimeline::default()));
        {
            let mut held = timeline.lock().unwrap();
            held.note_foreground_group(pid);
            held.set_program(pid, Some("claude".to_string()));
        }
        let job = Job {
            session: 17,
            timeline: Arc::clone(&timeline),
            pgid: pid,
            wake: LogWake::new(false),
        };
        let asks = Asks {
            files: |_| None,
            tap: NO_TAP,
            args: |_| None,
            roster: || None,
            home: scratch_home,
            stored_login: |_, _, _| true,
        };
        let mut known = HashMap::new();
        let mut tails = ProcessTails::default();
        let mut fold = footer::FooterCache::default();
        let mut roster = RosterMemo::default();
        let first = resolve_and_post(&mut known, &mut tails, &mut fold, &mut roster, &job, asks);
        assert!(!first.row);
        assert!(
            known.is_empty(),
            "an unread exec context must not pin a guessed directory"
        );

        let recovered = resolve_and_post(
            &mut known,
            &mut tails,
            &mut fold,
            &mut roster,
            &job,
            Asks {
                files: |_| None,
                args: recovered_args,
                ..asks
            },
        );
        assert!(
            recovered.row,
            "the next request finds the custom directory's registry"
        );
        let identity = known.get(&job.session).unwrap();
        assert_eq!(
            identity.started,
            Some(started),
            "the process did not restart"
        );
        assert_eq!(identity.dir.as_deref(), Some(claude.as_path()));
        assert_eq!(
            timeline
                .lock()
                .unwrap()
                .claude_footer()
                .unwrap()
                .subscription
                .as_ref()
                .unwrap()
                .email
                .as_deref(),
            Some("recovered@example.test")
        );
        assert_eq!(
            claude_dir_of_args(Some(&args_of(&["HOME=/home/someone"])), pid),
            Some(PathBuf::from("/home/someone/.claude")),
            "a readable HOME still supplies the default Claude directory"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn readable_launcher_directory_is_provisional_until_the_registered_image_is_read() {
        thread_local! {
            static EXEC_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
            static AUTH_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
        }
        fn args(registered: bool) -> Option<atpkg::caller_shell::ProcArgs> {
            EXEC_READS.set(EXEC_READS.get() + 1);
            let root = super::rig::home_root("footer-readable-launcher");
            let mut env = vec![format!(
                "HOME={}",
                root.join(if registered {
                    "selected-home"
                } else {
                    "launcher-home"
                })
                .display()
            )];
            if registered {
                env.push(format!(
                    "CLAUDE_CONFIG_DIR={}",
                    root.join("claude").display()
                ));
            }
            Some(atpkg::caller_shell::ProcArgs {
                argv: vec![if registered { "claude" } else { "launcher" }.into()],
                env,
                ..Default::default()
            })
        }

        let pid = i32::try_from(std::process::id()).unwrap();
        let started = birth_seconds(pid).expect("this test process's kernel start");
        let (root, claude) = claude_home("footer-readable-launcher", pid, started);
        let launcher_dir = root.join("launcher-home/.claude");
        std::fs::create_dir_all(&launcher_dir).unwrap();
        let now = aterm_agent::supervise::limit::unix_now();
        let profile = aterm_json::json!({
            "oauthAccount": {
                "accountUuid": "selected-account",
                "emailAddress": "selected@example.test",
                "planDisplayName": "Pro"
            },
            "cachedUsageUtilization": {
                "fetchedAtMs": now * 1000,
                "accountUuid": "selected-account",
                "utilization": {
                    "seven_day": { "utilization": 71, "resets_at": "2099-01-01T00:00:00Z" }
                }
            }
        });
        std::fs::write(
            claude.join(".claude.json"),
            aterm_json::to_string(&profile).unwrap(),
        )
        .unwrap();
        let timeline = Arc::new(Mutex::new(SessionTimeline::default()));
        {
            let mut held = timeline.lock().unwrap();
            held.note_foreground_group(pid);
            held.set_program(pid, Some("claude".into()));
        }
        let job = Job {
            session: 18,
            timeline: Arc::clone(&timeline),
            pgid: pid,
            wake: LogWake::new(false),
        };
        let asks = Asks {
            files: |_| None,
            tap: NO_TAP,
            args: |_| args(false),
            roster: || None,
            home: scratch_home,
            stored_login: |_, _, _| {
                AUTH_READS.set(AUTH_READS.get() + 1);
                true
            },
        };
        let mut known = HashMap::new();
        let mut tails = ProcessTails::default();
        let mut fold = footer::FooterCache::default();
        let mut roster = RosterMemo::default();
        let first = resolve_and_post(&mut known, &mut tails, &mut fold, &mut roster, &job, asks);
        assert!(!first.row);
        let provisional = known.get(&job.session).unwrap();
        assert_eq!(provisional.dir.as_deref(), Some(launcher_dir.as_path()));
        assert_eq!(provisional.launch_for, None);

        let selected = Asks {
            files: |_| None,
            args: |_| args(true),
            ..asks
        };
        let recovered = resolve_and_post(
            &mut known,
            &mut tails,
            &mut fold,
            &mut roster,
            &job,
            selected,
        );
        assert!(
            recovered.row,
            "the same process now names its custom directory"
        );
        let confirmed = known.get(&job.session).unwrap().clone();
        assert_eq!(confirmed.started, Some(started));
        assert_eq!(confirmed.dir.as_deref(), Some(claude.as_path()));
        assert_eq!(confirmed.launch_for, Some(Some(started)));
        let display = timeline
            .lock()
            .unwrap()
            .claude_footer()
            .unwrap()
            .subscription
            .clone()
            .unwrap();
        assert_eq!(display.email.as_deref(), Some("selected@example.test"));
        assert_eq!(display.weekly.as_deref(), Some("weekly 29% left"));
        assert_eq!(AUTH_READS.get(), 1);

        for registry_present in [true, false] {
            if !registry_present {
                std::fs::remove_file(claude.join(format!("sessions/{pid}.json"))).unwrap();
            }
            let before = EXEC_READS.get();
            let found = resolve_and_post(
                &mut known,
                &mut tails,
                &mut fold,
                &mut roster,
                &job,
                selected,
            );
            assert_eq!(found.row, registry_present);
            assert_eq!(EXEC_READS.get(), before + 1);
            assert_eq!(AUTH_READS.get(), 1);
            assert_eq!(known.get(&job.session), Some(&confirmed));
            assert_eq!(
                timeline
                    .lock()
                    .unwrap()
                    .claude_footer()
                    .unwrap()
                    .subscription
                    .as_ref(),
                Some(&display)
            );
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn the_roster_is_read_at_most_every_recheck() {
        let t0 = std::time::Instant::now();
        let asked = std::cell::Cell::new(0);
        let mut memo = RosterMemo::default();
        let mut at = |now: std::time::Instant, answer: Option<accounts::Roster>| {
            memo.at(now, || {
                asked.set(asked.get() + 1);
                answer
            })
            .cloned()
        };
        let two = two_accounts();
        let mut three = two.clone();
        three.rows.push(accounts::Row {
            label: "pool-a".into(),
            class: accounts::Class::Shared,
            dir: Some("/logins/pool-a".into()),
            ..accounts::Row::default()
        });
        let one = accounts::Roster {
            rows: two.rows[..1].to_vec(),
        };
        assert_eq!(at(t0, Some(two.clone())), Some(two.clone()));
        let within = t0 + ROSTER_RECHECK - Duration::from_millis(1);
        assert_eq!(at(within, Some(three.clone())), Some(two.clone()), "kept");
        assert_eq!(asked.get(), 1, "not read again within the period");
        let t1 = t0 + ROSTER_RECHECK;
        assert_eq!(at(t1, Some(three.clone())), Some(three), "read afresh");
        assert_eq!(asked.get(), 2);
        let t2 = t1 + ROSTER_RECHECK;
        assert_eq!(
            at(t2, Some(one.clone())),
            Some(one.clone()),
            "one account binds identity"
        );
        assert_eq!(
            at(t2 + Duration::from_secs(1), Some(two.clone())),
            Some(one),
            "the answer is kept for the period"
        );
        assert_eq!(asked.get(), 3);
        let t3 = t2 + ROSTER_RECHECK;
        assert_eq!(at(t3, None), None, "a load that fails");
        let t4 = t3 + ROSTER_RECHECK;
        assert_eq!(
            at(t4, Some(accounts::Roster::default())),
            Some(accounts::Roster::default()),
            "none listed"
        );
        let t5 = t4 + ROSTER_RECHECK;
        assert_eq!(at(t5, Some(two.clone())), Some(two), "listed again");
        assert_eq!(asked.get(), 6);
    }

    fn on_main(_: i32) -> Option<atpkg::caller_shell::ProcArgs> {
        Some(args_of(&["HOME=/home/someone"]))
    }

    fn on_work(_: i32) -> Option<atpkg::caller_shell::ProcArgs> {
        let login = format!("{}=/logins/work-2", accounts::LOGIN_ENV);
        Some(atpkg::caller_shell::ProcArgs {
            argv: vec!["claude".into(), "--resume".into()],
            ..args_of(&["HOME=/home/someone", &login])
        })
    }

    fn unread(pid: i32, claude: &std::path::Path) -> Identity {
        Identity {
            pid,
            started: None,
            dir: Some(claude.to_path_buf()),
            launch: LaunchFacts::default(),
            launch_for: None,
            login: None,
            argv: Vec::new(),
            task: None,
            login_checked_for: None,
            stored_login: false,
            unoffered: None,
            env_stamp: None,
        }
    }

    fn account_after(
        identity: &mut Identity,
        roster: &mut RosterMemo,
        asks: Asks,
    ) -> Option<Option<String>> {
        let timeline = Arc::new(Mutex::new(SessionTimeline::default()));
        timeline.lock().unwrap().note_foreground_group(identity.pid);
        resolve_into(
            &timeline,
            identity,
            &mut footer::TailCache::default(),
            &mut footer::FooterCache::default(),
            roster,
            asks,
        );
        let timeline = timeline.lock().unwrap();
        timeline.claude_footer().map(|facts| facts.account.clone())
    }

    #[test]
    fn selected_profile_changes_replace_subscription_even_when_registry_is_unreadable() {
        use aterm_agent::harness::profile::Bucket;

        const PID: i32 = 4243;
        const START: u64 = 1_790_607_848;
        let (root, claude) = claude_home("footer-subscription-resolve", PID, START);
        let profile =
            aterm_agent::harness::profile::path_in(&root, Some(&claude.to_string_lossy()), &claude);
        let now = aterm_agent::supervise::limit::unix_now();
        let content = |email: &str, account: &str| {
            aterm_json::json!({
                "oauthAccount": {
                    "accountUuid": account,
                    "emailAddress": email,
                    "organizationType": "claude_max",
                    "planDisplayName": "Max 5x"
                },
                "cachedUsageUtilization": {
                    "fetchedAtMs": now * 1000,
                    "accountUuid": "first-account",
                    "utilization": {
                        "seven_day": { "utilization": 71, "resets_at": "2099-01-01T00:00:00Z" },
                        "seven_day_opus": { "utilization": 80, "resets_at": "2099-01-01T00:00:00Z" }
                    }
                }
            })
        };
        std::fs::write(
            &profile,
            aterm_json::to_string(&content("first@example.com", "first-account")).unwrap(),
        )
        .unwrap();
        let timeline = Arc::new(Mutex::new(SessionTimeline::default()));
        {
            let mut held = timeline.lock().unwrap();
            held.note_foreground_group(PID);
            held.set_program(PID, Some("claude".into()));
        }
        let mut identity = unread(PID, &claude);
        identity.started = Some(START);
        let mut tail = footer::TailCache::default();
        let mut fold = footer::FooterCache::default();
        let mut memo = RosterMemo::default();
        let asks = Asks {
            files: |_| None,
            tap: NO_TAP,
            args: |_| {
                let root = super::rig::home_root("footer-subscription-resolve");
                Some(args_of(&[
                    &format!("HOME={}", root.display()),
                    &format!("CLAUDE_CONFIG_DIR={}", root.join("claude").display()),
                ]))
            },
            roster: || None,
            home: scratch_home,
            stored_login: |_, _, _| true,
        };
        let mut refresh = || {
            let (_, found) = resolve_facts(
                &timeline,
                &mut identity,
                &mut tail,
                &mut fold,
                &mut memo,
                asks,
            );
            let held = timeline.lock().unwrap();
            let facts = held.claude_footer().unwrap();
            let binding = facts.plan.map(|plan| (plan.window, plan.used_x10));
            (facts.subscription.clone().unwrap(), binding, found.timed)
        };
        let (first, plan, _) = refresh();
        assert_eq!(first.email.as_deref(), Some("first@example.com"));
        assert_eq!(first.weekly.as_deref(), Some("weekly 29% left"));
        assert_eq!(
            plan,
            Some((Bucket::SevenDay, 710)),
            "no model is named, so only the account-wide windows gate it"
        );
        {
            let mut held = timeline.lock().unwrap();
            let owner = held.claude_footer().unwrap().owner.clone().unwrap();
            let card = aterm_phase::LaunchCard {
                version: "2.1.287".into(),
                model: "Opus 5.5".into(),
                effort: None,
            };
            assert!(held.note_claude_card(PID, &owner, card));
        }
        assert_eq!(
            refresh(),
            (first.clone(), Some((Bucket::SevenDayOpus, 800)), true)
        );
        std::fs::remove_file(claude.join(format!("sessions/{PID}.json"))).unwrap();
        assert_eq!(refresh(), (first, Some((Bucket::SevenDayOpus, 800)), true));
        std::fs::write(
            &profile,
            aterm_json::to_string(&content("second@example.com", "second-account")).unwrap(),
        )
        .unwrap();
        let (second, plan, timed) = refresh();
        assert_eq!(second.email.as_deref(), Some("second@example.com"));
        assert!(
            second.weekly.is_none(),
            "the previous account's quota cannot survive"
        );
        assert_eq!(plan, None, "nor can its plan, on the kept facts");
        assert!(!timed);
        let five_hour = aterm_json::json!({
            "oauthAccount": { "accountUuid": "second-account", "emailAddress": "second@example.com" },
            "cachedUsageUtilization": {
                "fetchedAtMs": now * 1000, "accountUuid": "second-account",
                "utilization": { "five_hour": { "utilization": 40, "resets_at": "2099-01-01T00:00:00Z" } }
            }
        });
        std::fs::write(&profile, aterm_json::to_string(&five_hour).unwrap()).unwrap();
        let (shown, plan, timed) = refresh();
        assert!(shown.weekly.is_none());
        assert_eq!(plan, Some((Bucket::FiveHour, 400)));
        assert!(timed, "the plan alone keeps the clock");
        std::fs::write(&profile, "{").unwrap();
        assert_eq!(
            refresh(),
            (
                aterm_agent::harness::profile::SubscriptionDisplay::default(),
                None,
                false
            )
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_muse_login_store_switch_drops_the_previous_accounts_card_facts() {
        const PID: i32 = 4246;
        let root = crate::test_scratch::process_root("footer-muse-account-switch");
        let _ = std::fs::remove_dir_all(&root);
        let home = root.join("home");
        let store = home.join(".config/muse");
        std::fs::create_dir_all(&store).unwrap();
        let auth = store.join("auth.json");
        let write_auth = |email: &str| {
            std::fs::write(
                &auth,
                format!(
                    r#"{{"providers":{{"meta":{{"mechanism":"oauth","user_email":"{email}"}}}}}}"#
                ),
            )
            .unwrap()
        };
        write_auth("first@example.test");
        let timeline = Arc::new(Mutex::new(SessionTimeline::default()));
        {
            let mut held = timeline.lock().unwrap();
            held.note_foreground_group(PID);
            held.set_program(PID, Some("muse".into()));
            assert!(held.set_claude_footer(
                PID,
                None,
                Some(FooterFacts {
                    owner: Some(footer::FactsOwner {
                        floor: None,
                        session_id: format!("muse-{PID}"),
                    }),
                    subscription: Some(aterm_agent::harness::profile::SubscriptionDisplay {
                        email: Some("first@example.test".into()),
                        plan: Some("Muse Code Max Usage".into()),
                        weekly: Some("weekly 29% left".into()),
                        ..Default::default()
                    },),
                    ..FooterFacts::default()
                }),
            ));
        }
        let mut identity = unread(PID, &root.join("claude"));
        let mut tail = footer::TailCache::default();
        let mut fold = footer::FooterCache::default();
        let mut memo = RosterMemo::default();
        let asks = Asks {
            files: |_| None,
            tap: NO_TAP,
            args: |_| {
                let root = crate::test_scratch::process_root("footer-muse-account-switch");
                Some(atpkg::caller_shell::ProcArgs {
                    argv: vec!["muse".into()],
                    env: vec![format!("HOME={}", root.join("home").display())],
                    ..Default::default()
                })
            },
            roster: || None,
            home: scratch_home,
            stored_login: |_, _, _| true,
        };
        let refresh = |identity: &mut Identity,
                       tail: &mut footer::TailCache,
                       fold: &mut footer::FooterCache,
                       memo: &mut RosterMemo,
                       now| {
            let clock = footer::ReadClock {
                read_at: std::time::UNIX_EPOCH,
                now,
                offset_at: &|_, _| Some(0),
            };
            resolve_facts_at_with_cwd(
                &timeline,
                identity,
                tail,
                fold,
                memo,
                (asks, |_| None),
                &clock,
            );
            timeline
                .lock()
                .unwrap()
                .claude_footer()
                .and_then(|facts| facts.subscription.clone())
                .unwrap()
        };
        const RESET: i64 = 1_792_947_600;
        let first = refresh(&mut identity, &mut tail, &mut fold, &mut memo, RESET - 1);
        assert_eq!(first.email.as_deref(), Some("first@example.test"));
        assert_eq!(first.plan.as_deref(), Some("Muse Code Max Usage"));
        assert_eq!(first.weekly.as_deref(), Some("weekly 29% left"));
        let clock = footer::ReadClock {
            read_at: std::time::UNIX_EPOCH,
            now: RESET - 1,
            offset_at: &|_, _| Some(0),
        };
        let reset = aterm_agent::harness::profile::WeeklyReset {
            words: "Oct 25 at 5:00 PM".into(),
            at: muse_weekly_reset_at("Oct 25 at 5:00 PM", &clock),
        };
        assert_eq!(reset.at, Some(RESET));
        {
            let mut held = timeline.lock().unwrap();
            let mut facts = held.claude_footer().cloned().expect("facts");
            let sub = facts.subscription.as_mut().expect("subscription");
            sub.weekly_reset = Some(reset.clone());
            assert!(held.set_claude_footer(PID, None, Some(facts)));
        }
        let before = refresh(&mut identity, &mut tail, &mut fold, &mut memo, RESET - 1);
        assert_eq!(before.weekly.as_deref(), Some("weekly 29% left"));
        assert_eq!(before.weekly_reset.as_ref(), Some(&reset));
        let expired = refresh(&mut identity, &mut tail, &mut fold, &mut memo, RESET);
        assert_eq!(expired.email.as_deref(), Some("first@example.test"));
        assert_eq!(expired.plan.as_deref(), Some("Muse Code Max Usage"));
        assert_eq!(
            (expired.weekly, expired.weekly_reset.as_ref()),
            (None, Some(&reset)),
            "expiry keeps the observation's occurrence, not its percentage"
        );
        let relative = aterm_agent::harness::profile::WeeklyReset {
            words: "in 1 hour".into(),
            at: muse_weekly_reset_at("in 1 hour", &clock),
        };
        let relative_at = RESET - 1 + 3600;
        assert_eq!(relative.at, Some(relative_at));
        {
            let mut held = timeline.lock().unwrap();
            let mut facts = held.claude_footer().cloned().unwrap();
            let sub = facts.subscription.as_mut().unwrap();
            sub.weekly = Some("weekly 29% left".into());
            sub.weekly_reset = Some(relative.clone());
            assert!(held.set_claude_footer(PID, None, Some(facts)));
        }
        let before = refresh(
            &mut identity,
            &mut tail,
            &mut fold,
            &mut memo,
            relative_at - 1,
        );
        assert_eq!(before.weekly.as_deref(), Some("weekly 29% left"));
        let expired = refresh(&mut identity, &mut tail, &mut fold, &mut memo, relative_at);
        assert_eq!(
            (expired.weekly, expired.weekly_reset),
            (None, Some(relative))
        );
        let unknown = aterm_agent::harness::profile::WeeklyReset {
            words: "not a reset".into(),
            at: muse_weekly_reset_at("not a reset", &clock),
        };
        assert_eq!(unknown.at, None);
        {
            let mut held = timeline.lock().unwrap();
            let mut facts = held.claude_footer().cloned().unwrap();
            let sub = facts.subscription.as_mut().unwrap();
            sub.weekly = Some("weekly 29% left".into());
            sub.weekly_reset = Some(unknown.clone());
            assert!(held.set_claude_footer(PID, None, Some(facts)));
        }
        let kept = refresh(
            &mut identity,
            &mut tail,
            &mut fold,
            &mut memo,
            relative_at + 60,
        );
        assert_eq!(kept.weekly.as_deref(), Some("weekly 29% left"));
        assert_eq!(kept.weekly_reset, Some(unknown));
        write_auth("second@example.test");
        let second = refresh(
            &mut identity,
            &mut tail,
            &mut fold,
            &mut memo,
            relative_at + 61,
        );
        assert_eq!(second.email.as_deref(), Some("second@example.test"));
        assert_eq!(
            (second.plan, second.weekly, second.weekly_reset),
            (None, None, None),
            "the previous account's tier, quota and reset cannot survive"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn subscription_requires_stored_auth_for_the_registered_image_and_directory() {
        const PID: i32 = 4245;
        const IMAGE: u64 = 1_790_607_848;
        thread_local! {
            static CHECKS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
            static ALLOWED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
        }
        fn args(_: i32) -> Option<atpkg::caller_shell::ProcArgs> {
            let root = super::rig::home_root("footer-subscription-auth");
            Some(atpkg::caller_shell::ProcArgs {
                argv: vec![
                    "claude".into(),
                    "--settings".into(),
                    r#"{"apiKeyHelper":"/opt/key.sh"}"#.into(),
                ],
                env: vec![
                    format!("HOME={}", root.display()),
                    format!("CLAUDE_CONFIG_DIR={}", root.join("claude").display()),
                ],
                ..Default::default()
            })
        }
        let (root, claude) = claude_home("footer-subscription-auth", PID, IMAGE);
        let profile =
            aterm_agent::harness::profile::path_in(&root, Some(&claude.to_string_lossy()), &claude);
        let now = aterm_agent::supervise::limit::unix_now();
        let content = aterm_json::json!({
            "oauthAccount": { "accountUuid": "auth-account", "emailAddress": "auth@example.com", "planDisplayName": "Max 5x" },
            "cachedUsageUtilization": {
                "fetchedAtMs": now * 1000, "accountUuid": "auth-account",
                "utilization": { "seven_day": { "utilization": 71, "resets_at": "2099-01-01T00:00:00Z" } }
            }
        });
        std::fs::write(&profile, aterm_json::to_string(&content).unwrap()).unwrap();
        let timeline = Arc::new(Mutex::new(SessionTimeline::default()));
        {
            let mut held = timeline.lock().unwrap();
            held.note_foreground_group(PID);
            held.set_program(PID, Some("claude".into()));
        }
        let mut identity = unread(PID, &claude);
        identity.started = Some(IMAGE);
        let mut tail = footer::TailCache::default();
        let mut fold = footer::FooterCache::default();
        let mut memo = RosterMemo::default();
        let asks = Asks {
            files: |_| None,
            tap: NO_TAP,
            args,
            roster: || None,
            home: scratch_home,
            stored_login: |pid, argv, cwd| {
                assert_eq!(pid, PID as u32);
                assert_eq!(argv[0], "claude");
                assert!(std::path::Path::new(cwd).is_absolute());
                CHECKS.set(CHECKS.get() + 1);
                ALLOWED.get()
            },
        };
        let mut refresh = |asks| {
            resolve_facts(
                &timeline,
                &mut identity,
                &mut tail,
                &mut fold,
                &mut memo,
                asks,
            );
            timeline
                .lock()
                .unwrap()
                .claude_footer()
                .unwrap()
                .subscription
                .clone()
                .unwrap()
        };
        let unavailable = aterm_agent::harness::profile::SubscriptionDisplay::default();
        assert_eq!(
            refresh(asks),
            unavailable,
            "helper/provider/refused sources suppress every field"
        );
        assert_eq!(refresh(asks), unavailable);
        assert_eq!(
            CHECKS.get(),
            1,
            "one auth-source gather per registered image"
        );
        ALLOWED.set(true);
        assert_eq!(
            refresh(asks),
            unavailable,
            "the verdict belongs to the old image"
        );

        let registry = claude.join(format!("sessions/{PID}.json"));
        let mut row: aterm_json::Value =
            aterm_json::from_slice(&std::fs::read(&registry).unwrap()).unwrap();
        row.as_object_mut()
            .unwrap()
            .insert("startedAt".into(), aterm_json::json!((IMAGE + 60) * 1000));
        std::fs::write(&registry, aterm_json::to_string(&row).unwrap()).unwrap();
        let display = refresh(asks);
        assert_eq!(display.email.as_deref(), Some("auth@example.com"));
        assert_eq!(display.plan.as_deref(), Some("Max 5x"));
        assert_eq!(display.weekly.as_deref(), Some("weekly 29% left"));
        assert_eq!(refresh(asks), display);
        assert_eq!(CHECKS.get(), 2);

        let cwd = root.join("another-work");
        std::fs::create_dir(&cwd).unwrap();
        row.as_object_mut()
            .unwrap()
            .insert("cwd".into(), aterm_json::json!(cwd));
        std::fs::write(&registry, aterm_json::to_string(&row).unwrap()).unwrap();
        assert_eq!(refresh(asks), display);
        assert_eq!(
            CHECKS.get(),
            3,
            "different project settings need a new gather"
        );
        let unregistered = Asks {
            files: |_| None,
            args: |pid| {
                let mut args = args(pid)?;
                args.argv.push("--new-image".into());
                Some(args)
            },
            ..asks
        };
        assert_eq!(
            refresh(unregistered),
            unavailable,
            "an exec cannot borrow a prior image's verdict"
        );
        assert_eq!(CHECKS.get(), 3);
        let changed_home = Asks {
            files: |_| None,
            args: |pid| {
                let mut args = args(pid)?;
                args.env[0].push_str("/another-home");
                Some(args)
            },
            ..asks
        };
        assert_eq!(
            refresh(changed_home),
            unavailable,
            "the same argv with another HOME cannot borrow the old settings verdict"
        );
        assert_eq!(CHECKS.get(), 3);
        assert_ne!(
            exec_env_stamp(&["A=B".into(), "C=D".into()]),
            exec_env_stamp(&["A=BC=D".into()]),
            "entry boundaries are part of the environment identity"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn weekly_quota_expires_on_the_local_clock_after_the_idle_watch_lapses() {
        const PID: i32 = 4244;
        const START: u64 = 1_790_607_848;
        const OBSERVED: i64 = 1_792_050_000;
        let (root, claude) = claude_home("footer-subscription-expiry", PID, START);
        let profile =
            aterm_agent::harness::profile::path_in(&root, Some(&claude.to_string_lossy()), &claude);
        let asks = Asks {
            files: |_| None,
            tap: NO_TAP,
            args: |_| {
                let root = super::rig::home_root("footer-subscription-expiry");
                Some(args_of(&[
                    &format!("HOME={}", root.display()),
                    &format!("CLAUDE_CONFIG_DIR={}", root.join("claude").display()),
                ]))
            },
            roster: || None,
            home: scratch_home,
            stored_login: |_, _, _| true,
        };
        for (reset, expires_after) in [
            ("2099-01-01T00:00:00Z", 3600),
            ("2026-10-15T07:50:50Z", 650),
        ] {
            let content = aterm_json::json!({
                "oauthAccount": {
                    "accountUuid": "expiry-account",
                    "emailAddress": "expiry@example.com",
                    "organizationType": "claude_max",
                    "planDisplayName": "Max 5x"
                },
                "cachedUsageUtilization": {
                    "fetchedAtMs": OBSERVED * 1000,
                    "accountUuid": "expiry-account",
                    "utilization": { "seven_day": { "utilization": 71, "resets_at": reset } }
                }
            });
            std::fs::write(&profile, aterm_json::to_string(&content).unwrap()).unwrap();
            let timeline = Arc::new(Mutex::new(SessionTimeline::default()));
            {
                let mut held = timeline.lock().unwrap();
                held.note_foreground_group(PID);
                held.set_program(PID, Some("claude".into()));
            }
            let mut identity = unread(PID, &claude);
            identity.started = Some(START);
            let mut tail = footer::TailCache::default();
            let mut fold = footer::FooterCache::default();
            let mut memo = RosterMemo::default();
            let mut refresh = |elapsed| {
                let (_, found) = resolve_facts_at(
                    &timeline,
                    &mut identity,
                    &mut tail,
                    &mut fold,
                    &mut memo,
                    asks,
                    &footer::ReadClock {
                        read_at: std::time::SystemTime::now(),
                        now: OBSERVED + elapsed,
                        offset_at: &|_, _| Some(0),
                    },
                );
                let display = timeline
                    .lock()
                    .unwrap()
                    .claude_footer()
                    .unwrap()
                    .subscription
                    .clone()
                    .unwrap();
                (found, display)
            };
            let (found, display) = refresh(601);
            assert_eq!(display.weekly.as_deref(), Some("weekly 29% left"));
            assert!(found.timed, "weekly without a wall still needs the clock");

            let asked = std::time::Instant::now();
            let mut schedule = WatchSchedule::default();
            schedule.watch(
                Job {
                    session: 7,
                    timeline: timeline.clone(),
                    pgid: PID,
                    wake: LogWake::default(),
                },
                asked,
                true,
            );
            let past = asked + Duration::from_secs(601);
            let _ = schedule.due_followups(past);
            schedule.read(7, past, found.row);
            schedule.prune(past, |_, _| found.timed);
            assert!(
                schedule.watched.contains_key(&7),
                "quota keeps the idle clock"
            );
            for elapsed in (661..=3661).step_by(60) {
                let at = asked + Duration::from_secs(elapsed as u64);
                assert_eq!(schedule.next(at), Some(at));
                assert!(schedule.due_idle(at).is_some());
                let (found, display) = refresh(elapsed);
                assert_eq!(display.email.as_deref(), Some("expiry@example.com"));
                assert_eq!(display.plan.as_deref(), Some("Max 5x"));
                schedule.read(7, at, found.row);
                schedule.prune(at, |_, _| found.timed);
                if elapsed > expires_after {
                    assert!(display.weekly.is_none(), "stale quota cleared: {reset}");
                    assert!(!found.timed);
                    assert!(
                        schedule.watched.is_empty(),
                        "quota expiry releases the clock"
                    );
                    assert!(elapsed <= expires_after + 60, "bounded by the slow clock");
                    break;
                }
                let aged = elapsed > 900;
                assert!(
                    display
                        .weekly
                        .as_deref()
                        .is_some_and(|words| words.ends_with(" ago") == aged),
                    "past fifteen minutes, with its age: {elapsed} {:?}",
                    display.weekly
                );
            }
            assert!(schedule.watched.is_empty());
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_read_that_answers_is_given_its_account() {
        const PID: i32 = 4242;
        let (root, claude) = claude_home("footer-account", PID, 1_790_607_848);
        let listed = |args| Asks {
            files: |_| None,
            tap: NO_TAP,
            args,
            roster: || Some(two_accounts()),
            home: scratch_home,
            stored_login: |_, _, _| true,
        };
        let unasked = |args| Asks {
            files: |_| None,
            tap: NO_TAP,
            args,
            roster: || unreachable!("no login was read: the roster is not asked"),
            home: scratch_home,
            stored_login: |_, _, _| true,
        };
        let read = |asks: Asks| {
            let mut memo = RosterMemo::default();
            let account = account_after(&mut unread(PID, &claude), &mut memo, asks);
            (account, memo.read.is_some())
        };
        assert_eq!(read(listed(on_work)), (Some(Some("work-2".into())), true));
        assert_eq!(
            read(listed(on_main)),
            (Some(Some("main".into())), true),
            "the default login is the row with no directory"
        );
        let nobody = |_| {
            let login = format!("{}=/logins/nobody", accounts::LOGIN_ENV);
            Some(args_of(&["HOME=/home/someone", &login]))
        };
        assert_eq!(read(listed(nobody)), (Some(None), true), "no row names it");
        let none = Asks {
            files: |_| None,
            tap: NO_TAP,
            args: on_main,
            roster: || None,
            home: scratch_home,
            stored_login: |_, _, _| true,
        };
        assert_eq!(read(none), (Some(None), true), "no roster: asked, none");
        assert_eq!(
            read(unasked(|_| Some(args_of(&[])))),
            (Some(None), false),
            "an environment that reads empty"
        );
        let mut identity = unread(PID, &claude);
        let mut memo = RosterMemo::default();
        assert_eq!(
            account_after(&mut identity, &mut memo, unasked(|_| None)),
            Some(None)
        );
        assert_eq!(
            identity.launch_for, None,
            "not read: nothing is kept for it"
        );
        assert_eq!(
            account_after(&mut identity, &mut memo, listed(on_main)),
            Some(Some("main".into())),
            "read at the next"
        );
        std::fs::remove_file(claude.join(format!("sessions/{PID}.json"))).unwrap();
        let nothing = Asks {
            files: |_| None,
            tap: NO_TAP,
            args: |_| unreachable!("no registry row: the process is not read"),
            roster: || unreachable!("no facts: the roster is not asked"),
            home: scratch_home,
            stored_login: |_, _, _| true,
        };
        assert_eq!(read(nothing), (None, false));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_read_publishes_the_excerpts_beside_the_facts() {
        const PID: i32 = 4243;
        let (root, claude) = claude_home("footer-excerpts", PID, 1_790_607_848);
        let project = claude
            .join("projects")
            .join(footer::project_slug(&root.join("work")));
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(
            project.join("00000000-0000-4000-8000-000000000a01.jsonl"),
            concat!(
                r#"{"type":"user","timestamp":"2026-09-28T15:00:00Z","message":{"content":"wire the survey"}}"#,
                "\n",
                r#"{"type":"assistant","timestamp":"2026-09-28T15:00:05Z","message":{"id":"m1","model":"claude-opus-5-5","content":[{"type":"text","text":"On it."}]}}"#,
                "\n",
            ),
        )
        .unwrap();
        let asks = Asks {
            files: |_| None,
            tap: NO_TAP,
            args: |_| Some(args_of(&["HOME=/home/someone"])),
            roster: || None,
            home: scratch_home,
            stored_login: |_, _, _| true,
        };
        let read = |asks: Asks| {
            let timeline = Arc::new(Mutex::new(SessionTimeline::default()));
            timeline.lock().unwrap().note_foreground_group(PID);
            resolve_into(
                &timeline,
                &mut unread(PID, &claude),
                &mut footer::TailCache::default(),
                &mut footer::FooterCache::default(),
                &mut RosterMemo::default(),
                asks,
            );
            let timeline = timeline.lock().unwrap();
            (
                timeline.claude_footer().is_some(),
                timeline.claude_excerpts().cloned(),
            )
        };
        let (facts, excerpts) = read(asks);
        assert!(facts, "the footer is published");
        let excerpts = excerpts.expect("and its excerpts");
        assert_eq!(excerpts.excerpts.prompts.len(), 1);
        assert_eq!(excerpts.excerpts.prompts[0].text, "wire the survey");
        assert_eq!(excerpts.excerpts.said.as_deref(), Some("On it."));
        assert_eq!(excerpts.task, None, "no task file named");
        std::fs::remove_file(claude.join(format!("sessions/{PID}.json"))).unwrap();
        let nothing = Asks {
            files: |_| None,
            tap: NO_TAP,
            args: |_| unreachable!("no registry row: the process is not read"),
            roster: || unreachable!("no facts: the roster is not asked"),
            home: scratch_home,
            stored_login: |_, _, _| true,
        };
        assert_eq!(read(nothing), (false, None));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_side_rides_beside_the_facts() {
        const PID: i32 = 4244;
        const IMAGE: u64 = 1_790_607_848;
        let (root, claude) = claude_home("footer-side", PID, IMAGE);
        let timeline = Arc::new(Mutex::new(SessionTimeline::default()));
        timeline.lock().unwrap().note_foreground_group(PID);
        let asks = Asks {
            files: |_| None,
            tap: NO_TAP,
            args: |_| Some(args_of(&["HOME=/home/someone"])),
            roster: || None,
            home: scratch_home,
            stored_login: |_, _, _| false,
        };
        let mut identity = unread(PID, &claude);
        let (mut tail, mut fold, mut memo) = (
            footer::TailCache::default(),
            footer::FooterCache::default(),
            RosterMemo::default(),
        );
        let mut read = || {
            let (moved, ..) = resolve_into(
                &timeline,
                &mut identity,
                &mut tail,
                &mut fold,
                &mut memo,
                asks,
            );
            let timeline = timeline.lock().unwrap();
            (
                moved.is_some(),
                Arc::clone(timeline.claude_side().expect("a side")),
                timeline.claude_footer().cloned().expect("facts"),
            )
        };
        let work = root.join("work");
        let (moved, first, facts) = read();
        assert!(moved, "the first facts");
        assert_eq!(first.cwd.as_deref(), Some(work.as_path()));
        assert_eq!(
            (first.repo_root.as_ref(), first.traffic.as_ref()),
            (None, None)
        );
        let (moved, again, same) = read();
        assert!(!moved, "nothing moved: nothing repaints");
        assert!(
            Arc::ptr_eq(&first, &again),
            "the same side, not a rebuilt one"
        );
        assert_eq!(same, facts);
        {
            let mut timeline = timeline.lock().unwrap();
            timeline.set_claude_side(PID, Some(Arc::new(FactSide::default())));
            assert_eq!(timeline.claude_footer(), Some(&facts));
            assert!(!timeline.set_claude_footer(PID, None, Some(facts.clone())));
            timeline.set_claude_side(PID, Some(Arc::clone(&first)));
        }
        std::fs::create_dir_all(work.join(".git")).unwrap();
        std::fs::write(work.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        let (_, rooted, _) = read();
        assert!(!Arc::ptr_eq(&first, &rooted));
        assert_eq!(rooted.repo_root.as_deref(), Some(work.as_path()));
        std::fs::write(
            super::rig::transcript_of(&root, &claude),
            concat!(
                r#"{"type":"assistant","timestamp":"2026-10-19T12:00:05Z","message":{"id":"m1","model":"claude-opus-5-5","usage":{"input_tokens":10,"output_tokens":7,"cache_read_input_tokens":990}}}"#,
                "\n",
            ),
        )
        .unwrap();
        let (_, graphed, _) = read();
        assert!(!Arc::ptr_eq(&rooted, &graphed));
        let traffic = graphed.traffic.as_ref().expect("its traffic");
        assert_eq!(
            (traffic.totals, &*traffic.buckets),
            ((1_000, 7), &[(1_000, 7)][..])
        );
        let (moved, held, _) = read();
        assert!(
            !moved && Arc::ptr_eq(&graphed, &held),
            "held while nothing moved"
        );
        let elsewhere = root.join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::fs::write(
            claude.join(format!("sessions/{PID}.json")),
            format!(
                r#"{{"pid":{PID},"sessionId":"{}","cwd":{},"startedAt":{},"version":"2.1.287"}}"#,
                super::rig::SESSION_ID,
                aterm_json::to_string(&elsewhere.display().to_string()).unwrap(),
                IMAGE * 1000 + 684
            ),
        )
        .unwrap();
        let (_, moved_side, _) = read();
        assert!(!Arc::ptr_eq(&held, &moved_side));
        assert_eq!(moved_side.cwd.as_deref(), Some(elsewhere.as_path()));
        let mut timeline = timeline.lock().unwrap();
        timeline.set_claude_side(PID + 1, Some(Arc::new(FactSide::default())));
        assert!(Arc::ptr_eq(
            timeline.claude_side().expect("still this group's"),
            &moved_side
        ));
        timeline.note_foreground_group(PID + 1);
        assert!(timeline.claude_side().is_none());
        drop(timeline);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_login_is_the_registered_images() {
        const PID: i32 = 4242;
        const IMAGE: u64 = 1_790_607_848;
        let (root, claude) = claude_home("footer-login", PID, IMAGE);
        let listed = |args| Asks {
            files: |_| None,
            tap: NO_TAP,
            args,
            roster: || Some(two_accounts()),
            home: scratch_home,
            stored_login: |_, _, _| true,
        };
        let mut memo = RosterMemo::default();
        let mut identity = Identity {
            login: login_of_args(on_main(PID).as_ref()),
            argv: vec!["shim".into()],
            ..unread(PID, &claude)
        };
        assert_eq!(
            account_after(&mut identity, &mut memo, listed(on_work)),
            Some(Some("work-2".into())),
            "the first row: read again"
        );
        assert_eq!(identity.launch_for, Some(Some(IMAGE)));
        assert_eq!(
            identity.argv,
            ["claude", "--resume"],
            "its command line too"
        );
        let same = listed(|_| unreachable!("the image was read: not again"));
        assert_eq!(
            account_after(&mut identity, &mut memo, same),
            Some(Some("work-2".into()))
        );
        let _ = claude_home("footer-login", PID, IMAGE + 60);
        assert_eq!(
            account_after(&mut identity, &mut memo, listed(on_main)),
            Some(Some("main".into())),
            "a new image: read again"
        );
        assert_eq!(identity.launch_for, Some(Some(IMAGE + 60)));
        assert_eq!(identity.argv, ["claude"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn only_plain_rule_glyphs_are_coverable() {
        let mut s = RenderInput::empty();
        s.rows = 1;
        s.cols = 20;
        let mut row = row_of(&"\u{2500}".repeat(20), [60, 60, 60]);
        row[3].fg = [255, 0, 0]; // a label's ink
        row[6].bg = [0, 0, 200]; // a selection
        s.cells = vec![row];
        s.clusters = vec![vec![(9, "e\u{301}".into())]];
        s.combining = vec![Vec::new()];
        s.images = vec![Vec::new()];
        let (mask, rule) = rule_mask(&s, 0, 0, 20).unwrap();
        assert_eq!(rule.fg, [60, 60, 60]);
        let mut lit = s.clone();
        for c in &mut lit.cells[0][17..] {
            c.bg = [0, 0, 200];
        }
        let (mask_lit, rule_lit) = rule_mask(&lit, 0, 0, 20).unwrap();
        assert_eq!(
            rule_lit.bg,
            [0, 0, 0],
            "the style most of the rule is drawn in"
        );
        assert_eq!(footer::rule_run(&mask_lit), Some(10..17));
        let ink: Vec<usize> = (0..20).filter(|&c| !mask[c]).collect();
        assert_eq!(ink, [3, 6, 9]);
        assert_eq!(footer::rule_run(&mask), Some(10..20));
        s.cells[0].truncate(12);
        let (mask, _) = rule_mask(&s, 0, 0, 20).unwrap();
        assert!(!mask[12] && mask[11]);
        s.cells[0] = row_of("  hello", [1, 1, 1]);
        assert!(rule_mask(&s, 0, 0, 20).is_none());
    }

    fn engine_fill(rows: usize) -> RenderInput {
        let mut s = RenderInput::empty();
        s.rows = rows;
        s.cols = 40;
        s.cells = (0..rows)
            .map(|r| row_of(&format!("engine row {r}"), [9, 9, 9]))
            .collect();
        s.clusters = (0..rows).map(|_| Vec::new()).collect();
        s.combining = (0..rows).map(|_| Vec::new()).collect();
        s.images = (0..rows).map(|_| Vec::new()).collect();
        s.snapshot_seq = 40;
        s.engine_fill_seq = 40;
        s.shifted_fill_seq = 40;
        s.terminal_id = 7;
        s.extract_gen = 3;
        s.row_rev_lane = 7;
        s.row_rev = (0..rows).map(|r| 100 + r as u64).collect();
        s
    }

    fn written(
        s: &mut RenderInput,
        edits: Vec<(usize, usize, Vec<RenderCell>, RenderCell)>,
    ) -> FooterUndo {
        let mut slot = FooterUndo::default();
        write_edits(
            &mut slot,
            s,
            edits
                .iter()
                .map(|(row, col, cells, ground)| (*row, *col, &cells[..], *ground)),
        );
        slot
    }

    #[test]
    fn a_footer_written_again_allocates_nothing() {
        let mut s = engine_fill(4);
        let original = s.cells.clone();
        let painted = row_of("  \u{25C6} Opus 5.5 xhigh", [1, 1, 1]);
        let chip = row_of("\u{221e} \u{23F8} plan", [2, 2, 2]);
        let ground = cell(' ', [0, 0, 0]);
        let edits = [(3, 0, &painted[..], ground), (2, 30, &chip[..], ground)];
        let mut slot = FooterUndo::default();
        let ((), fresh) = crate::work_counts::allocations_in(|| {
            write_edits(&mut slot, &mut s, edits);
        });
        assert!(fresh > 0, "CONTROL: a fresh slot allocates its rows");
        let first = s.cells.clone();
        for frame in 0..3 {
            assert!(undo(&mut slot, &mut s), "frame {frame}");
            assert_eq!(s.cells, original, "frame {frame}: undone exactly");
            let ((), again) = crate::work_counts::allocations_in(|| {
                write_edits(&mut slot, &mut s, edits);
            });
            assert_eq!(again, 0, "frame {frame}: the rows' buffers reused");
            assert_eq!(s.cells, first, "frame {frame}: the same write");
        }
    }

    #[test]
    fn unicode_footer_side_channels_undo_exactly_reuse_buffers_and_respect_foreign_writes() {
        let blank = cell(' ', [0, 0, 0]);
        let rule = cell('\u{2500}', [60, 60, 60]);
        let facts = FooterFacts {
            subscription: Some(aterm_agent::harness::profile::SubscriptionDisplay {
                email: Some("jose\u{0301}👩‍💻@example.com".into()),
                plan: Some("Pro".into()),
                weekly: Some("weekly 29% left".into()),
                ..Default::default()
            }),
            ..FooterFacts::default()
        };
        let fit = footer::fit_rule(&facts, 76, None);
        let paint = compose_rule_spans(&fit, None, rule, blank, [0, 128, 255], 76, None);
        assert!(!paint.clusters.is_empty() && !paint.combining.is_empty());
        let mut scratch = engine_fill(3);
        scratch.cells[2] = vec![rule; 100];
        scratch.clusters.resize_with(3, Vec::new);
        scratch.combining.resize_with(3, Vec::new);
        scratch.combining[0].push((0, vec!['\u{0300}'].into_boxed_slice()));
        let original = scratch.clone();
        let mut slot = FooterUndo::default();
        let write = |slot: &mut FooterUndo, scratch: &mut RenderInput| {
            write_edits(
                slot,
                scratch,
                rule_edits(2, 10, &paint.cells, &paint.glass, blank),
            );
            slot.publish_text(scratch, 2, 10, &paint);
        };
        write(&mut slot, &mut scratch);
        let painted = scratch.clone();
        for frame in 0..3 {
            assert!(undo(&mut slot, &mut scratch), "frame {frame}");
            assert_eq!(
                scratch, original,
                "cells, metadata and reuse identity restored"
            );
            let ((), allocations) =
                crate::work_counts::allocations_in(|| write(&mut slot, &mut scratch));
            assert_eq!(allocations, 0, "frame {frame}: metadata buffers reused");
            assert_eq!(scratch, painted);
        }
        scratch.combining[2][0].1 = vec!['\u{0300}'].into_boxed_slice();
        let changed = scratch.clone();
        assert!(
            !undo(&mut slot, &mut scratch),
            "another writer changed our mark"
        );
        assert_eq!(
            scratch, changed,
            "foreign metadata and cells are left alone"
        );

        let mut occupied = original;
        occupied.combining[2].push((
            10 + paint.combining[0].0,
            vec!['\u{0300}'].into_boxed_slice(),
        ));
        let before = occupied.clone();
        write(&mut slot, &mut occupied);
        assert_eq!(
            occupied, before,
            "a skipped vendor span gets no footer metadata"
        );
        assert!(!slot.is_written());
    }

    #[test]
    fn the_undo_gives_back_the_engine_fill_exactly() {
        let mut s = engine_fill(4);
        let original = s.cells.clone();
        let painted = row_of("  \u{25C6} Opus 5.5 xhigh", [1, 1, 1]);
        let mut slot = written(&mut s, vec![(3, 0, painted.clone(), cell(' ', [0, 0, 0]))]);
        assert!(slot.is_written());
        assert_eq!(&s.cells[3][..painted.len()], painted.as_slice());
        assert_ne!(s.snapshot_seq, s.engine_fill_seq, "the write is declared");
        assert_eq!(
            s.shifted_fill_seq, s.snapshot_seq,
            "an untouched engine fill keeps the renderer's stamp compare"
        );
        assert_eq!(s.row_rev[3], 0, "the written row is compared by content");
        assert_eq!(s.row_rev[2], 102, "every other row keeps its stamp");
        assert!(undo(&mut slot, &mut s));
        assert!(!slot.is_written(), "consumed");
        assert_eq!(s.cells, original, "cells and trimmed lengths restored");
        assert_eq!(s.snapshot_seq, 40);
        assert_eq!(s.engine_fill_seq, 40);
        assert_eq!(s.shifted_fill_seq, 40);
        assert_eq!(s.row_rev, vec![100, 101, 102, 103]);
    }

    #[test]
    fn the_program_cells_read_what_the_undo_puts_back() {
        let mut s = engine_fill(4);
        let original = s.cells.clone();
        let ground = cell(' ', [0, 0, 0]);
        let over = row_of("YOLO on", [1, 1, 1]);
        let past = row_of("\u{25C6} Opus", [2, 2, 2]);
        let slot = written(&mut s, vec![(1, 2, over, ground), (2, 20, past, ground)]);
        let text = |cells: &mut dyn Iterator<Item = &RenderCell>| -> String {
            cells.map(|c| c.ch).collect()
        };
        assert!(text(&mut s.cells[1].iter()).contains("YOLO on"), "CONTROL");
        assert!(
            text(&mut s.cells[2].iter()).ends_with("\u{25C6} Opus"),
            "CONTROL"
        );
        for (row, engine) in original.iter().enumerate() {
            for cols in [0..40, 0..5, 3..30, 25..40, 13..13] {
                let want: String = engine
                    .iter()
                    .skip(cols.start)
                    .take(cols.len())
                    .map(|c| c.ch)
                    .collect();
                assert_eq!(
                    text(&mut slot.program_cells(&s, row, cols.clone())),
                    want,
                    "row {row}, columns {cols:?}"
                );
            }
        }
        let (count, allocations) =
            crate::work_counts::allocations_in(|| slot.program_cells(&s, 2, 0..40).count());
        assert_eq!((count, allocations), (original[2].len(), 0));
        s.cells[1][3].ch = 'M';
        let presented = text(&mut s.cells[1].iter());
        assert_eq!(text(&mut slot.program_cells(&s, 1, 0..40)), presented);
    }

    #[test]
    fn a_scratch_touched_since_is_left_alone() {
        let painted = row_of("  footer", [1, 1, 1]);
        let mut s = engine_fill(3);
        let mut slot = written(&mut s, vec![(2, 0, painted.clone(), cell(' ', [0, 0, 0]))]);
        s.snapshot_seq += 1; // another host write
        let before = (s.cells.clone(), s.snapshot_seq, s.row_rev.clone());
        assert!(!undo(&mut slot, &mut s));
        assert_eq!((s.cells.clone(), s.snapshot_seq, s.row_rev.clone()), before);
        assert!(!slot.is_written(), "a stale record is dropped");

        let mut s = engine_fill(3);
        let mut slot = written(&mut s, vec![(2, 0, painted, cell(' ', [0, 0, 0]))]);
        s.cells[2][3].ch = 'X';
        assert!(
            !undo(&mut slot, &mut s),
            "the painted span must still be ours"
        );
        assert_eq!(s.cells[2][3].ch, 'X');
    }

    #[test]
    fn a_rule_with_chips_is_written_where_it_changes() {
        let rule = cell('\u{2500}', [5, 5, 5]);
        let ground = cell(' ', [0, 0, 0]);
        let fill = || {
            let mut s = engine_fill(3);
            s.cells[2] = vec![rule; 48];
            s
        };
        let mut cells = row_of(" \u{221e} \u{23F8} plan ", [2, 2, 2]);
        let glass = cells.len()..cells.len() + 24;
        cells.extend(std::iter::repeat_n(rule, glass.len()));
        cells.extend(row_of(" Opus 5.5 ", [3, 3, 3]));
        let col = 48 - 1 - cells.len();
        let split_write = |s: &mut RenderInput, glass: &std::ops::Range<usize>| {
            let mut slot = FooterUndo::default();
            let _ = crate::work_counts::take_footer_cells_written();
            write_edits(&mut slot, s, rule_edits(2, col, &cells, glass, ground));
            (slot, crate::work_counts::take_footer_cells_written())
        };
        let original = fill().cells;
        let mut whole = fill();
        let _ = crate::work_counts::take_footer_cells_written();
        let mut whole_slot = written(&mut whole, vec![(2, col, cells.clone(), ground)]);
        let whole_cost = crate::work_counts::take_footer_cells_written();
        let mut split = fill();
        let (mut slot, cost) = split_write(&mut split, &glass);
        assert_eq!(split.cells, whole.cells, "the same glass");
        assert_eq!(
            (split.snapshot_seq, &split.row_rev),
            (whole.snapshot_seq, &whole.row_rev),
            "the same tokens"
        );
        assert_eq!(cost, cells.len() - glass.len(), "the chips and the facts");
        assert_eq!(slot.rewritten_as_they_were(), 0);
        assert_eq!(whole_cost, cells.len(), "CONTROL: whole, every cell");
        assert_eq!(
            whole_slot.rewritten_as_they_were(),
            glass.len(),
            "CONTROL: whole, the middle written over itself"
        );
        assert!(undo(&mut slot, &mut split));
        assert!(undo(&mut whole_slot, &mut whole));
        assert_eq!((&split.cells, &whole.cells), (&original, &original));
        let mut bare = fill();
        let (_, cost) = split_write(&mut bare, &(0..0));
        assert_eq!(cost, cells.len());
        let (mut whole, mut split) = (fill(), fill());
        for s in [&mut whole, &mut split] {
            s.cells[2][col + glass.start + 3] = cell('x', [5, 5, 5]);
        }
        let _ = written(&mut whole, vec![(2, col, cells.clone(), ground)]);
        let _ = split_write(&mut split, &glass);
        assert_ne!(
            split.cells, whole.cells,
            "CONTROL: the skip keeps what is there"
        );
    }

    #[test]
    fn two_writes_on_one_row_come_off_in_order() {
        let mut s = engine_fill(2);
        let original = s.cells.clone();
        let left = row_of("  left footer      ", [1, 1, 1]);
        let right = row_of("  right footer", [2, 2, 2]);
        let ground = cell(' ', [0, 0, 0]);
        let mut slot = written(&mut s, vec![(1, 0, left, ground), (1, 20, right, ground)]);
        assert_eq!(s.cells[1].len(), 34, "padded to the right pane's edge");
        assert!(undo(&mut slot, &mut s));
        assert_eq!(s.cells, original);
    }

    #[test]
    fn a_span_in_the_rule_row_comes_off_exactly() {
        let mut s = engine_fill(4);
        s.cells[2] = row_of(&"\u{2500}".repeat(40), [60, 60, 60]);
        let original = s.cells.clone();
        let facts = footer::fit_rule(&facts(), 36, None);
        let blank = cell(' ', [200, 200, 200]);
        let (cells, _) = compose_rule(&facts, None, s.cells[2][0], blank, [0, 128, 255], 36);
        let col = 40 - 1 - cells.len();
        let mut slot = written(&mut s, vec![(2, col, cells.clone(), blank)]);
        assert_eq!(s.cells[2].len(), 40, "nothing padded");
        assert_eq!(&s.cells[2][col..39], cells.as_slice());
        assert_eq!(
            s.cells[2][39], original[2][39],
            "the rule's last glyph stays"
        );
        assert!(s.cells[2][..col].iter().all(|c| *c == original[2][0]));
        assert_eq!(s.row_rev[2], 0, "the rule row is compared by content");
        assert!(undo(&mut slot, &mut s));
        assert_eq!(s.cells, original);
        assert_eq!(s.row_rev, vec![100, 101, 102, 103]);
        let mut s = engine_fill(2);
        let original = s.cells.clone();
        let chip = cell('\u{23F8}', [9, 9, 9]);
        let mut slot = written(&mut s, vec![(1, 30, vec![chip], blank)]);
        assert!(
            s.cells[1][12..30].iter().all(|c| *c == blank),
            "{:?}",
            &s.cells[1][12..30]
        );
        assert!(undo(&mut slot, &mut s));
        assert_eq!(s.cells, original);
    }

    #[test]
    fn a_span_with_side_channel_content_is_not_written() {
        let mut s = engine_fill(2);
        s.clusters[1].push((4, "e\u{301}".into()));
        let before = s.cells.clone();
        let slot = written(
            &mut s,
            vec![(1, 0, row_of("  footer", [1, 1, 1]), cell(' ', [0, 0, 0]))],
        );
        assert!(!slot.is_written());
        assert_eq!(s.cells, before);
        assert_eq!(s.snapshot_seq, 40, "nothing written, nothing declared");
    }

    #[test]
    fn only_an_untouched_engine_fill_keeps_its_stamps() {
        let mut s = engine_fill(2);
        s.snapshot_seq = 41; // e.g. the find bar, earlier this frame
        let mut slot = written(
            &mut s,
            vec![(1, 0, row_of("  footer", [1, 1, 1]), cell(' ', [0, 0, 0]))],
        );
        assert_eq!(s.shifted_fill_seq, 40, "not re-armed");
        assert_eq!(s.row_rev[1], 101, "not cleared");
        assert!(undo(&mut slot, &mut s));
        assert_eq!(s.snapshot_seq, 41, "back to the other write's own bump");
    }

    #[test]
    fn a_blessed_chrome_prepend_keeps_the_stamps_and_its_inverse() {
        let mut s = engine_fill(3);
        let engine_cells = s.cells.clone();
        let blessed = s.host_prepend_blessing();
        assert!(blessed);
        s.cells.insert(0, row_of("strip", [7, 7, 7]));
        s.clusters.insert(0, Vec::new());
        s.combining.insert(0, Vec::new());
        s.images.insert(0, Vec::new());
        s.rows += 1;
        s.snapshot_seq += 1;
        s.note_host_row_prepend(1, blessed);
        let mut slot = written(
            &mut s,
            vec![(3, 0, row_of("  footer", [1, 1, 1]), cell(' ', [0, 0, 0]))],
        );
        assert_eq!(
            s.shifted_fill_seq, s.snapshot_seq,
            "the stamp compare stays on under the strip"
        );
        assert_eq!(s.row_rev[3], 0, "the written row is compared by content");
        assert!(undo(&mut slot, &mut s));
        let mut pool = Vec::new();
        assert!(
            s.undo_host_row_prepend(3, &mut pool, 1),
            "the strip's inverse still runs"
        );
        assert_eq!(s.cells, engine_cells);
        assert_eq!(s.snapshot_seq, s.engine_fill_seq);
    }

    #[test]
    fn only_an_unreadable_pill_row_is_drift() {
        let screen = |mode_row: &str| -> Vec<String> {
            let rule = "\u{2500}".repeat(60);
            vec![rule.clone(), "\u{276F} ".into(), rule, mode_row.into()]
        };
        let renamed = "  \u{23F5}\u{23F5} turbo mode on (shift+tab to cycle)";
        assert_eq!(unread_pill_row(&screen(renamed)).as_deref(), Some(renamed));
        for fine in [
            "  \u{23F5}\u{23F5} auto mode on (shift+tab to cycle)",
            "  \u{23F8} manual mode on \u{00B7} ? for shortcuts",
            "  ? for shortcuts",
            "  \u{23F5}\u{23F5} bypass permissi",
        ] {
            assert_eq!(unread_pill_row(&screen(fine)), None, "{fine:?}");
        }
        assert_eq!(unread_pill_row(&["$ ls".to_owned()]), None, "no composer");
    }

    fn claude_rows(name: &str, cols: usize) -> Vec<String> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../aterm-phase/src/fixtures")
            .join(format!("claude-2.1.283-{name}.txt"));
        let rows: Vec<String> = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
            .lines()
            .skip(1)
            .map(|row| {
                let rule = row.starts_with('\u{2500}') && row.trim_end().ends_with('\u{2500}');
                if !rule {
                    return row.to_owned();
                }
                let label: String = row.trim_start_matches('\u{2500}').to_owned();
                let n = cols - label.chars().count();
                format!("{}{label}", "\u{2500}".repeat(n))
            })
            .collect();
        assert!(rows.len() <= 24, "{name}: an 80x24 capture");
        rows
    }

    fn painted(row: &str) -> String {
        const RULE: &str = "\x1b[38;2;90;90;90m";
        const LABEL: &str = "\x1b[38;2;200;120;255m";
        const PILL: &str = "\x1b[38;2;255;110;90m";
        const DIM: &str = "\x1b[38;2;130;130;130m";
        if row.starts_with('\u{2500}') {
            let rule: String = row.chars().take_while(|c| *c == '\u{2500}').collect();
            let rest = &row[rule.len()..];
            let trail = rest
                .chars()
                .rev()
                .take_while(|c| *c == '\u{2500}')
                .map(char::len_utf8)
                .sum::<usize>();
            let (label, end) = rest.split_at(rest.len() - trail);
            return format!("{RULE}{rule}{LABEL}{label}{RULE}{end}\x1b[0m");
        }
        if footer::is_mode_row(row) {
            let at = row.find(" on").map_or(row.len(), |i| i + 3);
            return format!("{PILL}{}{DIM}{}\x1b[0m", &row[..at], &row[at..]);
        }
        row.to_owned()
    }

    fn feed_rows(app: &App, session: u64, rows: &[String]) {
        let mut out = String::from("\x1b[?2004h\x1b[2J");
        for (i, row) in rows.iter().enumerate() {
            out.push_str(&format!("\x1b[{};1H{}", i + 1, painted(row)));
        }
        let caret = rows
            .iter()
            .rposition(|r| r.starts_with(['\u{276F}', '>']))
            .unwrap_or(0);
        out.push_str(&format!("\x1b[{};3H", caret + 1));
        let term = app.pool.get(session).expect("session").term.clone();
        crate::term_lock(&term).process(out.as_bytes());
    }

    fn publish(app: &App, session: u64, facts: FooterFacts) {
        let entry = app.pool.get(session).expect("session");
        let mut timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
        timeline.note_foreground_group(4242);
        assert!(timeline.set_claude_footer(4242, None, Some(facts)));
    }

    fn frame(app: &mut App, wid: WindowId) {
        frame_with_clock(app, wid, None);
    }

    fn frame_with_clock(
        app: &mut App,
        wid: WindowId,
        clock: Option<&dyn Fn() -> footer::ReadClock<'static>>,
    ) {
        let prepared = app.prepare_terminal_capture_grid_with_cursor_fx_and_plan_outcome(
            wid,
            crate::app_render::ComposedCursorFxClock::Advance(std::time::Instant::now()),
        );
        let crate::app_render::CapturePreparation::Ready((grid, plan)) = prepared else {
            panic!("the headless capture must produce a frame");
        };
        app.splice_tab_strip(wid);
        let route = VisibleContentRoute::Terminal {
            composed: grid.composed,
        };
        match clock {
            Some(clock) => app.splice_claude_footer_at(wid, &plan, route, clock),
            None => app.splice_claude_footer(wid, &plan, route),
        }
    }

    fn glass(cols: u16) -> (App, WindowId, u64) {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let session = app.focused_session_id(wid).expect("the front session");
        if cols != 80 {
            assert!(app.apply_term_resize(wid, 24, cols));
        }
        (app, wid, session)
    }

    fn strip(app: &App, wid: WindowId) -> usize {
        app.windows[&wid].input_scratch.cells.len() - usize::from(app.windows[&wid].rows)
    }

    #[test]
    fn the_facts_go_into_the_rule_and_claudes_row_stays_whole() {
        let footer_text = " \u{25C6} Opus 5.5 xhigh   \u{2302} ~/aterm   \u{2387} main \u{2500}";
        for (name, cols, default_mode) in [
            ("footer-bypass-idle", 80, false),
            ("footer-bypass-idle", 131, false),
            ("footer-bypass-idle", 150, false),
            ("footer-busy", 80, false),
            ("footer-busy", 131, false),
            ("footer-plan", 80, false),
            ("footer-manual", 150, false),
            ("footer-accept-edits", 80, false),
            ("footer-auto-draft", 131, false),
            ("footer-bypass-idle", 80, true),
        ] {
            let label = format!(
                "{name}@{cols}{}",
                if default_mode { " (default mode)" } else { "" }
            );
            let (mut app, wid, session) = glass(cols);
            let mut rows = claude_rows(name, usize::from(cols));
            let bottom = aterm_phase::phase::composer_bottom(&rows).expect("the composer");
            if default_mode {
                rows[bottom + 1] = "  ? for shortcuts".into();
            }
            feed_rows(&app, session, &rows);
            frame(&mut app, wid);
            let strip = strip(&app, wid);
            let vendor: Vec<Vec<RenderCell>> = app.windows[&wid].input_scratch.cells.clone();
            publish(&app, session, facts());
            frame(&mut app, wid);
            let glass = &app.windows[&wid].input_scratch.cells;
            let rule = &glass[strip + bottom];
            assert!(
                text_of(rule).trim_end().ends_with(footer_text),
                "{label}: {:?}",
                text_of(rule)
            );
            let at = rule.iter().position(|c| c.ch == '\u{25C6}').unwrap();
            assert_eq!(rule.len(), usize::from(cols), "{label}");
            let set = footer_text.trim_end_matches('\u{2500}');
            assert_eq!(
                at,
                usize::from(cols) - cells_for(set),
                "{label}: right-aligned, the rule's last glyph after it"
            );
            let o = rule.iter().position(|c| c.ch == 'O').unwrap();
            let glyph = *vendor[strip + bottom].last().unwrap();
            let ink = app.windows[&wid].input_scratch.implicit_blank.fg;
            assert_eq!(
                rule[o].fg,
                mix(glyph.fg, ink, VALUE_LIFT),
                "{label}: the value, in the rule's gray lifted toward the pane's ink"
            );
            assert!(
                matches!(rule[o].script, aterm_core::terminal::Script::Sub { .. }),
                "{label}: small and low"
            );
            for (c, (was, now)) in vendor[strip + bottom].iter().zip(rule).enumerate() {
                if was != now {
                    assert_eq!(*was, glyph, "{label}: col {c} was not a plain rule glyph");
                }
            }
            assert_eq!(
                rule.last(),
                Some(&glyph),
                "{label}: the rule's last glyph stays"
            );
            for r in bottom + 1..rows.len() {
                assert_eq!(
                    glass[strip + r],
                    vendor[strip + r],
                    "{label}: row {r} under the rule is Claude's, every cell"
                );
            }
            for r in 0..bottom {
                assert_eq!(glass[strip + r], vendor[strip + r], "{label}: row {r}");
            }
        }
        let (mut app, wid, session) = glass(150);
        let rows = claude_rows("footer-bypass-idle", 150);
        let bottom = aterm_phase::phase::composer_bottom(&rows).expect("the composer");
        feed_rows(&app, session, &rows);
        publish(&app, session, facts_with_usage());
        frame(&mut app, wid);
        let rule =
            |app: &App| text_of(&app.windows[&wid].input_scratch.cells[strip(app, wid) + bottom]);
        let shown = rule(&app);
        assert!(
            shown.contains('\u{29D7}') && shown.contains('\u{03A3}'),
            "{shown:?}"
        );
        app.config.claude_footer = Some(crate::app_config::ClaudeFooterConfig {
            usage: Some(false),
            ..Default::default()
        });
        frame(&mut app, wid);
        let hidden = rule(&app);
        assert!(
            hidden.contains('\u{29D7}') && !hidden.contains('\u{03A3}'),
            "{hidden:?}"
        );
    }

    #[test]
    fn no_composer_no_footer() {
        let (mut app, wid, session) = glass(80);
        let boxed: Vec<String> = [
            "\u{25CF} done",
            "",
            &"\u{2594}".repeat(80),
            "   Change effort level?",
            "",
            "   \u{276F} 1. Yes, switch to high",
            "     2. No, go back",
        ]
        .iter()
        .map(|r| (*r).to_owned())
        .collect();
        feed_rows(&app, session, &boxed);
        publish(&app, session, facts());
        frame(&mut app, wid);
        assert!(
            !app.windows[&wid]
                .input_scratch
                .cells
                .iter()
                .any(|row| row.iter().any(|c| c.ch == '\u{25C6}')),
            "no footer anywhere"
        );
        feed_rows(&app, session, &claude_rows("footer-bypass-idle", 80));
        frame(&mut app, wid);
        assert!(
            app.windows[&wid]
                .input_scratch
                .cells
                .iter()
                .any(|row| row.iter().any(|c| c.ch == '\u{25C6}'))
        );
    }

    #[test]
    fn a_split_panes_rule_carries_its_own_footer() {
        let (mut app, wid, session) = glass(80);
        let _other = app.split_active_stub_tab(wid);
        let (pane_rows, cols) = {
            let term = crate::term_lock(&app.pool.get(session).unwrap().term);
            (usize::from(term.rows()), usize::from(term.cols()))
        };
        assert!(cols < 60, "a half-width pane: {cols}");
        let last = usize::from(app.windows[&wid].rows) - 1;
        assert!(
            pane_rows < last + 1,
            "a split pane gives a row to its title"
        );
        let rule = "\u{2500}".repeat(cols);
        let rows: Vec<String> = (0..pane_rows - 4)
            .map(|_| String::new())
            .chain([
                rule.clone(),
                "\u{276F} ".to_owned(),
                rule,
                "  \u{23F5}\u{23F5} bypass permissions on".to_owned(),
            ])
            .collect();
        feed_rows(&app, session, &rows);
        frame(&mut app, wid);
        let strip = strip(&app, wid);
        let before = app.windows[&wid].input_scratch.cells[strip + last - 1].clone();
        let mode_before = app.windows[&wid].input_scratch.cells[strip + last].clone();
        let deep = FooterFacts {
            path: Some("~/src/github.com/someone/aterm".into()),
            ..facts()
        };
        publish(&app, session, deep);
        frame(&mut app, wid);
        let row = &app.windows[&wid].input_scratch.cells[strip + last - 1];
        let text = text_of(&row[..cols]);
        assert!(
            text.ends_with(
                " \u{25C6} Opus 5.5 xhigh   \u{2302} \u{2026}/aterm   \u{2387} main \u{2500}"
            ),
            "{text:?}"
        );
        assert_eq!(&row[cols..], &before[cols..], "the sibling's cells");
        assert_eq!(
            app.windows[&wid].input_scratch.cells[strip + last],
            mode_before,
            "Claude's row"
        );
    }

    #[test]
    fn a_launch_card_fills_only_an_open_footer() {
        let (mut app, wid, session) = glass(80);
        let owner = footer::FactsOwner {
            floor: Some(1_790_607_848),
            session_id: "00000000-0000-4000-8000-000000000004".into(),
        };
        let open = FooterFacts {
            path: Some("~/aterm".into()),
            version: Some("2.1.283".into()),
            model_open: true,
            effort_open: true,
            owner: Some(owner.clone()),
            ..FooterFacts::default()
        };
        let rows = claude_rows("footer-bypass-idle", 80);
        let bottom = aterm_phase::phase::composer_bottom(&rows).unwrap();
        super::probe::drawer(&app, session, 4242, "claude", "2.1.283");
        feed_rows(&app, session, &rows);
        publish(&app, session, open.clone());
        let plan = app.active_visible_leaf_plan(wid).unwrap();
        let fp_before = app.claude_footer_fp(wid, &plan);
        frame(&mut app, wid);
        let strip = strip(&app, wid);
        let rule = |app: &App| text_of(&app.windows[&wid].input_scratch.cells[strip + bottom]);
        assert!(
            rule(&app).ends_with(" \u{25C6} Opus 5.5 xhigh   \u{2302} ~/aterm \u{2500}"),
            "{:?}",
            rule(&app)
        );
        assert_ne!(
            app.claude_footer_fp(wid, &plan),
            fp_before,
            "noting it repaints"
        );
        let mut scrolled = rows.clone();
        for r in scrolled.iter_mut().take(5) {
            r.clear();
        }
        feed_rows(&app, session, &scrolled);
        frame(&mut app, wid);
        assert!(
            rule(&app).contains("\u{25C6} Opus 5.5 xhigh"),
            "{:?}",
            rule(&app)
        );
        let entry = app.pool.get(session).unwrap();
        {
            let mut timeline = entry.ctx.timeline.lock().unwrap();
            assert!(timeline.set_claude_footer(
                4242,
                None,
                Some(FooterFacts {
                    owner: Some(footer::FactsOwner {
                        floor: Some(1_790_607_999),
                        ..owner.clone()
                    }),
                    ..open.clone()
                })
            ));
            assert!(timeline.claude_card().is_none());
        }
        frame(&mut app, wid);
        assert!(!rule(&app).contains('\u{25C6}'), "{:?}", rule(&app));
        feed_rows(&app, session, &rows);
        publish(
            &app,
            session,
            FooterFacts {
                model: Some("Fable 5.1".into()),
                model_open: false,
                ..open.clone()
            },
        );
        frame(&mut app, wid);
        assert!(
            rule(&app).contains("\u{25C6} Fable 5.1 xhigh"),
            "{:?}",
            rule(&app)
        );
        let (mut app, wid, session) = glass(80);
        super::probe::drawer(&app, session, 4242, "claude", "2.1.284");
        feed_rows(&app, session, &rows);
        publish(
            &app,
            session,
            FooterFacts {
                version: None,
                ..open
            },
        );
        frame(&mut app, wid);
        let text = text_of(&app.windows[&wid].input_scratch.cells[strip + bottom]);
        assert!(!text.contains("Opus"), "{text:?}");
        let entry = app.pool.get(session).unwrap();
        assert!(
            entry.ctx.timeline.lock().unwrap().claude_card().is_none(),
            "not kept"
        );
    }

    #[test]
    fn a_card_quoted_under_the_drawers_own_is_not_kept() {
        for quoted in ["2.1.200", "2.1.283"] {
            let (mut app, wid, session) = glass(80);
            let open = FooterFacts {
                path: Some("~/aterm".into()),
                version: Some("2.1.283".into()),
                model_open: true,
                effort_open: true,
                owner: Some(footer::FactsOwner {
                    floor: Some(1_790_607_848),
                    session_id: "00000000-0000-4000-8000-000000000006".into(),
                }),
                ..FooterFacts::default()
            };
            let rows = claude_rows("footer-bypass-idle", 80);
            let bottom = aterm_phase::phase::composer_bottom(&rows).unwrap();
            super::probe::drawer(&app, session, 4242, "claude", "2.1.283");
            feed_rows(&app, session, &rows);
            publish(&app, session, open);
            frame(&mut app, wid);
            let strip = strip(&app, wid);
            let rule = |app: &App| text_of(&app.windows[&wid].input_scratch.cells[strip + bottom]);
            assert!(
                rule(&app).contains("\u{25C6} Opus 5.5 xhigh"),
                "CONTROL: the drawer's own card: {:?}",
                rule(&app)
            );
            let mut quoting = rows.clone();
            quoting[10] = format!(
                "  \u{23BF}  \u{2590}\u{259B}\u{2588}\u{2588}\u{2588}\u{259B}\u{2588}   Claude Code \
                 v{quoted}"
            );
            quoting[11] = "     Haiku 4.5 \u{00B7} API Usage Billing".into();
            feed_rows(&app, session, &quoting);
            frame(&mut app, wid);
            let read = app.windows[&wid]
                .claude_footer_texts
                .borrow()
                .footer_card(0, session)
                .map(|(_, read)| (read.banner, read.card.model.clone()));
            assert_eq!(
                read,
                Some((Some(10), "Haiku 4.5".to_string())),
                "{quoted}: the footer's reader takes the quote"
            );
            assert!(
                rule(&app).contains("\u{25C6} Opus 5.5 xhigh"),
                "{quoted}: the quote is not kept: {:?}",
                rule(&app)
            );
            let entry = app.pool.get(session).unwrap();
            let kept = entry.ctx.timeline.lock().unwrap().claude_card().cloned();
            assert_eq!(
                kept.map(|card| (card.version, card.model)),
                Some(("2.1.283".to_string(), "Opus 5.5".to_string())),
                "{quoted}"
            );
        }
    }

    #[test]
    fn a_newer_launch_card_replaces_the_kept_one() {
        let open = FooterFacts {
            path: Some("~/aterm".into()),
            version: Some("2.1.283".into()),
            model_open: true,
            effort_open: true,
            owner: Some(footer::FactsOwner {
                floor: Some(1_790_607_848),
                session_id: "00000000-0000-4000-8000-000000000005".into(),
            }),
            ..FooterFacts::default()
        };
        let rows = claude_rows("footer-bypass-idle", 80);
        let bottom = aterm_phase::phase::composer_bottom(&rows).unwrap();
        let stale: Vec<String> = rows
            .iter()
            .map(|r| r.replace("Opus 5.5 with xhigh effort", "Haiku 4.5"))
            .collect();
        assert_ne!(stale, rows, "the fixture's card names Opus 5.5");
        let (mut app, wid, session) = glass(80);
        feed_rows(&app, session, &stale);
        super::probe::drawer(&app, session, 4242, "claude", "2.1.283");
        publish(&app, session, open.clone());
        frame(&mut app, wid);
        let strip = strip(&app, wid);
        let rule = |app: &App| text_of(&app.windows[&wid].input_scratch.cells[strip + bottom]);
        assert!(
            !rule(&app).contains('\u{25C6}'),
            "the predecessor's card is not kept: {:?}",
            rule(&app)
        );
        feed_rows(&app, session, &rows);
        frame(&mut app, wid);
        assert!(
            rule(&app).contains("\u{25C6} Opus 5.5 xhigh   \u{2302} ~/aterm"),
            "{:?}",
            rule(&app)
        );
        feed_rows(&app, session, &stale);
        frame(&mut app, wid);
        assert!(
            rule(&app).contains("\u{25C6} Haiku 4.5   \u{2302} ~/aterm"),
            "{:?}",
            rule(&app)
        );
        let mut scrolled = stale.clone();
        for r in scrolled.iter_mut().take(5) {
            r.clear();
        }
        feed_rows(&app, session, &scrolled);
        frame(&mut app, wid);
        assert!(
            rule(&app).contains("\u{25C6} Haiku 4.5"),
            "{:?}",
            rule(&app)
        );
        let (mut app, wid, session) = glass(80);
        super::probe::drawer(&app, session, 4242, "claude", "2.1.283");
        feed_rows(&app, session, &stale);
        publish(&app, session, open);
        frame(&mut app, wid);
        let rule = text_of(&app.windows[&wid].input_scratch.cells[strip + bottom]);
        assert!(
            rule.contains("\u{25C6} Haiku 4.5   \u{2302} ~/aterm"),
            "CONTROL: {rule:?}"
        );
    }

    #[test]
    fn a_move_the_person_holds_leads_the_rule_until_it_goes() {
        use aterm_agent::harness::upgrade::Phase;
        use aterm_agent::harness::upgrade_drive::Row;
        let (mut app, wid, session) = glass(150);
        let rows = claude_rows("footer-bypass-idle", 150);
        let bottom = aterm_phase::phase::composer_bottom(&rows).unwrap();
        let caret = rows
            .iter()
            .rposition(|r| r.starts_with('\u{276F}'))
            .unwrap();
        let compose = |app: &App, text: &str, dim: bool| {
            let sgr = if dim { "\x1b[2m" } else { "" };
            let row = caret + 1;
            let out = format!("\x1b[{row};1H\x1b[2K\u{276F} {sgr}{text}\x1b[0m\x1b[{row};3H");
            let term = app.pool.get(session).expect("session").term.clone();
            crate::term_lock(&term).process(out.as_bytes());
        };
        let suggestion = "Try \"refactor lib.rs\"";
        feed_rows(&app, session, &rows);
        compose(&app, suggestion, true);
        publish(&app, session, facts());
        let tab = {
            let store = app.store.read().unwrap_or_else(|p| p.into_inner());
            store
                .by_local(session)
                .expect("the front session")
                .sid
                .as_str()
                .to_string()
        };
        let now = crate::upgrade_host::now_s();
        let held = Row {
            session: "conv".into(),
            tab,
            from: "2.1.285".into(),
            to: "2.1.286".into(),
            source: "managed".into(),
            phase: Phase::Announced {
                at_s: now - 600,
                asks: 1,
            },
            behind_since: now - (5 * 3_600 + 38 * 60),
            wait: "attended".into(),
            wait_since: now - 20,
            wait_seen: now - 5,
            person_named: true,
            ready: true,
            ..Row::default()
        };
        let note = held.pause_note(now).expect("held by the person: noted");
        let using = note.using.as_ref().expect("an empty composer's words");
        let plan = app.active_visible_leaf_plan(wid).unwrap();
        let fp_before = app.claude_footer_fp(wid, &plan);
        app.apply_agent_upgrades(vec![held.clone()]);
        let fp_held = app.claude_footer_fp(wid, &plan);
        assert_ne!(fp_held, fp_before, "the note repaints");
        frame(&mut app, wid);
        let strip = strip(&app, wid);
        let rule = |app: &App| app.windows[&wid].input_scratch.cells[strip + bottom].clone();
        let facts_text = "\u{25C6} Opus 5.5 xhigh   \u{2302} ~/aterm   \u{2387} main \u{2500}";
        let led = |words: &str| format!(" \u{21E3} {words}   {facts_text}");
        let text = text_of(&rule(&app));
        assert!(text.ends_with(&led(&using.whole)), "{text:?}");
        let cells = rule(&app);
        let at = cells.iter().position(|c| c.ch == '\u{21E3}').unwrap();
        let model_mark = cells.iter().position(|c| c.ch == '\u{25C6}').unwrap();
        assert_eq!(cells[at].fg, cells[model_mark].fg, "the note's mark");
        let word = at + 1 + cells[at + 1..].iter().position(|c| c.ch != ' ').unwrap();
        assert_ne!(cells[at].fg, cells[word].fg, "a mark, not a value");
        app.apply_agent_upgrades(vec![Row {
            wait_seen: now,
            ..held.clone()
        }]);
        assert_eq!(app.claude_footer_fp(wid, &plan), fp_held, "no churn");
        compose(&app, "fix the build", false);
        frame(&mut app, wid);
        let text = text_of(&rule(&app));
        assert!(text.ends_with(&led(&note.draft.whole)), "typing: {text:?}");
        let paused = Row {
            wait: "draft".into(),
            wait_seen: 0,
            person_named: false,
            ..held.clone()
        };
        app.apply_agent_upgrades(vec![paused.clone()]);
        frame(&mut app, wid);
        let text = text_of(&rule(&app));
        assert!(text.ends_with(&led(&note.draft.whole)), "paused: {text:?}");
        for (why, text, dim) in [("cleared", "", false), ("its suggestion", suggestion, true)] {
            compose(&app, text, dim);
            frame(&mut app, wid);
            let text = text_of(&rule(&app));
            assert!(!text.contains('\u{21E3}'), "{why}: {text:?}");
            assert!(text.ends_with(&format!(" {facts_text}")), "{why}: {text:?}");
        }
        let busy = claude_rows("footer-busy", 150);
        let busy_bottom = aterm_phase::phase::composer_bottom(&busy).unwrap();
        feed_rows(&app, session, &busy);
        app.apply_agent_upgrades(vec![held.clone()]);
        frame(&mut app, wid);
        let text = text_of(&app.windows[&wid].input_scratch.cells[strip + busy_bottom]);
        assert!(!text.contains('\u{21E3}'), "a turn runs: {text:?}");
        assert!(text.contains(facts_text), "a turn runs: {text:?}");
        feed_rows(&app, session, &rows);
        compose(&app, suggestion, true);
        for (why, gone) in [
            (
                "no look has seen the person lately",
                Row {
                    person_named: false,
                    ..held.clone()
                },
            ),
            (
                "the move is under way",
                Row {
                    phase: Phase::Exiting { at_s: now },
                    ..held.clone()
                },
            ),
            (
                "its own work holds it",
                Row {
                    wait: "not-idle:busy".into(),
                    ..held.clone()
                },
            ),
        ] {
            app.apply_agent_upgrades(vec![held.clone()]);
            frame(&mut app, wid);
            assert!(text_of(&rule(&app)).contains('\u{21E3}'), "{why}: held");
            app.apply_agent_upgrades(vec![gone]);
            assert_ne!(app.claude_footer_fp(wid, &plan), fp_held, "{why}: repaints");
            frame(&mut app, wid);
            let text = text_of(&rule(&app));
            assert!(!text.contains('\u{21E3}'), "{why}: {text:?}");
            assert!(text.ends_with(&format!(" {facts_text}")), "{why}: {text:?}");
        }
    }

    #[test]
    fn no_facts_keys_like_no_footer() {
        let none = FactMask::default();
        assert_eq!(fingerprint(None, None, true, none), 0);
        assert_ne!(fingerprint(Some(&facts()), None, false, none), 0);
        let mut other = facts();
        other.branch = Some("feature".into());
        assert_ne!(
            fingerprint(Some(&facts()), None, true, none),
            fingerprint(Some(&other), None, true, none)
        );
        assert_ne!(
            fingerprint(Some(&facts()), None, true, none),
            fingerprint(
                Some(&facts()),
                None,
                true,
                none.with(FactKind::Branch, true)
            )
        );
    }

    #[test]
    fn the_account_moves_the_repaint_key() {
        let on = |label: &str| FooterFacts {
            account: Some(label.into()),
            ..facts()
        };
        for usage in [true, false] {
            let key =
                |facts: &FooterFacts| fingerprint(Some(facts), None, usage, FactMask::default());
            assert_ne!(key(&on("main")), key(&on("work-2")), "another account");
            assert_ne!(key(&on("main")), key(&facts()), "one where there was none");
            assert_eq!(key(&on("main")), key(&on("main")), "the same account");
        }
        let (app, wid, session) = glass(80);
        let plan = app.active_visible_leaf_plan(wid).unwrap();
        publish(&app, session, on("main"));
        let main = app.claude_footer_fp(wid, &plan);
        publish(&app, session, on("work-2"));
        assert_ne!(app.claude_footer_fp(wid, &plan), main);
    }

    #[test]
    fn footer_uses_the_cell_frames_viewport_and_blank_after_terminal_moves() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let plan = app.active_visible_leaf_plan(wid).unwrap();
        let front = app.front_terminal(wid).unwrap().clone();
        let (rows, cols) = {
            let ws = &app.windows[&wid];
            (usize::from(ws.rows), usize::from(ws.cols))
        };
        {
            let entry = app.pool.get(front.session).unwrap();
            let mut timeline = entry.ctx.timeline.lock().unwrap();
            timeline.note_foreground_group(42);
            assert!(timeline.set_claude_footer(42, None, Some(facts())));
        }
        {
            let mut term = crate::term_lock(&front.term);
            term.resize(rows as u16, cols as u16);
            term.set_default_foreground(aterm_core::terminal::Rgb {
                r: 10,
                g: 20,
                b: 30,
            });
            term.set_default_background(aterm_core::terminal::Rgb {
                r: 40,
                g: 50,
                b: 60,
            });
            for _ in 0..rows + 4 {
                term.process(b"history\r\n");
            }
            let ws = app.windows.get_mut(&wid).unwrap();
            term.cell_frame_into(&mut ws.input_scratch, rows, cols);
            vendor_frame(&mut ws.input_scratch);
        }
        let extracted_blank = app.windows[&wid].input_scratch.implicit_blank;
        assert_eq!(extracted_blank.fg, [10, 20, 30]);
        assert_eq!(extracted_blank.bg, [40, 50, 60]);
        assert_eq!(app.windows[&wid].input_scratch.display_offset, 0);
        {
            let mut term = crate::term_lock(&front.term);
            term.process(b"\x1b]10;rgb:aa/bb/cc\x1b\\\x1b]11;rgb:dd/ee/ff\x1b\\");
            term.scroll_display(1);
            assert!(term.grid().display_offset() > 0);
            assert_ne!(term.implicit_blank_render_cell(), extracted_blank);
        }
        app.splice_claude_footer(
            wid,
            &plan,
            VisibleContentRoute::Terminal { composed: false },
        );
        let footer = &app.windows[&wid].input_scratch.cells[rows - 2];
        let mark = footer
            .iter()
            .position(|c| c.ch == '\u{25C6}')
            .expect("the extracted live frame still paints");
        assert_eq!(footer[mark + 2].bg, extracted_blank.bg);

        let text_before = Arc::clone(
            &app.windows[&wid].claude_footer_texts.borrow().panes[0]
                .as_ref()
                .unwrap()
                .rows,
        );
        app.splice_claude_footer(
            wid,
            &plan,
            VisibleContentRoute::Terminal { composed: false },
        );
        let text_after = Arc::clone(
            &app.windows[&wid].claude_footer_texts.borrow().panes[0]
                .as_ref()
                .unwrap()
                .rows,
        );
        assert!(Arc::ptr_eq(&text_before, &text_after));
        assert_eq!(
            app.windows[&wid].input_scratch.cells[rows - 2][mark].ch,
            '\u{25C6}'
        );

        {
            let mut term = crate::term_lock(&front.term);
            let ws = app.windows.get_mut(&wid).unwrap();
            term.cell_frame_into(&mut ws.input_scratch, rows, cols);
            vendor_frame(&mut ws.input_scratch);
            assert!(ws.input_scratch.display_offset > 0);
            term.scroll_display(-1);
            assert_eq!(term.grid().display_offset(), 0);
        }
        app.splice_claude_footer(
            wid,
            &plan,
            VisibleContentRoute::Terminal { composed: false },
        );
        let row = &app.windows[&wid].input_scratch.cells[rows - 2];
        assert!(
            row.iter().all(|c| c.ch == '\u{2500}'),
            "history frame keeps the vendor's rule"
        );
    }

    #[test]
    fn footer_source_uses_leaf_index_for_splits_and_view_for_mixed_tabs() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let mut plan = app.active_visible_leaf_plan(wid).unwrap();
        let leaf = plan.leaves.remove(0);
        let mut sibling = leaf.clone();
        sibling.focused = false;
        let ws = app.windows.get_mut(&wid).unwrap();
        ws.input_scratch.terminal_id = 11;
        ws.composed_focus_scratch.terminal_id = 22;
        ws.unfocused_pane_scratch.insert(1, RenderInput::empty());
        ws.unfocused_pane_scratch.get_mut(&1).unwrap().terminal_id = 33;
        ws.leaf_render_cache
            .entry(leaf.view)
            .or_default()
            .input
            .terminal_id = 44;

        let id =
            |route: VisibleContentRoute, index: usize, leaf: &crate::tab_model::VisibleLeaf| {
                frame_source(ws, route, index, leaf).map(|s| s.terminal_id)
            };
        assert_eq!(
            id(VisibleContentRoute::Terminal { composed: false }, 0, &leaf),
            Some(11)
        );
        assert_eq!(
            id(VisibleContentRoute::Terminal { composed: true }, 0, &leaf),
            Some(22)
        );
        assert_eq!(
            id(
                VisibleContentRoute::Terminal { composed: true },
                1,
                &sibling
            ),
            Some(33)
        );
        assert_eq!(id(VisibleContentRoute::Heterogeneous, 0, &leaf), Some(44));
        assert_eq!(
            id(
                VisibleContentRoute::Terminal { composed: true },
                2,
                &sibling
            ),
            None
        );
    }

    #[test]
    fn agy_launch_card_and_composer_footer_render_on_the_glass() {
        let (mut app, wid, session) = glass(120);
        {
            let entry = app.pool.get(session).expect("session");
            let mut timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
            timeline.note_foreground_group(4242);
            timeline.set_program(4242, Some("agy".into()));
            assert!(timeline.set_claude_footer(
                4242,
                None,
                Some(FooterFacts {
                    model_open: true,
                    effort_open: true,
                    path: Some("~/aterm".into()),
                    branch: Some("main".into()),
                    owner: Some(footer::FactsOwner {
                        floor: None,
                        session_id: "agy-4242".into(),
                    }),
                    ..FooterFacts::default()
                }),
            ));
        }
        let host = crate::harness_host::HostHandle::for_supervised_test(
            Vec::new(),
            std::sync::Arc::new(|_| crate::harness_host::BodyEnd::Stopped),
        );
        app.harness = Some(host.clone());
        let ctx = app.pool.get(session).expect("session").ctx.clone();
        crate::session_timeline::claim_supervisor(
            &ctx,
            &crate::harness_host::holder(),
            None,
            None,
            crate::metrics::now_us(),
        )
        .expect("claim");

        let rule = "\u{2500}".repeat(120);
        let rows = vec![
            String::new(),
            "      \u{2584}\u{2580}\u{2580}\u{2584}        Antigravity CLI 1.4.0".into(),
            "     \u{2580}\u{2580}\u{2580}\u{2580}\u{2580}\u{2580}       redacted-user@example.test (Google AI Ultra)".into(),
            "    \u{2580}\u{2580}\u{2580}\u{2580}\u{2580}\u{2580}\u{2580}\u{2580}      Gemini 4 Argon (Medium)".into(),
            "   \u{2584}\u{2580}\u{2580}    \u{2580}\u{2580}\u{2584}     ~/aterm".into(),
            "  \u{2584}\u{2580}\u{2580}      \u{2580}\u{2580}\u{2584}".into(),
            String::new(),
            rule.clone(),
            "> ".into(),
            rule,
            "? for shortcuts                                                                                  Gemini 4 Argon \u{00B7} medium".into(),
        ];
        super::probe::drawer(&app, session, 4242, "agy", "1.4.2");
        feed_rows(&app, session, &rows);
        frame(&mut app, wid);

        let strip = strip(&app, wid);
        let card_line = text_of(&app.windows[&wid].input_scratch.cells[strip + 5]);
        assert!(
            card_line.contains("enhanced by aterm harness")
                && card_line.contains("\u{00B7} agy v0.1")
                && !card_line.contains("autopilot"),
            "agy card line on row top+4: {card_line:?}"
        );
        assert!(
            app.windows[&wid]
                .claude_card
                .painted_layers()
                .iter()
                .any(|l| l.slot == aterm_core::render::FxSlot::Under
                    && !l.content.items().is_empty()),
            "agy card rainbow border and trail drawn"
        );
        assert!(
            app.windows[&wid]
                .claude_card
                .painted_layers()
                .iter()
                .any(
                    |l| l.slot == aterm_core::render::FxSlot::Over && !l.content.items().is_empty()
                ),
            "agy card kitty and stars drawn"
        );
        let top_rule = text_of(&app.windows[&wid].input_scratch.cells[strip + 7]);
        assert!(
            top_rule.starts_with("\u{2500}\u{2500}    agy \u{2500}")
                && top_rule.contains(" alab harness v0.1 \u{2590} \u{25CF} on \u{258C} "),
            "agy top rule has its lockup and the harness switch: {top_rule:?}"
        );
        assert!(
            app.windows[&wid]
                .claude_card
                .painted_layers()
                .iter()
                .any(|l| l.id == top_mark_id(session) && !l.content.items().is_empty()),
            "agy's icon drawn over its lockup's cells"
        );
        let bottom_rule = text_of(&app.windows[&wid].input_scratch.cells[strip + 9]);
        assert!(
            bottom_rule.contains('\u{221E}'),
            "agy composer rule has the autopilot chip: {bottom_rule:?}"
        );
        assert!(
            bottom_rule
                .contains("\u{25C6} Gemini 4 Argon medium   \u{2302} ~/aterm   \u{2387} main"),
            "agy composer rule has model, effort, path, and branch: {bottom_rule:?}"
        );
        host.shutdown_and_join();
    }

    #[test]
    fn agy_follows_its_own_status_row_once_its_card_is_gone() {
        let rule = "\u{2500}".repeat(120);
        let screen = |card: bool, model: &str| -> Vec<String> {
            let mut rows = vec![String::new()];
            if card {
                rows.extend([
                    "      \u{2584}\u{2580}\u{2580}\u{2584}        Antigravity CLI 1.4.0".to_string(),
                    "     \u{2580}\u{2580}\u{2580}\u{2580}\u{2580}\u{2580}       redacted-user@example.test (Google AI Ultra)".to_string(),
                    "    \u{2580}\u{2580}\u{2580}\u{2580}\u{2580}\u{2580}\u{2580}\u{2580}      Gemini 4 Argon (Medium)".to_string(),
                    "   \u{2584}\u{2580}\u{2580}    \u{2580}\u{2580}\u{2584}     ~/aterm".to_string(),
                    "  \u{2584}\u{2580}\u{2580}      \u{2580}\u{2580}\u{2584}".to_string(),
                ]);
            } else {
                rows.extend([
                    "\u{25CF} Switched the model.".to_string(),
                    String::new(),
                    String::new(),
                    String::new(),
                    String::new(),
                ]);
            }
            rows.extend([
                String::new(),
                rule.clone(),
                "> ".into(),
                rule.clone(),
                format!("? for shortcuts{}{model}", " ".repeat(60)),
            ]);
            rows
        };
        let agy_facts = |app: &App, session: u64| {
            let entry = app.pool.get(session).expect("session");
            let mut timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
            timeline.note_foreground_group(4242);
            timeline.set_program(4242, Some("agy".into()));
            assert!(timeline.set_claude_footer(
                4242,
                None,
                Some(FooterFacts {
                    model_open: true,
                    effort_open: true,
                    path: Some("~/aterm".into()),
                    owner: Some(footer::FactsOwner {
                        floor: None,
                        session_id: "agy-4242".into(),
                    }),
                    ..FooterFacts::default()
                }),
            ));
        };
        let bottom = |app: &App, wid: WindowId| {
            text_of(&app.windows[&wid].input_scratch.cells[strip(app, wid) + 9])
        };
        let (mut app, wid, session) = glass(120);
        agy_facts(&app, session);
        super::probe::drawer(&app, session, 4242, "agy", "1.4.2");
        feed_rows(
            &app,
            session,
            &screen(true, "Gemini 4 Argon \u{00B7} medium"),
        );
        frame(&mut app, wid);
        assert!(
            bottom(&app, wid).contains("\u{25C6} Gemini 4 Argon medium"),
            "{:?}",
            bottom(&app, wid)
        );
        feed_rows(&app, session, &screen(false, "Gemini 4 Pro \u{00B7} high"));
        frame(&mut app, wid);
        assert!(
            bottom(&app, wid).contains("\u{25C6} Gemini 4 Pro high"),
            "its own status row: {:?}",
            bottom(&app, wid)
        );
        let (mut app, wid, session) = glass(120);
        agy_facts(&app, session);
        feed_rows(&app, session, &screen(false, "Gemini 4 Pro \u{00B7} high"));
        super::probe::drawer(&app, session, 4242, "agy", "1.4.2");
        frame(&mut app, wid);
        assert!(
            !bottom(&app, wid).contains("Gemini 4 Pro"),
            "CONTROL: not the drawer's row: {:?}",
            bottom(&app, wid)
        );
    }

    #[test]
    fn muse_launch_card_and_composer_footer_render_on_the_glass() {
        let (mut app, wid, session) = glass(120);
        {
            let entry = app.pool.get(session).expect("session");
            let mut timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
            timeline.note_foreground_group(5151);
            timeline.set_program(5151, Some("muse".into()));
            assert!(timeline.set_claude_footer(
                5151,
                None,
                Some(FooterFacts {
                    model_open: true,
                    effort_open: true,
                    path: Some("~/aterm".into()),
                    branch: Some("main".into()),
                    owner: Some(footer::FactsOwner {
                        floor: None,
                        session_id: "muse-5151".into(),
                    }),
                    ..FooterFacts::default()
                }),
            ));
        }
        let host = crate::harness_host::HostHandle::for_supervised_test(
            Vec::new(),
            std::sync::Arc::new(|_| crate::harness_host::BodyEnd::Stopped),
        );
        app.harness = Some(host.clone());
        let ctx = app.pool.get(session).expect("session").ctx.clone();
        crate::session_timeline::claim_supervisor(
            &ctx,
            &crate::harness_host::holder(),
            None,
            None,
            crate::metrics::now_us(),
        )
        .expect("claim");

        let rule = "\u{2500}".repeat(120);
        let rows = vec![
            String::new(),
            "  Muse Code 0.9.3".into(),
            String::new(),
            rule.clone(),
            "\u{276F} ".into(),
            rule,
            "  gpt-5.4 \u{00B7} high \u{00B7} ~/aterm \u{00B7} Auto-review".into(),
        ];
        super::probe::drawer(&app, session, 5151, "muse", "0.9.3");
        feed_rows(&app, session, &rows);
        frame(&mut app, wid);

        let strip = strip(&app, wid);
        let card_line = text_of(&app.windows[&wid].input_scratch.cells[strip + 2]);
        assert!(
            card_line.contains("enhanced by aterm harness")
                && card_line.contains("\u{00B7} muse v0.1 \u{00B7}")
                && card_line.contains("autopilot"),
            "muse card line on row top+1: {card_line:?}"
        );
        assert!(
            app.windows[&wid]
                .claude_card
                .painted_layers()
                .iter()
                .any(|l| l.slot == aterm_core::render::FxSlot::Under
                    && !l.content.items().is_empty()),
            "muse card rainbow border and trail drawn"
        );
        assert!(
            app.windows[&wid]
                .claude_card
                .painted_layers()
                .iter()
                .any(
                    |l| l.slot == aterm_core::render::FxSlot::Over && !l.content.items().is_empty()
                ),
            "muse card kitty and stars drawn"
        );
        let top_rule = text_of(&app.windows[&wid].input_scratch.cells[strip + 3]);
        assert!(
            top_rule.starts_with("\u{2500}\u{2500}    muse \u{2500}")
                && top_rule.contains(" alab harness \u{25CF} on "),
            "muse top rule has its lockup and the harness switch: {top_rule:?}"
        );
        assert!(
            app.windows[&wid]
                .claude_card
                .painted_layers()
                .iter()
                .any(|l| l.id == top_mark_id(session) && !l.content.items().is_empty()),
            "muse's icon drawn over its lockup's cells"
        );
        let bottom_rule = text_of(&app.windows[&wid].input_scratch.cells[strip + 5]);
        assert!(
            bottom_rule.contains('\u{221E}'),
            "muse composer rule has the autopilot chip: {bottom_rule:?}"
        );
        assert!(
            bottom_rule.contains("\u{25C6} gpt-5.4 high   \u{2302} ~/aterm   \u{2387} main"),
            "muse composer rule has model, effort, path, and branch: {bottom_rule:?}"
        );
        host.shutdown_and_join();
    }

    #[test]
    fn a_narrow_pane_keeps_muses_lockup_switch_and_facts() {
        let frames =
            aterm_phase::prompt::fixtures::reflow_frames(aterm_phase::muse::fixtures::IDLE_NARROW);
        let pane = |rows: &[String], cols: u16| {
            let (mut app, wid, session) = glass(cols);
            {
                let entry = app.pool.get(session).expect("session");
                let mut timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
                timeline.note_foreground_group(5252);
                timeline.set_program(5252, Some("muse".into()));
                assert!(timeline.set_claude_footer(
                    5252,
                    None,
                    Some(FooterFacts {
                        model_open: true,
                        effort_open: true,
                        path: Some("~/aterm".into()),
                        owner: Some(footer::FactsOwner {
                            floor: None,
                            session_id: "muse-5252".into(),
                        }),
                        ..FooterFacts::default()
                    }),
                ));
            }
            let host = crate::harness_host::HostHandle::for_supervised_test(
                Vec::new(),
                std::sync::Arc::new(|_| crate::harness_host::BodyEnd::Stopped),
            );
            app.harness = Some(host.clone());
            let ctx = app.pool.get(session).expect("session").ctx.clone();
            crate::session_timeline::claim_supervisor(
                &ctx,
                &crate::harness_host::holder(),
                None,
                None,
                crate::metrics::now_us(),
            )
            .expect("claim");
            super::probe::drawer(&app, session, 5252, "muse", "1.4.11");
            feed_rows(&app, session, rows);
            frame(&mut app, wid);
            let strip = strip(&app, wid);
            let cells = &app.windows[&wid].input_scratch.cells;
            let (top, bottom) = (text_of(&cells[strip + 3]), text_of(&cells[strip + 5]));
            let icon = app.windows[&wid]
                .claude_card
                .painted_layers()
                .iter()
                .any(|l| l.id == top_mark_id(session) && !l.content.items().is_empty());
            host.shutdown_and_join();
            (top, bottom, icon)
        };
        assert_eq!(frames.len(), 9, "the sweep's widths");
        for (cols, rows) in &frames {
            let (top, bottom, icon) = pane(rows, *cols);
            assert!(
                bottom.contains("\u{221E}") && bottom.contains("\u{25C6} lab-stub"),
                "{cols}: the chip and the facts on the bottom rule: {bottom:?}"
            );
            if *cols >= 32 {
                assert!(
                    top.starts_with("\u{2500}\u{2500}    muse \u{2500}")
                        && top.ends_with(" \u{25CF} on \u{2500}\u{2500}")
                        && !top.contains("Voice"),
                    "{cols}: the lockup over the label, the switch at the right: {top:?}"
                );
                assert!(icon, "{cols}: the icon over the lockup's cells");
            } else {
                assert_eq!(top, rows[3], "{cols}: the cut label as Muse Code drew it");
                assert!(!icon, "{cols}: no lockup over the cut label");
            }
            let mut bare = rows.clone();
            bare[3] = rows[3]
                .chars()
                .map(|c| if c == '\u{2500}' { c } else { ' ' })
                .collect();
            let (top, bottom, icon) = pane(&bare, *cols);
            assert_eq!(
                (bottom.contains("\u{221E}"), top.contains("muse") || icon),
                (*cols >= 40, false),
                "CONTROL {cols}: fewer than ten `─` and no label are no composer: {top:?} {bottom:?}"
            );
        }
    }

    #[test]
    fn muse_signed_in_cards_populate_the_footer() {
        let (mut app, wid, session) = glass(160);
        const RESET: i64 = 1_792_947_600;
        let now = std::cell::Cell::new(RESET - 1);
        let clock_reads = std::cell::Cell::new(0usize);
        let clock = || {
            clock_reads.set(clock_reads.get() + 1);
            footer::ReadClock {
                read_at: std::time::UNIX_EPOCH,
                now: now.get(),
                offset_at: &|_, _| Some(0),
            }
        };
        let frame = |app: &mut App, wid| frame_with_clock(app, wid, Some(&clock));
        {
            let entry = app.pool.get(session).expect("session");
            let mut timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
            timeline.note_foreground_group(6161);
            timeline.set_program(6161, Some("muse".into()));
            assert!(timeline.set_claude_footer(
                6161,
                None,
                Some(FooterFacts {
                    model_open: true,
                    effort_open: true,
                    path: Some("~/aterm".into()),
                    branch: Some("main".into()),
                    owner: Some(footer::FactsOwner {
                        floor: None,
                        session_id: "muse-6161".into(),
                    }),
                    subscription: Some(aterm_agent::harness::profile::SubscriptionDisplay {
                        email: Some("redacted-user@example.test".into()),
                        ..Default::default()
                    },),
                    ..FooterFacts::default()
                }),
            ));
            assert!(!timeline.muse_cards_need_account());
        }
        let host = crate::harness_host::HostHandle::for_supervised_test(
            Vec::new(),
            std::sync::Arc::new(|_| crate::harness_host::BodyEnd::Stopped),
        );
        app.harness = Some(host.clone());
        let ctx = app.pool.get(session).expect("session").ctx.clone();
        crate::session_timeline::claim_supervisor(
            &ctx,
            &crate::harness_host::holder(),
            None,
            None,
            crate::metrics::now_us(),
        )
        .expect("claim");

        let rule = "\u{2500}".repeat(160);
        let labelled = format!(
            "\u{2500}\u{2500} Voice input (\u{2325} + v to start) {}",
            "\u{2500}".repeat(128)
        );
        let footer_row = "  gpt-5.4 \u{00B7} high \u{00B7} ~/aterm \u{00B7} Auto-review";
        let bare = vec![
            String::new(),
            "  Muse Code 0.9.3".into(),
            String::new(),
            labelled.clone(),
            "\u{276F} ".into(),
            rule.clone(),
            footer_row.into(),
        ];
        feed_rows(&app, session, &bare);
        frame(&mut app, wid);
        let strip = strip(&app, wid);
        let bottom = text_of(&app.windows[&wid].input_scratch.cells[strip + 5]);
        assert!(
            bottom.contains("redacted-user@example.test") && bottom.contains("weekly unavailable"),
            "CONTROL: no card shown yet: {bottom:?}"
        );

        let mut carded = vec![
            String::new(),
            "  Muse Code 0.9.3".into(),
            "\u{250C}".to_owned() + &"\u{2500}".repeat(55) + "\u{2510}",
            "\u{2502}  Session usage                                        \u{2502}".into(),
            "\u{2502}  Subscription \u{00B7} Muse Code Max Usage                   \u{2502}".into(),
            "\u{2502}    Weekly         71% used \u{00B7} Resets Oct 25 at 5:00 PM \u{2502}".into(),
            "\u{2514}".to_owned() + &"\u{2500}".repeat(55) + "\u{2518}",
            labelled.clone(),
            "\u{276F} ".into(),
            rule.clone(),
            footer_row.into(),
        ];
        feed_rows(&app, session, &carded);
        frame(&mut app, wid);
        frame(&mut app, wid);
        let top = text_of(&app.windows[&wid].input_scratch.cells[strip + 7]);
        assert!(
            !top.contains("Voice input") && top.starts_with("\u{2500}\u{2500}    muse \u{2500}"),
            "the lockup at the top rule's left, where its idle label was: {top:?}"
        );
        let bottom = text_of(&app.windows[&wid].input_scratch.cells[strip + 9]);
        assert!(
            bottom.contains("redacted-user@example.test")
                && bottom.contains("Muse Code Max Usage")
                && bottom.contains("weekly 29% left")
                && !bottom.contains("unavailable"),
            "the usage card populates its tier and weekly beside the account: {bottom:?}"
        );
        let mut account_only = carded.clone();
        let status = vec![
            "┌".to_owned() + &"─".repeat(83) + "┐",
            format!("│{:<83}│", "  MUSE CODE 1.4.11 / first-session IDLE"),
            format!(
                "│{:<83}│",
                "  ACCOUNT        Redacted User (redacted-user@example.test)"
            ),
            "└".to_owned() + &"─".repeat(83) + "┘",
        ];
        drop(account_only.splice(7..7, status));
        let account_only_bottom = strip + account_only.len() - 2;
        feed_rows(&app, session, &account_only);
        frame(&mut app, wid);
        frame(&mut app, wid);
        let bottom = text_of(&app.windows[&wid].input_scratch.cells[account_only_bottom]);
        assert!(
            bottom.contains("redacted-user@example.test")
                && !bottom.contains("Muse Code Max Usage"),
            "a named status card cannot borrow another card's plan: {bottom:?}"
        );
        feed_rows(&app, session, &carded);
        frame(&mut app, wid);
        frame(&mut app, wid);
        let bottom = text_of(&app.windows[&wid].input_scratch.cells[strip + 9]);
        assert!(
            bottom.contains("Muse Code Max Usage") && bottom.contains("weekly 29% left"),
            "usage-only bootstrap remains available before any switch: {bottom:?}"
        );
        let already_read = clock_reads.get();
        frame(&mut app, wid);
        assert_eq!(
            clock_reads.get(),
            already_read,
            "a settled reset does not read the clock again"
        );
        now.set(RESET);
        let timeline = &app.pool.get(session).expect("session").ctx.timeline;
        let mut identity = unread(6161, std::path::Path::new("unused"));
        identity.dir = None;
        resolve_facts_at_with_cwd(
            timeline,
            &mut identity,
            &mut footer::TailCache::default(),
            &mut footer::FooterCache::default(),
            &mut RosterMemo::default(),
            (
                Asks {
                    files: |_| None,
                    tap: NO_TAP,
                    args: |_| None,
                    roster: || None,
                    home: || None,
                    stored_login: |_, _, _| false,
                },
                |_| None,
            ),
            &footer::ReadClock {
                read_at: std::time::UNIX_EPOCH,
                now: now.get(),
                offset_at: &|_, _| Some(0),
            },
        );
        {
            let held = timeline.lock().unwrap();
            let sub = held.claude_footer().unwrap().subscription.as_ref().unwrap();
            assert!(sub.weekly.is_none());
            assert_eq!(
                sub.weekly_reset.as_ref().and_then(|reset| reset.at),
                Some(RESET)
            );
        }
        frame(&mut app, wid);
        app.windows
            .get_mut(&wid)
            .unwrap()
            .claude_footer_texts
            .borrow_mut()
            .trim(0);
        frame(&mut app, wid);
        let bottom = text_of(&app.windows[&wid].input_scratch.cells[strip + 9]);
        assert!(
            bottom.contains("redacted-user@example.test")
                && bottom.contains("weekly unavailable")
                && !bottom.contains("weekly 29% left"),
            "an unchanged expired card cannot graft its quota again: {bottom:?}"
        );
        assert_eq!(
            clock_reads.get(),
            already_read,
            "expiry tombstones need no frame clock"
        );
        carded[5] = carded[5].replace("Oct 25", "Oct 26");
        feed_rows(&app, session, &carded);
        frame(&mut app, wid);
        frame(&mut app, wid);
        let bottom = text_of(&app.windows[&wid].input_scratch.cells[strip + 9]);
        assert!(
            bottom.contains("weekly 29% left"),
            "a new window is admitted: {bottom:?}"
        );
        {
            let entry = app.pool.get(session).expect("session");
            let mut timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
            let mut facts = timeline.claude_footer().cloned().expect("facts");
            facts.subscription.as_mut().expect("subscription").email =
                Some("second@example.test".into());
            assert!(timeline.set_claude_footer(6161, None, Some(facts)));
            assert!(timeline.muse_cards_need_account());
            let sub = timeline
                .claude_footer()
                .unwrap()
                .subscription
                .as_ref()
                .unwrap();
            assert_eq!(
                (&sub.plan, &sub.weekly, &sub.weekly_reset),
                (&None, &None, &None)
            );
        }
        frame(&mut app, wid);
        frame(&mut app, wid);
        let bottom = text_of(&app.windows[&wid].input_scratch.cells[strip + 9]);
        assert!(
            bottom.contains("second@example.test")
                && bottom.contains("weekly unavailable")
                && !bottom.contains("Muse Code Max Usage")
                && !bottom.contains("weekly 29% left"),
            "anonymous old usage cannot be attributed to the new login: {bottom:?}"
        );
        app.windows
            .get_mut(&wid)
            .expect("window")
            .claude_footer_texts
            .borrow_mut()
            .trim(0);
        let mut rebuilt = carded.clone();
        rebuilt[10] = "  gpt-5.4 · high · ~/other · Auto-review".into();
        feed_rows(&app, session, &rebuilt);
        frame(&mut app, wid);
        frame(&mut app, wid);
        let bottom = text_of(&app.windows[&wid].input_scratch.cells[strip + 9]);
        assert!(
            bottom.contains("second@example.test")
                && bottom.contains("weekly unavailable")
                && !bottom.contains("Muse Code Max Usage")
                && !bottom.contains("weekly 29% left"),
            "a new pane/text generation cannot reattribute the old card: {bottom:?}"
        );
        let stale = vec![
            String::new(),
            "  Muse Code 0.9.3".into(),
            "\u{250C}".to_owned() + &"\u{2500}".repeat(83) + "\u{2510}",
            "\u{2502}  MUSE CODE 1.4.11 / redacted-session                                            IDLE \u{2502}".into(),
            "\u{2502}  ACCOUNT        Redacted User (redacted-user@example.test)                          \u{2502}".into(),
            "\u{2502}  BILLING        Subscription \u{00B7} Muse Code Max Usage                                  \u{2502}".into(),
            "\u{2514}".to_owned() + &"\u{2500}".repeat(83) + "\u{2518}",
            "\u{250C}".to_owned() + &"\u{2500}".repeat(55) + "\u{2510}",
            "\u{2502}  Session usage                                        \u{2502}".into(),
            "\u{2502}  Subscription \u{00B7} Muse Code Max Usage                   \u{2502}".into(),
            "\u{2502}    Weekly         71% used \u{00B7} Resets Oct 25 at 5:00 PM \u{2502}".into(),
            "\u{2514}".to_owned() + &"\u{2500}".repeat(55) + "\u{2518}",
            labelled,
            "\u{276F} ".into(),
            rule,
            footer_row.into(),
        ];
        let stale_bottom_row = strip + stale.len() - 2;
        feed_rows(&app, session, &stale);
        frame(&mut app, wid);
        frame(&mut app, wid);
        let bottom = text_of(&app.windows[&wid].input_scratch.cells[stale_bottom_row]);
        assert!(
            bottom.contains("second@example.test")
                && bottom.contains("weekly unavailable")
                && !bottom.contains("redacted-user@example.test")
                && !bottom.contains("weekly 29% left"),
            "the previous account's cards stay historical after a switch: {bottom:?}"
        );
        let mut current = stale.clone();
        current[4] = format!(
            "│{:<83}│",
            "  ACCOUNT        Second User (second@example.test)"
        );
        current[5] = format!("│{:<83}│", "  BILLING        Subscription · Muse Code Pro");
        feed_rows(&app, session, &current);
        frame(&mut app, wid);
        frame(&mut app, wid);
        let bottom = text_of(&app.windows[&wid].input_scratch.cells[stale_bottom_row]);
        assert!(
            bottom.contains("second@example.test")
                && bottom.contains("Muse Code Pro")
                && bottom.contains("weekly unavailable")
                && !bottom.contains("Muse Code Max Usage")
                && !bottom.contains("weekly 29% left"),
            "only the matching status card's own fields are attributable: {bottom:?}"
        );
        {
            let entry = app.pool.get(session).expect("session");
            let mut timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
            assert!(timeline.clear_claude_footer());
            timeline.note_foreground_group(6162);
            timeline.set_program(6162, Some("muse".into()));
            assert!(
                timeline.muse_cards_need_account(),
                "clearing facts and changing process cannot clear surviving old glass"
            );
        }
        host.shutdown_and_join();
    }

    #[test]
    fn a_pane_with_no_context_reading_draws_no_context_fact() {
        const COLS: usize = 240;
        const READING: &str = "ctx 84% left";
        let mark = footer::CONTEXT_MARK;
        let rule = "\u{2500}".repeat(COLS);
        let indicator = format!(
            "{:>COLS$}",
            "Context left until auto-compact: 84%",
            COLS = COLS
        );
        let over_composer = |shown: bool| {
            if shown {
                indicator.clone()
            } else {
                String::new()
            }
        };
        let claude_screen = |ctx: bool, mode_row_end: &str| -> Vec<String> {
            let hints = "  \u{23F5}\u{23F5} bypass permissions on (shift+tab to cycle)";
            let pad = COLS - hints.chars().count() - mode_row_end.chars().count();
            vec![
                String::new(),
                " \u{2590}\u{259B}\u{2588}\u{2588}\u{2588}\u{259C}\u{258C}   Claude Code v2.1.92".into(),
                "\u{259D}\u{259C}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{259B}\u{2598}  Opus 4.6 \u{00B7} Claude Max".into(),
                "  \u{2598}\u{2598} \u{259D}\u{259D}    ~/aterm".into(),
                over_composer(ctx),
                rule.clone(),
                "\u{276F} ".into(),
                rule.clone(),
                format!("{hints}{}{mode_row_end}", " ".repeat(pad)),
            ]
        };
        let agy_screen = vec![
            "      \u{2584}\u{2580}\u{2580}\u{2584}        Antigravity CLI 1.4.0".to_string(),
            "    \u{2580}\u{2580}\u{2580}\u{2580}\u{2580}\u{2580}\u{2580}\u{2580}      Gemini 4 Argon (Medium)".into(),
            String::new(),
            rule.clone(),
            "> ".into(),
            rule.clone(),
            "? for shortcuts".into(),
        ];
        let muse_screen = vec![
            String::new(),
            "  Muse Code 0.9.3".to_string(),
            String::new(),
            rule.clone(),
            "\u{276F} ".into(),
            rule.clone(),
            "  gpt-5.4 \u{00B7} high \u{00B7} ~/aterm \u{00B7} Auto-review".into(),
        ];
        let bottom = |app: &App, wid: WindowId, rows: &[String]| {
            let at = rows
                .iter()
                .rposition(|r| r.starts_with('\u{2500}'))
                .expect("the composer's bottom rule");
            text_of(&app.windows[&wid].input_scratch.cells[strip(app, wid) + at])
        };
        let named = |app: &App, session: u64, pgid: i32, program: &str| {
            let entry = app.pool.get(session).expect("session");
            let mut timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
            timeline.note_foreground_group(pgid);
            assert!(timeline.set_program(pgid, Some(program.into())));
            entry.ctx.timeline.clone()
        };
        let context = |timeline: &Arc<Mutex<SessionTimeline>>| {
            timeline
                .lock()
                .unwrap()
                .claude_footer()
                .expect("the pane's facts")
                .context
                .clone()
        };
        let asks = Asks {
            files: |_| None,
            tap: NO_TAP,
            args: |_| Some(args_of(&["HOME=/home/someone"])),
            roster: || None,
            home: scratch_home,
            stored_login: |_, _, _| true,
        };
        let fresh = |shown: &str, program: &str| {
            assert!(
                shown.contains(footer::ACCOUNT_MARK) && !shown.contains(mark),
                "{program}: the facts, and no context fact without a reading: {shown:?}"
            );
        };
        let drawn = |shown: &str, program: &str, reading: &str| {
            assert!(
                shown.contains(&format!("{mark} {reading}")),
                "{program}: a reading is drawn: {shown:?}"
            );
        };

        const PID: i32 = 4261;
        const START: u64 = 1_790_607_861;
        let (root, home) = claude_home("footer-no-context-reading", PID, START);
        let (mut app, wid, session) = glass(COLS as u16);
        let timeline = named(&app, session, PID, "claude");
        let mut identity = unread(PID, &home);
        identity.started = Some(START);
        let mut tail = footer::TailCache::default();
        let mut fold = footer::FooterCache::default();
        let mut memo = RosterMemo::default();
        resolve_facts(
            &timeline,
            &mut identity,
            &mut tail,
            &mut fold,
            &mut memo,
            asks,
        );
        assert_eq!(context(&timeline), None, "claude: no reading, no fact");
        let screen = claude_screen(false, "");
        feed_rows(&app, session, &screen);
        frame(&mut app, wid);
        fresh(&bottom(&app, wid, &screen), "claude");
        let screen = claude_screen(true, "");
        feed_rows(&app, session, &screen);
        frame(&mut app, wid);
        drawn(&bottom(&app, wid, &screen), "claude", READING);
        resolve_facts(
            &timeline,
            &mut identity,
            &mut tail,
            &mut fold,
            &mut memo,
            asks,
        );
        assert_eq!(
            context(&timeline).as_deref(),
            Some(READING),
            "claude: the reading stays through the next registry read"
        );
        let screen = claude_screen(false, "61% until auto-compact");
        feed_rows(&app, session, &screen);
        frame(&mut app, wid);
        drawn(&bottom(&app, wid, &screen), "claude", "ctx 61% left");
        std::fs::remove_dir_all(root).unwrap();

        for (pgid, program, screen) in [(7001, "agy", &agy_screen), (7002, "muse", &muse_screen)] {
            let (mut app, wid, session) = glass(COLS as u16);
            let timeline = named(&app, session, pgid, program);
            feed_rows(&app, session, screen);
            frame(&mut app, wid);
            assert_eq!(context(&timeline), None, "{program}: no reading, no fact");
            fresh(&bottom(&app, wid, screen), program);
            {
                let mut held = timeline.lock().unwrap();
                let mut facts = held.claude_footer().cloned().expect("the frame's facts");
                facts.context = Some(READING.into());
                assert!(held.set_claude_footer(pgid, None, Some(facts)));
            }
            frame(&mut app, wid);
            drawn(&bottom(&app, wid, screen), program, READING);
        }

        const NO_PROCESS: i32 = 999_983;
        let (app, _, session) = glass(COLS as u16);
        let timeline = named(&app, session, NO_PROCESS, "agy");
        let mut identity = unread(NO_PROCESS, &home);
        identity.dir = None;
        let fill = |identity: &mut Identity| {
            resolve_facts(
                &timeline,
                identity,
                &mut footer::TailCache::default(),
                &mut footer::FooterCache::default(),
                &mut RosterMemo::default(),
                asks,
            );
        };
        fill(&mut identity);
        assert_eq!(
            context(&timeline),
            None,
            "agy, resolved: no reading, no fact"
        );
        {
            let mut held = timeline.lock().unwrap();
            let mut facts = held.claude_footer().cloned().expect("the resolver's facts");
            facts.context = Some(READING.into());
            assert!(held.set_claude_footer(NO_PROCESS, None, Some(facts)));
        }
        fill(&mut identity);
        assert_eq!(
            context(&timeline).as_deref(),
            Some(READING),
            "agy, resolved: the reading stays through the next fill"
        );
    }

    #[test]
    fn a_context_reading_goes_when_claude_code_stops_showing_it() {
        const COLS: usize = 160;
        const PID: i32 = 4262;
        let mark = footer::CONTEXT_MARK;
        let rule = "\u{2500}".repeat(COLS);
        let footer_row = |left: &str, end: &str| {
            let pad = COLS - 2 - left.chars().count() - end.chars().count();
            format!("{left}{}{end}", " ".repeat(pad))
        };
        let rest = "  \u{23F8} manual mode on \u{00B7} ? for shortcuts";
        let screen = |caret: &str, under: Vec<String>| -> Vec<String> {
            let mut rows = vec![
                "\u{23FA} Hi! I am the local lab stub, not a model. (reply 1)".to_string(),
                String::new(),
                rule.clone(),
                caret.to_string(),
                rule.clone(),
            ];
            rows.extend(under);
            rows
        };
        let shown = screen("\u{276F}", vec![footer_row(rest, "61% until auto-compact")]);
        let notice = screen(
            "\u{276F} h",
            vec![footer_row(
                "  \u{23F8} manual mode on",
                "\u{25D0} medium \u{00B7} /effort",
            )],
        );
        let menu = screen(
            "\u{276F} /",
            vec![
                "  \u{276F} /add-dir                 Add a new working directory".to_string(),
                "    /agents                  Manage agent configurations".to_string(),
                "    /autocompact             Set how full the context gets before auto-summarizing"
                    .to_string(),
            ],
        );
        let mut boxed = aterm_phase::prompt::fixtures::bash_one_row();
        let last = boxed.len() - 1;
        boxed[last] = footer_row(rest, "");
        let gone = screen("\u{276F}", vec![footer_row(rest, "")]);
        let bottom = |app: &App, wid: WindowId, rows: &[String]| {
            let at = rows
                .iter()
                .rposition(|r| r.starts_with('\u{2500}'))
                .expect("the composer's bottom rule");
            text_of(&app.windows[&wid].input_scratch.cells[strip(app, wid) + at])
        };
        let (mut app, wid, session) = glass(COLS as u16);
        let timeline = {
            let entry = app.pool.get(session).expect("session");
            let mut timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
            timeline.note_foreground_group(PID);
            assert!(timeline.set_program(PID, Some("claude".into())));
            assert!(timeline.set_claude_footer(
                PID,
                None,
                Some(FooterFacts {
                    subscription: Some(
                        aterm_agent::harness::profile::SubscriptionDisplay::default(),
                    ),
                    ..FooterFacts::default()
                }),
            ));
            entry.ctx.timeline.clone()
        };
        let context = || {
            timeline
                .lock()
                .unwrap()
                .claude_footer()
                .expect("the pane's facts")
                .context
                .clone()
        };
        let mut show = |rows: &[String]| {
            feed_rows(&app, session, rows);
            frame(&mut app, wid);
            bottom(&app, wid, rows)
        };
        let reading = format!("{mark} ctx 61% left");
        let drawn = show(&shown);
        assert!(drawn.contains(&reading), "the indicator, read: {drawn:?}");
        for (what, rows) in [
            ("the effort notice in the slot", &notice),
            ("the / menu in the mode row's place", &menu),
            ("a box over the composer", &boxed),
        ] {
            let drawn = show(rows);
            assert!(
                drawn.contains(&reading),
                "{what}: the reading stays: {drawn:?}"
            );
            assert_eq!(context().as_deref(), Some("ctx 61% left"), "{what}");
        }
        let drawn = show(&gone);
        assert!(
            drawn.contains(footer::ACCOUNT_MARK) && !drawn.contains(mark),
            "the slot empty: the fact goes with the indicator: {drawn:?}"
        );
        assert_eq!(context(), None, "and from the session's facts");
        let drawn = show(&shown);
        assert!(
            drawn.contains(&reading),
            "shown again, drawn again: {drawn:?}"
        );
    }

    #[test]
    fn single_toggle_turns_the_harness_white_and_back_for_claude_agy_and_muse() {
        let rule = "\u{2500}".repeat(160);
        let claude_fixture = vec![
            String::new(),
            " \u{2590}\u{259B}\u{2588}\u{2588}\u{2588}\u{259C}\u{258C}   Claude Code v2.1.92".into(),
            "\u{259D}\u{259C}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{259B}\u{2598}  Opus 4.6 \u{00B7} Claude Max".into(),
            "  \u{2598}\u{2598} \u{259D}\u{259D}    ~/aterm".into(),
            String::new(),
            rule.clone(),
            "\u{276F} ".into(),
            rule.clone(),
            format!(
                "{:<136}{}",
                "  \u{23F5}\u{23F5} bypass permissions on (shift+tab to cycle)",
                "84% until auto-compact"
            ),
        ];
        let agy_fixture = vec![
            "      \u{2584}\u{2580}\u{2580}\u{2584}        Antigravity CLI 1.4.0".into(),
            "     \u{2580}\u{2580}\u{2580}\u{2580}\u{2580}\u{2580}       redacted-user@example.test (Google AI Ultra)".into(),
            "    \u{2580}\u{2580}\u{2580}\u{2580}\u{2580}\u{2580}\u{2580}\u{2580}      Gemini 4 Argon (Medium)".into(),
            "   \u{2584}\u{2580}\u{2580}    \u{2580}\u{2580}\u{2584}     ~/aterm".into(),
            "  \u{2584}\u{2580}\u{2580}      \u{2580}\u{2580}\u{2584}".into(),
            "\u{25CF} # Heading".into(),
            rule.clone(),
            "> ".into(),
            rule.clone(),
            "? for shortcuts                                                                                  Gemini 4 Argon \u{00B7} medium".into(),
        ];
        let muse_fixture = vec![
            String::new(),
            "  Muse Code 0.9.3".into(),
            String::new(),
            format!(
                "\u{2500}\u{2500} Voice input (\u{2325} + v to start) {}",
                "\u{2500}".repeat(128)
            ),
            "\u{276F} ".into(),
            rule,
            "  gpt-5.4 \u{00B7} high \u{00B7} ~/aterm \u{00B7} Auto-review".into(),
        ];

        for (prog, build, rows, top_rule_row, card_row) in [
            ("claude", "2.1.92", claude_fixture, 5usize, 4usize),
            ("agy", "1.4.2", agy_fixture, 6usize, 4usize),
            ("muse", "0.9.3", muse_fixture, 3usize, 2usize),
        ] {
            let (mut app, wid, session) = glass(160);
            {
                let entry = app.pool.get(session).expect("session");
                let mut timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
                timeline.note_foreground_group(7000);
                timeline.set_program(7000, Some(prog.into()));
                assert!(timeline.set_claude_footer(
                    7000,
                    None,
                    Some(FooterFacts {
                        model_open: true,
                        effort_open: true,
                        path: Some("~/aterm".into()),
                        branch: Some("main".into()),
                        owner: Some(footer::FactsOwner {
                            floor: None,
                            session_id: format!("{prog}-7000"),
                        }),
                        subscription: Some(
                            aterm_agent::harness::profile::SubscriptionDisplay::default(),
                        ),
                        context: Some("ctx 84% left".into()),
                        ..FooterFacts::default()
                    }),
                ));
            }
            let host = crate::harness_host::HostHandle::for_supervised_test(
                Vec::new(),
                std::sync::Arc::new(|_| crate::harness_host::BodyEnd::Stopped),
            );
            app.harness = Some(host.clone());
            let ctx = app.pool.get(session).expect("session").ctx.clone();
            crate::session_timeline::claim_supervisor(
                &ctx,
                &crate::harness_host::holder(),
                None,
                None,
                crate::metrics::now_us(),
            )
            .expect("claim");

            super::probe::drawer(&app, session, 7000, prog, build);
            feed_rows(&app, session, &rows);
            frame(&mut app, wid);

            let strip = strip(&app, wid);
            let expected_lockup = format!("\u{2500}\u{2500}    {prog} \u{2500}");
            let expected_switch = if prog == "agy" {
                " alab harness v0.1 \u{2590} \u{25CF} on \u{258C} "
            } else {
                " alab harness \u{25CF} on "
            };
            let sid = app.pool.get(session).expect("session").ctx.self_id.clone();
            let chip_line = |app: &App| {
                let head = format!("autopilot sid={} ", sid.as_str());
                app.drawn_chip_lines()
                    .into_iter()
                    .find(|l| l.starts_with(&head))
                    .expect("an autopilot line for the session")
            };
            let line_on = chip_line(&app);
            assert!(
                line_on.contains(" drawn=\"") && !line_on.contains(" why="),
                "{prog}: the harness on draws its chip: {line_on}"
            );
            let top_on = text_of(&app.windows[&wid].input_scratch.cells[strip + top_rule_row]);
            assert!(
                top_on.starts_with(&expected_lockup) && top_on.contains(expected_switch),
                "{prog}: active harness lays the lockup at the top rule's left and the switch at its right: {top_on:?}"
            );
            let bottom_on =
                text_of(&app.windows[&wid].input_scratch.cells[strip + top_rule_row + 2]);
            assert!(
                bottom_on.contains("\u{2386} ")
                    && bottom_on.contains("\u{25f7} weekly ")
                    && bottom_on.contains("\u{25D0} ctx 84% left"),
                "{prog}: standardized bottom rule carries account email, weekly budget, and context: {bottom_on:?}"
            );
            if prog == "agy" {
                assert!(
                    bottom_on.contains("redacted-user@example.test")
                        && bottom_on.contains("Google AI Ultra"),
                    "agy: launch card account email and plan populate standardized footer: {bottom_on:?}"
                );
            }
            if prog == "muse" {
                assert!(
                    !top_on.contains("Voice input"),
                    "muse: the lockup takes the idle voice label's cells: {top_on:?}"
                );
                let row = &app.windows[&wid].input_scratch.cells[strip + top_rule_row];
                let at = row
                    .windows(4)
                    .position(|w| w.iter().map(|c| c.ch).eq("muse".chars()))
                    .expect("the muse word");
                for letter in &row[at..at + 4] {
                    assert!(
                        letter.fg[2] > 150 && letter.fg[0] < 80,
                        "muse: the name is Meta blue, not purple: {:?}",
                        letter.fg
                    );
                }
            }
            let card_on = text_of(&app.windows[&wid].input_scratch.cells[strip + card_row]);
            if prog == "agy" {
                assert!(
                    card_on.contains("enhanced by aterm harness")
                        && card_on.contains("\u{00B7} agy v0.1"),
                    "agy: active harness draws launch card banner: {card_on:?}"
                );
            } else {
                assert!(
                    card_on.contains("enhanced by aterm harness")
                        && card_on.contains("autopilot")
                        && (prog != "muse" || card_on.contains("\u{00B7} muse v0.1")),
                    "{prog}: active harness draws launch card banner: {card_on:?}"
                );
            }
            assert!(
                app.windows[&wid]
                    .claude_card
                    .painted_layers()
                    .iter()
                    .any(|l| l.slot == aterm_core::render::FxSlot::Over
                        && !l.content.items().is_empty()),
                "{prog}: active harness draws rainbow kitty"
            );

            let off_view = app.toggle_session_harness(session);
            assert_eq!(
                off_view.mode(),
                Some(aterm_agent::supervise::Autopilot::Off),
                "{prog}: single toggle switches harness to Off"
            );
            frame(&mut app, wid);

            let top_off = text_of(&app.windows[&wid].input_scratch.cells[strip + top_rule_row]);
            let card_off = text_of(&app.windows[&wid].input_scratch.cells[strip + card_row]);
            let line_off = chip_line(&app);
            if prog == "agy" {
                assert!(
                    line_off.ends_with(" face=off drawn=- why=switch-only spoken=- description=-"),
                    "agy: chrome names the footer left bare on purpose, not a tab behind: \
                     {line_off}"
                );
            } else {
                assert!(
                    line_off.contains(" face=off drawn=\"\u{25CB} off\" "),
                    "{prog}: the off chip is drawn: {line_off}"
                );
            }
            if prog == "agy" {
                assert!(
                    top_off
                        .contains(" alab harness v0.1 \u{25CB} off \u{2590} \u{25CF} on \u{258C} ")
                        && !top_off.starts_with(&expected_lockup),
                    "agy: toggled-off harness leaves only the off switch button on the top rule: {top_off:?}"
                );
                assert!(
                    !card_off.contains("enhanced by aterm harness"),
                    "agy: toggled-off harness disables launch card banner: {card_off:?}"
                );
                let raw_md = text_of(&app.windows[&wid].input_scratch.cells[strip + 5]);
                assert_eq!(
                    raw_md.trim_end(),
                    "\u{25CF} # Heading",
                    "agy: toggled-off harness restores raw transcript row"
                );
            } else {
                assert!(
                    top_off.contains(" alab harness \u{25CB} off \u{2590} on \u{258C} ")
                        && top_off.starts_with(&expected_lockup)
                        && !top_off.contains("alab kitty"),
                    "{prog}: toggled-off harness draws its off switch, `on` its button, on the \
                     white top rule, the lockup still at its left and no wordmark: {top_off:?}"
                );
                assert!(
                    card_off.contains("enhanced by aterm harness")
                        && card_off.contains("autopilot \u{25CB} off"),
                    "{prog}: toggled-off harness wears the white card: {card_off:?}"
                );
            }

            let on_view = app.toggle_session_harness(session);
            assert_eq!(
                on_view.mode(),
                Some(aterm_agent::supervise::Autopilot::Cruise),
                "{prog}: second single toggle seamlessly re-enables Cruise"
            );
            frame(&mut app, wid);

            let top_reenabled =
                text_of(&app.windows[&wid].input_scratch.cells[strip + top_rule_row]);
            assert!(
                top_reenabled.contains(expected_switch),
                "{prog}: re-enabled harness restores composer top rule branding: {top_reenabled:?}"
            );
            let card_reenabled = text_of(&app.windows[&wid].input_scratch.cells[strip + card_row]);
            assert!(
                card_reenabled.contains("enhanced by aterm harness"),
                "{prog}: re-enabled harness restores launch card banner: {card_reenabled:?}"
            );

            assert!(
                !app.windows[&wid].claude_lights.border_toggles.is_empty(),
                "{prog}: border_toggles registered when On"
            );
            app.toggle_session_harness(session);
            frame(&mut app, wid);
            assert!(
                !app.windows[&wid].claude_lights.border_toggles.is_empty(),
                "{prog}: border_toggles still registered when Off, so its `on` turns it back on"
            );

            host.shutdown_and_join();
        }
    }

    #[test]
    fn the_composer_border_wears_the_cards_palette() {
        use aterm_agent::supervise::Autopilot;
        let rule = "\u{2500}".repeat(120);
        let rows: Vec<String> = vec![
            String::new(),
            " \u{2590}\u{259B}\u{2588}\u{2588}\u{2588}\u{259C}\u{258C}   Claude Code v2.1.92".into(),
            "\u{259D}\u{259C}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{259B}\u{2598}  Opus 4.6 \u{00B7} Claude Max".into(),
            "  \u{2598}\u{2598} \u{259D}\u{259D}    ~/aterm".into(),
            String::new(),
            rule.clone(),
            "\u{276F} ".into(),
            rule,
            "  \u{23F5}\u{23F5} bypass permissions on (shift+tab to cycle)".into(),
        ];
        let (mut app, wid, session) = glass(120);
        {
            let entry = app.pool.get(session).expect("session");
            let mut timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
            timeline.note_foreground_group(7000);
            timeline.set_program(7000, Some("claude".into()));
            assert!(timeline.set_claude_footer(
                7000,
                None,
                Some(FooterFacts {
                    model_open: true,
                    effort_open: true,
                    path: Some("~/aterm".into()),
                    branch: Some("main".into()),
                    owner: Some(footer::FactsOwner {
                        floor: None,
                        session_id: "claude-7000".into(),
                    }),
                    ..FooterFacts::default()
                }),
            ));
        }
        let host = crate::harness_host::HostHandle::for_supervised_test(
            Vec::new(),
            std::sync::Arc::new(|_| crate::harness_host::BodyEnd::Stopped),
        );
        app.harness = Some(host.clone());
        let ctx = app.pool.get(session).expect("session").ctx.clone();
        let claim = || {
            crate::session_timeline::claim_supervisor(
                &ctx,
                &crate::harness_host::holder(),
                None,
                None,
                crate::metrics::now_us(),
            )
            .expect("claim");
        };
        let release = || {
            crate::session_timeline::release_supervisor(
                &ctx,
                None,
                crate::metrics::now_us(),
                aterm_slog::Actor::Ctl,
            );
        };
        feed_rows(&app, session, &rows);
        let under = |app: &mut App, word: Autopilot| {
            app.set_session_autopilot(session, Some(word), crate::autopilot::Surface::Chip);
            frame(app, wid);
            let top = strip(app, wid) + 5;
            let cells = &app.windows[&wid].input_scratch.cells[top];
            let run: Vec<[u8; 3]> = cells
                .iter()
                .filter(|c| c.ch == '\u{2500}')
                .map(|c| c.fg)
                .collect();
            (run, text_of(cells))
        };

        app.config.claude_footer = Some(crate::app_config::ClaudeFooterConfig {
            card: Some(false),
            ..Default::default()
        });
        let (bare, text) = under(&mut app, Autopilot::Cruise);
        assert!(
            bare.windows(2).all(|w| w[0] == w[1]) && !text.contains("alab kitty"),
            "CONTROL: unclaimed bare, the rule untinted: {text:?}"
        );
        let plan = app.active_visible_leaf_plan(wid).expect("plan");
        let unclaimed = app.claude_footer_fp(wid, &plan);
        assert_eq!(app.claude_footer_fp(wid, &plan), unclaimed, "no churn");
        claim();
        assert_ne!(
            app.claude_footer_fp(wid, &plan),
            unclaimed,
            "the claim landing repaints the border with the card off"
        );
        let (claimed, text) = under(&mut app, Autopilot::Cruise);
        assert!(
            text.contains("harness \u{25CF} on") && claimed.windows(2).any(|w| w[0] != w[1]),
            "claimed, tinted: {text:?}"
        );
        app.config.claude_footer = None;
        let blue_violet = |c: [u8; 3]| {
            i32::from(c[2]) >= i32::from(c[0]) + 24 && i32::from(c[2]) >= i32::from(c[1]) + 24
        };
        let warm = |c: [u8; 3]| c[0] >= c[1] && c[0] >= c[2];
        let blue = |c: [u8; 3]| c[0] <= c[1] && i32::from(c[2]) >= i32::from(c[0].max(c[1])) + 56;

        let (cruise, text) = under(&mut app, Autopilot::Cruise);
        assert!(text.contains("harness \u{25CF} on"), "{text:?}");
        assert!(cruise.len() > 60, "the run: {}", cruise.len());
        assert!(
            cruise.iter().copied().any(blue_violet),
            "Cruise's rule reaches blue and violet: {cruise:?}"
        );
        assert!(
            cruise.iter().copied().any(blue),
            "Cruise's visible rule shows true blue: {cruise:?}"
        );
        let ground = app.windows[&wid].input_scratch.implicit_blank;
        let dark = crate::claude_card::dark_ground(ground);
        let paint = |t: f32| {
            let c = aterm_effects::harness_card::CardPalette::Cruise.art(t, dark);
            let stop = [(c >> 16) as u8, (c >> 8) as u8, c as u8];
            super::mix([90, 90, 90], super::mix(stop, ground.fg, 0.25), 0.68)
        };
        assert!(
            text.starts_with("\u{2500}\u{2500}    claude \u{2500}")
                && text.ends_with(" alab harness \u{25CF} on \u{2500}\u{2500}"),
            "the lockup at the rule's left, the switch at its right: {text:?}"
        );
        let paint = |t: f32| {
            let fg = paint(t);
            if fg == [90, 90, 90] {
                [fg[0] ^ 1, fg[1], fg[2]]
            } else {
                fg
            }
        };
        let shown: Vec<[u8; 3]> = [paint(1.0), paint(1.0)]
            .into_iter()
            .chain((0..86).map(|j| paint(1.0 - j as f32 / 85.0)))
            .chain([paint(1.0), paint(1.0)])
            .collect();
        assert_eq!(cruise, shown, "the palette backward over the gap");
        assert!(
            !cruise.iter().copied().all(warm),
            "CONTROL: Cruise's rule is not a sunset"
        );
        let mut old_half = (0..=255).map(|i| {
            let c = aterm_effects::spectrum::SPECTRUM_LUT[i];
            [(c >> 16) as u8, (c >> 8) as u8, c as u8]
        });
        assert!(
            !old_half.any(blue_violet),
            "CONTROL: the old reading stopped short of blue"
        );

        let (land, text) = under(&mut app, Autopilot::Land);
        assert!(text.contains("harness \u{25CF} on"), "{text:?}");
        assert!(
            land.iter().copied().all(warm),
            "Land's rule is the sunset: {land:?}"
        );

        let (off, text) = under(&mut app, Autopilot::Off);
        assert!(
            text.contains(" alab harness \u{25CB} off \u{2590} on \u{258C} ")
                && !text.contains("alab kitty"),
            "Off's switch says off, `on` its button, no wordmark: {text:?}"
        );
        for c in &off {
            let spread = c.iter().max().unwrap() - c.iter().min().unwrap();
            assert!(spread <= 24, "Off's rule is the moonbow: {c:?} in {off:?}");
        }
        assert_ne!(off, cruise, "Off is not Cruise");
        release();
        assert!(!app.session_autopilot(session).2, "the claim is given back");
        let (released, text) = under(&mut app, Autopilot::Off);
        assert!(
            text.contains("alab harness \u{25CB} off"),
            "no claim: {text:?}"
        );
        assert_eq!(released, off, "white with no claim");
        let (frame_row, cols) = app.windows[&wid]
            .claude_lights
            .border_toggles
            .iter()
            .find(|t| t.session == session)
            .map(|t| (t.frame_row, t.cols.clone()))
            .expect("the switch is a target");
        let col = cols.start + 2;
        let (cw, ch) = app.win_cell_size(wid);
        let (ox, oy) = app.frame_origin(wid);
        let x = ox as f64 + (app.win_pad(wid) + col * cw + cw / 2) as f64;
        let y =
            oy as f64 + (app.win_pad_top(wid) + app.win_head(wid) + frame_row * ch + ch / 2) as f64;
        assert_eq!(app.frame_cell_at(wid, x, y), Some((frame_row, col)));
        app.windows.get_mut(&wid).expect("window").last_cursor_px = (x, y);
        assert!(app.press_claude_light(wid), "the switch takes the press");
        assert_eq!(
            app.session_autopilot(session).0,
            Some(Autopilot::Cruise),
            "Off's switch turns the harness back on"
        );

        claim();
        {
            let term = app.pool.get(session).expect("session").term.clone();
            crate::term_lock(&term)
                .process(b"\x1b]11;rgb:ff/ff/ff\x1b\\\x1b]10;rgb:20/20/20\x1b\\");
        }
        let contrast = |c: [u8; 3]| {
            use aterm_effects::color_math::relative_luminance;
            let y = relative_luminance(u32::from_be_bytes([0, c[0], c[1], c[2]]));
            let paper = relative_luminance(0x00FF_FFFF);
            (y.max(paper) + 0.05) / (y.min(paper) + 0.05)
        };
        for word in [Autopilot::Cruise, Autopilot::Off] {
            let (_, text) = under(&mut app, word);
            let cells = &app.windows[&wid].input_scratch.cells[strip(&app, wid) + 5];
            if word == Autopilot::Off {
                assert!(
                    !text.contains('\u{2726}'),
                    "Off wears no wordmark: {text:?}"
                );
                let off = cells.iter().position(|c| c.ch == '\u{25CB}').unwrap();
                assert!(contrast(cells[off].fg) >= 4.5, "Off's state on paper");
                continue;
            }
            let face: Vec<char> = "alab harness \u{25CF} on".chars().collect();
            let at = (0..cells.len().saturating_sub(face.len()))
                .find(|&k| {
                    cells[k..k + face.len()]
                        .iter()
                        .map(|c| c.ch)
                        .eq(face.iter().copied())
                })
                .unwrap_or_else(|| panic!("{word:?}: the switch on paper: {text:?}"));
            let letters: Vec<[u8; 3]> = cells[at..at + face.len()]
                .iter()
                .chain(&cells[6..12])
                .filter(|c| c.ch != ' ')
                .map(|c| c.fg)
                .collect();
            assert_eq!(
                letters.len(),
                "alabharness\u{25CF}onclaude".chars().count(),
                "{text:?}"
            );
            for c in &letters {
                assert!(contrast(*c) >= 4.5, "{word:?}: {c:?} too pale on paper");
            }
        }
        let ink = [0x20, 0x20, 0x20];
        let head: Vec<char> = " alab harness \u{25CF} on ".chars().collect();
        let old_pale = (0..head.len()).filter(|&k| head[k] != ' ').any(|k| {
            let c = aterm_effects::spectrum::SPECTRUM_LUT[(k * 255) / head.len()];
            contrast(mix([(c >> 16) as u8, (c >> 8) as u8, c as u8], ink, 0.25)) < 4.5
        });
        assert!(old_pale, "CONTROL: the old letters vanish on paper");

        app.harness_policy = crate::harness_policy_cell::HarnessPolicyCell::new(
            crate::app_config::HarnessPolicy::read("[harness]\nenabled = false\n"),
        );
        let (_, text) = under(&mut app, Autopilot::Off);
        assert!(
            !text.contains("alab kitty")
                && text.contains(" alab harness \u{25CB} off \u{2590} on \u{258C} "),
            "master off: the bare off switch {text:?}"
        );
        host.shutdown_and_join();
    }

    #[test]
    fn agy_glass_presentation_preserves_literal_cells_and_undoes_cleanly() {
        fn collect(scratch: &RenderInput) -> Vec<AgyRowEdit> {
            collect_for(scratch, aterm_phase::Program::Agy)
        }
        fn collect_for(scratch: &RenderInput, program: aterm_phase::Program) -> Vec<AgyRowEdit> {
            let rows: Vec<String> = scratch
                .cells
                .iter()
                .map(|r| r.iter().map(|c| c.ch).collect())
                .collect();
            let mut edits = Vec::new();
            collect_agy_transcript_edits(
                scratch,
                program,
                &rows,
                PaneTextRegion {
                    strip: 0,
                    row_off: 0,
                    col_off: 0,
                    rows: scratch.rows,
                    cols: scratch.cols,
                },
                Some(scratch.rows),
                RenderCell::default(),
                RenderCell {
                    fg: [80, 80, 80],
                    ..RenderCell::default()
                },
                [180, 100, 80],
                &mut edits,
            );
            edits
        }
        let rows = [
            "Signing in...",
            "Authenticated.",
            "  └ Tip: literal tool output",
            "^[[?2026$p^[P>|aterm(0.42.0)",
            "○ Thought for 1s (literal suffix)",
            "● # Overview",
            "  Here is **bold**, *italic*, and `code`.",
            "  ```text",
            "  Signing in...",
            "  └ Tip: literal code",
            "  **literal**",
            "  ^[[?2026$p",
            "  ```",
            "  - First item",
            "  ```text",
            "──────────────",
            "> x",
            "hello",
            "  **literal**",
            "      ▄▀▀▄        Antigravity CLI 1.4.0",
            "     ▀▀▀▀▀▀       redacted-user@example.test (Google AI Ultra)",
            "    ▀▀▀▀▀▀▀▀      Gemini 4 Argon (Medium)",
            "   ▄▀▀    ▀▀▄     ~/aterm",
            "  ▄▀▀      ▀▀▄",
            "  **still literal after a quoted card**",
            "  ```",
        ];
        let quoted: Vec<String> = rows.iter().map(|r| (*r).to_owned()).collect();
        assert!(aterm_phase::agy::launch_card_scan(&quoted).1.is_some());
        let mut scratch = RenderInput::empty();
        scratch.rows = rows.len();
        scratch.cols = 100;
        scratch.cursor_visible = false;
        scratch.cells = rows.iter().map(|r| row_of(r, [0, 0, 0])).collect();
        let original = scratch.cells.clone();
        let edits = collect(&scratch);
        assert_eq!(
            edits.iter().map(|e| e.frame_row).collect::<Vec<_>>(),
            vec![5, 6, 13]
        );
        let mut undo_slot = FooterUndo::default();
        write_edits(
            &mut undo_slot,
            &mut scratch,
            edits
                .iter()
                .map(|e| (e.frame_row, e.col, e.cells.as_slice(), e.ground)),
        );
        assert_eq!(text_of(&scratch.cells[5]).trim_end(), "● Overview");
        assert!(
            scratch.cells[5][2].bold
                && scratch.cells[5][2].underline == aterm_core::terminal::UnderlineStyle::Single,
            "H1 heading is bold + underlined"
        );
        assert_eq!(
            text_of(&scratch.cells[6]).trim_end(),
            "  Here is bold, italic, and code."
        );
        assert!(scratch.cells[6][10].bold && scratch.cells[6][16].italic);
        assert_ne!(
            scratch.cells[6][28].bg,
            RenderCell::default().bg,
            "inline code pill background tinted"
        );
        assert_eq!(text_of(&scratch.cells[13]).trim_end(), "  • First item");
        for (r, original_row) in original.iter().enumerate().take(scratch.rows) {
            if ![5, 6, 13].contains(&r) {
                assert_eq!(&scratch.cells[r], original_row);
            }
        }
        assert!(undo(&mut undo_slot, &mut scratch));
        assert_eq!(scratch.cells, original);

        let md_rows = [
            "  ## Section",
            "  ### Detail",
            "  See [docs](https://example.com) now.",
            "  - [ ] Pending",
            "    - Nested",
            "  > Quoted",
        ];
        scratch.rows = md_rows.len();
        scratch.cells = md_rows.iter().map(|r| row_of(r, [0, 0, 0])).collect();
        let md_edits = collect(&scratch);
        assert_eq!(md_edits.len(), 6);
        assert_eq!(text_of(&md_edits[0].cells).trim_end(), "  Section");
        assert!(
            md_edits[0].cells[2].bold
                && !md_edits[0].cells[2].italic
                && md_edits[0].cells[2].underline == aterm_core::terminal::UnderlineStyle::None
        );
        assert_eq!(text_of(&md_edits[1].cells).trim_end(), "  Detail");
        assert!(md_edits[1].cells[2].bold && md_edits[1].cells[2].italic);
        assert_eq!(text_of(&md_edits[2].cells).trim_end(), "  See docs now.");
        assert_eq!(
            md_edits[2].cells[6].underline,
            aterm_core::terminal::UnderlineStyle::Single
        );
        assert_eq!(text_of(&md_edits[3].cells).trim_end(), "  \u{2610} Pending");
        assert_eq!(
            text_of(&md_edits[4].cells).trim_end(),
            "    \u{25E6} Nested"
        );
        assert_eq!(text_of(&md_edits[5].cells).trim_end(), "  \u{258E} Quoted");

        let muse_rows = [
            "  Muse Code 0.9.3",
            "\u{276f} user prompt",
            "  **wrapped user text stays literal**",
            "  \u{256d}\u{2500} Bash",
            "  \u{2502} **tool output stays literal**",
            "  \u{2570}\u{2500}",
            "  \u{25c7} Thinking **literal**...",
            "\u{25c6} # Muse Heading",
            "  Here is ***bold italic*** and **bold `code`**.",
            "  ---",
        ];
        scratch.rows = muse_rows.len();
        scratch.cells = muse_rows.iter().map(|r| row_of(r, [0, 0, 0])).collect();
        let muse_edits = collect_for(&scratch, aterm_phase::Program::Muse);
        assert_eq!(
            muse_edits.iter().map(|e| e.frame_row).collect::<Vec<_>>(),
            vec![7, 8, 9],
            "only Muse assistant reply rows (7, 8, 9) are styled"
        );
        assert_eq!(
            text_of(&muse_edits[0].cells).trim_end(),
            "\u{25c6} Muse Heading"
        );
        assert_eq!(
            text_of(&muse_edits[1].cells).trim_end(),
            "  Here is bold italic and bold code."
        );
        assert!(muse_edits[1].cells[10].bold && muse_edits[1].cells[10].italic);
        assert!(
            muse_edits[2]
                .cells
                .iter()
                .skip(2)
                .take(10)
                .all(|c| c.ch == '\u{2500}'),
            "horizontal rule rendered as box-drawing line"
        );

        scratch.rows = 3;
        scratch.cells = [8, 9, 11]
            .into_iter()
            .map(|r| original[r].clone())
            .collect();
        assert!(collect(&scratch).is_empty());

        scratch.rows = 1;
        scratch.cells = vec![row_of("  **text**", [0, 0, 0])];
        assert_eq!(collect(&scratch).len(), 1);
        scratch.cells = vec![row_of("  **你 **", [0, 0, 0])];
        scratch.cells[0][5].wide = true;
        assert!(collect(&scratch).is_empty());
        scratch.cells = vec![row_of("  **❤ **", [0, 0, 0])];
        scratch.cells[0][4].emoji_presentation = true;
        scratch.cells[0][5].wide = true;
        assert!(collect(&scratch).is_empty());
        scratch.cells = vec![row_of("  **😀**", [0, 0, 0])];
        scratch.cells[0][4].text_presentation = true;
        assert!(collect(&scratch).is_empty());
        scratch.cells = vec![row_of("  **גבא**", [0, 0, 0])];
        let mut map: Vec<u16> = (0..scratch.cols)
            .map(|c| u16::try_from(c).unwrap())
            .collect();
        map.swap(4, 6);
        scratch.visual_to_logical = vec![map];
        assert!(collect(&scratch).is_empty());
        scratch.visual_to_logical.clear();
        assert_eq!(collect(&scratch).len(), 1);
        scratch.cells = vec![row_of("  **text**", [0, 0, 0])];
        scratch.selection.start_selection(
            0,
            2,
            aterm_core::selection::SelectionSide::Left,
            aterm_core::selection::SelectionType::Simple,
        );
        scratch
            .selection
            .update_selection(0, 7, aterm_core::selection::SelectionSide::Right);
        scratch.selection.complete_selection();
        assert!(scratch.any_selection());
        assert!(collect(&scratch).is_empty());

        let scrolled_rows = [
            "  let **literal_in_scrolled_code** = 1;",
            "  ```",
            "  ## Scrolled Heading",
            "  - Scrolled **bold** and `code` item",
        ];
        let mut scrolled = RenderInput::empty();
        scrolled.rows = scrolled_rows.len();
        scrolled.cols = 100;
        scrolled.cursor_visible = false;
        scrolled.cells = scrolled_rows.iter().map(|r| row_of(r, [0, 0, 0])).collect();
        let scrolled_text: Vec<String> = scrolled_rows.iter().map(|r| (*r).to_owned()).collect();
        let mut scrolled_edits = Vec::new();
        collect_agy_transcript_edits(
            &scrolled,
            aterm_phase::Program::Agy,
            &scrolled_text,
            PaneTextRegion {
                strip: 0,
                row_off: 0,
                col_off: 0,
                rows: scrolled.rows,
                cols: scrolled.cols,
            },
            None,
            RenderCell::default(),
            RenderCell {
                fg: [80, 80, 80],
                ..RenderCell::default()
            },
            [180, 100, 80],
            &mut scrolled_edits,
        );
        assert_eq!(
            scrolled_edits
                .iter()
                .map(|e| e.frame_row)
                .collect::<Vec<_>>(),
            vec![2, 3],
            "scrolled-off code block stays literal (row 0), markdown below closing fence renders (rows 2, 3)"
        );
        assert_eq!(
            text_of(&scrolled_edits[0].cells).trim_end(),
            "  Scrolled Heading"
        );
        assert_eq!(
            text_of(&scrolled_edits[1].cells).trim_end(),
            "  \u{2022} Scrolled bold and code item"
        );

        let multi_block_rows = [
            "  let **literal_block_1** = 1;",
            "  ```",
            "  ## Between Blocks",
            "  - **Important** point",
            "  ```",
            "  let **literal_block_2** = 2;",
            "  ```",
            "  - **Final** summary",
        ];
        scrolled.rows = multi_block_rows.len();
        scrolled.cells = multi_block_rows
            .iter()
            .map(|r| row_of(r, [0, 0, 0]))
            .collect();
        let multi_block_text: Vec<String> =
            multi_block_rows.iter().map(|r| (*r).to_owned()).collect();
        scrolled_edits.clear();
        collect_agy_transcript_edits(
            &scrolled,
            aterm_phase::Program::Agy,
            &multi_block_text,
            PaneTextRegion {
                strip: 0,
                row_off: 0,
                col_off: 0,
                rows: scrolled.rows,
                cols: scrolled.cols,
            },
            None,
            RenderCell::default(),
            RenderCell {
                fg: [80, 80, 80],
                ..RenderCell::default()
            },
            [180, 100, 80],
            &mut scrolled_edits,
        );
        assert_eq!(
            scrolled_edits
                .iter()
                .map(|e| e.frame_row)
                .collect::<Vec<_>>(),
            vec![2, 3, 7],
            "only markdown rows between and after the two code blocks (2, 3, 7) are styled"
        );

        let bottom_open_rows = [
            "  ## Overview Above Fence",
            "  - **First** item",
            "  ```",
            "  # shell comment in unclosed code block",
            "  echo **literal**",
        ];
        scrolled.rows = bottom_open_rows.len();
        scrolled.cells = bottom_open_rows
            .iter()
            .map(|r| row_of(r, [0, 0, 0]))
            .collect();
        let bottom_open_text: Vec<String> =
            bottom_open_rows.iter().map(|r| (*r).to_owned()).collect();
        scrolled_edits.clear();
        collect_agy_transcript_edits(
            &scrolled,
            aterm_phase::Program::Agy,
            &bottom_open_text,
            PaneTextRegion {
                strip: 0,
                row_off: 0,
                col_off: 0,
                rows: scrolled.rows,
                cols: scrolled.cols,
            },
            None,
            RenderCell::default(),
            RenderCell {
                fg: [80, 80, 80],
                ..RenderCell::default()
            },
            [180, 100, 80],
            &mut scrolled_edits,
        );
        assert_eq!(
            scrolled_edits
                .iter()
                .map(|e| e.frame_row)
                .collect::<Vec<_>>(),
            vec![0, 1],
            "markdown above unclosed bottom fence renders (0, 1); code below fence stays literal"
        );

        let rule_80 = "\u{2500}".repeat(80);
        let rule_60 = "\u{2500}".repeat(60);
        let scrolled_rule_rows = [
            "  ## Above Earlier Rule",
            rule_80.as_str(),
            "  ## Below Earlier Rule",
            "  - **Rich** item after rule",
            rule_60.as_str(),
        ];
        scrolled.cols = 80;
        scrolled.rows = scrolled_rule_rows.len();
        scrolled.cells = scrolled_rule_rows
            .iter()
            .map(|r| row_of(r, [0, 0, 0]))
            .collect();
        let scrolled_rule_text: Vec<String> =
            scrolled_rule_rows.iter().map(|r| (*r).to_owned()).collect();
        scrolled_edits.clear();
        collect_agy_transcript_edits(
            &scrolled,
            aterm_phase::Program::Agy,
            &scrolled_rule_text,
            PaneTextRegion {
                strip: 0,
                row_off: 0,
                col_off: 0,
                rows: scrolled.rows,
                cols: scrolled.cols,
            },
            None,
            RenderCell::default(),
            RenderCell {
                fg: [80, 80, 80],
                ..RenderCell::default()
            },
            [180, 100, 80],
            &mut scrolled_edits,
        );
        assert_eq!(
            scrolled_edits
                .iter()
                .map(|e| e.frame_row)
                .collect::<Vec<_>>(),
            vec![0, 2, 3],
            "markdown rows both above (0) and below (2, 3) a rule in a scrolled viewport render"
        );

        let mut split_scratch = RenderInput::empty();
        split_scratch.cols = 120;
        split_scratch.rows = 1;
        let mut full_row = vec![RenderCell::default(); 40];
        full_row.extend(row_of("  ## Split Heading", [0, 0, 0]));
        full_row.resize(120, RenderCell::default());
        split_scratch.cells = vec![full_row];
        split_scratch.cursor_visible = true;
        split_scratch.cursor_row = 0;
        split_scratch.cursor_col = 5;
        let split_rows = vec!["  ## Split Heading".to_owned()];
        scrolled_edits.clear();
        collect_agy_transcript_edits(
            &split_scratch,
            aterm_phase::Program::Agy,
            &split_rows,
            PaneTextRegion {
                strip: 0,
                row_off: 0,
                col_off: 40,
                rows: 1,
                cols: 80,
            },
            None,
            RenderCell::default(),
            RenderCell {
                fg: [80, 80, 80],
                ..RenderCell::default()
            },
            [180, 100, 80],
            &mut scrolled_edits,
        );
        assert_eq!(
            scrolled_edits.len(),
            1,
            "adjacent pane cursor outside col_off..col_off+cols must not skip agy markdown row"
        );
        split_scratch.cursor_col = 45;
        scrolled_edits.clear();
        collect_agy_transcript_edits(
            &split_scratch,
            aterm_phase::Program::Agy,
            &split_rows,
            PaneTextRegion {
                strip: 0,
                row_off: 0,
                col_off: 40,
                rows: 1,
                cols: 80,
            },
            None,
            RenderCell::default(),
            RenderCell {
                fg: [80, 80, 80],
                ..RenderCell::default()
            },
            [180, 100, 80],
            &mut scrolled_edits,
        );
        assert!(
            scrolled_edits.is_empty(),
            "cursor inside the agy pane's own columns skips the active cursor row"
        );
    }

    #[test]
    fn agy_subagent_display_rainbow_blings_transcript_panel_and_footer_rows() {
        let rule_80 = "\u{2500}".repeat(80);
        let screen_rows = [
            "● Spawning 2 subagent(s)... (ctrl+o to expand) \u{00B7} 3s",
            "  └ Agent(self: Codebase Researcher): Reading file... \u{00B7} 4m36s",
            "● Spawned 1 subagent(s)",
            "● Agent(self: Verifier) needs approval for RunCommand",
            rule_80.as_str(),
            "> ",
            rule_80.as_str(),
            "  ● Agent(self: Codebase (tests) Researcher)     Reading file... \u{00B7} 4m36s",
            "  ● Agent(research: Docs (API) Finder)         Searching web... \u{00B7} 8s",
            "  ● [17:20:01] sleep 30                  running",
            rule_80.as_str(),
            "  ? for shortcuts                        Gemini 4 Argon \u{00B7} high \u{00B7} 1 task(s) \u{00B7} 2 subagent(s)",
        ];
        let rows: Vec<String> = screen_rows.iter().map(|r| format!("{r:<100}")).collect();
        let blank = RenderCell {
            ch: ' ',
            fg: [0xd8, 0xdc, 0xe4],
            bg: [0x14, 0x16, 0x1b],
            ..RenderCell::default()
        };
        let mut scratch = RenderInput::empty();
        scratch.rows = rows.len();
        scratch.cols = 100;
        scratch.cursor_visible = false;
        scratch.cells = rows
            .iter()
            .map(|r| {
                r.chars()
                    .map(|ch| RenderCell { ch, ..blank })
                    .collect::<Vec<_>>()
            })
            .collect();
        // Simulate agy coloring the panel's `●` and `Agent(...)` in ANSI blue/green.
        scratch.cells[7][2].fg = [0x40, 0xc0, 0x60];
        for c in
            &mut scratch.cells[7][4..4 + "Agent(self: Codebase (tests) Researcher)".chars().count()]
        {
            c.fg = [0x5c, 0x9c, 0xff];
        }
        let original = scratch.cells.clone();

        // 1. `claude_panel::read_panel` reads agy's open subagent panel rows even when ANSI-tinted,
        // and `PanelRow.name` spans columns `4..4 + unwrapped.chars().count()`.
        let pane_geom = crate::claude_card::PaneGeom {
            strip: 0,
            row_off: 0,
            col_off: 0,
            rows: scratch.rows,
            cols: scratch.cols,
        };
        let panel_read = crate::claude_panel::read_panel(1, &rows, 6, &scratch, pane_geom, blank)
            .expect("agy open subagent panel should be read by claude_panel");
        assert_eq!(panel_read.panel.rows().len(), 2);
        assert_eq!(
            panel_read.panel.rows()[0].name.range(),
            4..4 + "Codebase (tests) Researcher".len()
        );
        assert_eq!(
            panel_read.panel.rows()[1].name.range(),
            4..4 + "research: Docs (API) Finder".len()
        );
        assert!(
            panel_read.ink[0].bullet.is_some() && panel_read.ink[0].name.is_some(),
            "ANSI-colored agy subagent bullet and name are accepted by claude_panel::read_panel"
        );

        // 2. `collect_agy_transcript_edits` rewrites transcript subagent rows, panel rows, and the
        // footer row beneath the open panel.
        let mut edits = Vec::new();
        collect_agy_transcript_edits(
            &scratch,
            aterm_phase::Program::Agy,
            &rows,
            PaneTextRegion {
                strip: 0,
                row_off: 0,
                col_off: 0,
                rows: scratch.rows,
                cols: scratch.cols,
            },
            Some(4),
            blank,
            RenderCell {
                ch: '\u{2500}',
                fg: [0x58, 0x5c, 0x68],
                ..blank
            },
            [0xe0, 0x7a, 0x5f],
            &mut edits,
        );
        let mut undo_slot = FooterUndo::default();
        write_edits(
            &mut undo_slot,
            &mut scratch,
            edits
                .iter()
                .map(|e| (e.frame_row, e.col, e.cells.as_slice(), e.ground)),
        );
        let row_str = |r: usize| -> String {
            scratch.cells[r]
                .iter()
                .map(|c| c.ch)
                .collect::<String>()
                .trim_end()
                .to_owned()
        };
        assert_eq!(
            row_str(0),
            "● Spawning 2 subagents (ctrl+o to expand) \u{00B7} 3s"
        );
        assert_eq!(
            row_str(1),
            "  └ Codebase Researcher  Reading file \u{00B7} 4m36s"
        );
        assert_eq!(row_str(2), "● Spawned 1 subagent");
        assert_eq!(row_str(3), "● Verifier needs approval for RunCommand");
        assert_eq!(
            row_str(7),
            "  ● Codebase (tests) Researcher  Reading file \u{00B7} 4m36s"
        );
        assert_eq!(
            row_str(8),
            "  ● research: Docs (API) Finder  Searching web \u{00B7} 8s"
        );
        assert_eq!(row_str(9), "  ● [17:20:01] sleep 30  running");
        let footer_text = row_str(11);
        assert!(
            footer_text.starts_with("  YOLO on \u{00B7} ? for shortcuts"),
            "open-panel footer keeps YOLO on prefix: {footer_text:?}"
        );
        assert!(
            !footer_text.contains("Gemini 4 Argon"),
            "redundant model/effort stripped from footer: {footer_text:?}"
        );
        assert!(
            footer_text.ends_with("1 task \u{00B7} ● 2 subagents"),
            "footer right-aligns humanized task and rainbow subagent badge: {footer_text:?}"
        );

        // Verify rainbow spectrum ink (bold, distinct hues, >= 4.5:1 contrast) on subagent names.
        let name_range = panel_read.panel.rows()[0].name.range();
        let name_cells = &scratch.cells[7][name_range.clone()];
        let mut distinct_fgs = std::collections::HashSet::new();
        for c in name_cells.iter().filter(|c| c.ch != ' ') {
            assert!(c.bold, "subagent name cell {:?} should be bold", c.ch);
            assert!(
                reads_on(c.fg, blank.bg) >= 4.5,
                "subagent rainbow ink {:?} must meet 4.5:1 contrast on {:?}",
                c.fg,
                blank.bg
            );
            distinct_fgs.insert(c.fg);
        }
        assert!(
            distinct_fgs.len() >= 4,
            "subagent name should span multiple rainbow spectrum stops, got {distinct_fgs:?}"
        );

        // Verify `claude_panel` derives in-flight tool states (`DotState::Tool`) for agy subagents
        // past `QUIET_MS` and emits dot `InkCell` + `RainHalo` without overwriting the rainbow
        // name cells in `scratch.cells`.
        assert!(panel_read.agy_agent[0] && panel_read.agy_agent[1]);
        let mut track = aterm_agent::harness::agents::PanelTrack::default();
        track.fold_flown(
            &panel_read.panel,
            &[aterm_agent::harness::agents::Tint::Plain; aterm_phase::agents_panel::PANEL_ROWS_MAX],
            &panel_read.agy_flights,
            0,
        );
        let mut rows_25s = rows.clone();
        rows_25s[7] =
            "  ● Agent(self: Codebase (tests) Researcher)  Reading file \u{00B7} 5m01s".to_owned();
        rows_25s[8] = "  ● Agent(research: Docs (API) Finder)  Working \u{00B7} 33s".to_owned();
        let panel_read_25s =
            crate::claude_panel::read_panel(1, &rows_25s, 6, &scratch, pane_geom, blank)
                .expect("agy panel after 25s");
        assert!(matches!(
            panel_read.agy_flights[1],
            aterm_agent::harness::flight::Flight::Tool { .. }
        ));
        assert_eq!(
            panel_read_25s.agy_flights[1],
            aterm_agent::harness::flight::Flight::Model,
            "parentheses inside the accepted name do not become part of the model activity"
        );
        track.fold_flown(
            &panel_read_25s.panel,
            &[aterm_agent::harness::agents::Tint::Plain; aterm_phase::agents_panel::PANEL_ROWS_MAX],
            &panel_read_25s.agy_flights,
            25_000,
        );
        assert_eq!(
            track.dots()[0].state,
            aterm_agent::harness::agents::DotState::Tool,
            "agy tool subagent stays in DotState::Tool (sat=1.0) past QUIET_MS instead of fading to Quiet"
        );
        assert_eq!(
            track.dots()[1].state,
            aterm_agent::harness::agents::DotState::Running
        );
        assert_eq!(track.dots()[0].sat, 1.0);
        assert_eq!(
            track.dots()[0].waits.as_ref().map(|w| w.tool.as_str()),
            Some("Reading file")
        );
        for palette in [
            aterm_effects::harness_card::CardPalette::Cruise,
            aterm_effects::harness_card::CardPalette::Land,
            aterm_effects::harness_card::CardPalette::Off,
        ] {
            let (ink, halo) = crate::claude_panel::look_for_test(
                &panel_read_25s,
                &track,
                palette,
                pane_geom,
                blank,
            );
            assert!(
                ink.iter().any(|c| c.row == 7 && c.col == 2),
                "bullet dot at col 2 is inked under {palette:?}"
            );
            assert!(
                !ink.iter()
                    .any(|c| c.row == 7 && name_range.contains(&usize::from(c.col))),
                "make_look must not emit InkCells over agy rainbow name columns under {palette:?}: {ink:?}"
            );
            assert!(
                halo.iter().any(|h| h.row == 7),
                "subagent bullet dot emits halo light quads under {palette:?}"
            );
        }
        let inks = MarkdownInks::of(
            blank,
            RenderCell {
                ch: '\u{2500}',
                fg: [0x58, 0x5c, 0x68],
                ..blank
            },
            [0xe0, 0x7a, 0x5f],
        );
        let nested_panel = render_agy_panel_item_row(
            "  ● Agent(self: Researcher (GUI))  Reading file (foo.rs) \u{00B7} 25s",
            0,
            scratch.cols,
            scratch.cols,
            blank,
            &inks,
        )
        .expect("parenthesized role in agy panel row should render");
        let nested_panel_text: String = nested_panel.iter().map(|c| c.ch).collect();
        assert!(
            nested_panel_text.contains("Researcher (GUI)  Reading file (foo.rs) \u{00B7} 25s"),
            "parenthesized role unwrapped cleanly in panel row: {nested_panel_text:?}"
        );
        let nested_tx = render_agy_subagent_transcript_row(
            "  └ Agent(self: Researcher (GUI)): Reading file (foo.rs)... \u{00B7} 25s",
            0,
            scratch.cols,
            scratch.cols,
            blank,
            &inks,
        )
        .expect("parenthesized role in agy transcript row should render");
        let nested_tx_text: String = nested_tx.iter().map(|c| c.ch).collect();
        assert!(
            nested_tx_text.contains("Researcher (GUI)  Reading file (foo.rs) \u{00B7} 25s"),
            "parenthesized role unwrapped cleanly in transcript row: {nested_tx_text:?}"
        );

        // Verify `undo` restores all original cells cleanly.
        assert!(undo(&mut undo_slot, &mut scratch));
        assert_eq!(scratch.cells, original);
    }

    #[test]
    fn agy_usage_card_weekly_places_and_expires_iso_reset_clock() {
        let rule = "\u{2500}".repeat(80);
        let rows: Vec<String> = vec![
            "● /usage".into(),
            "  └ Gemini Models".into(),
            "    Weekly Limit Remaining                 ████████░░  78%    2026-10-29 09:31 PDT"
                .into(),
            "  Claude and GPT models".into(),
            "    Weekly Limit Remaining                 ██████████  100%   2026-10-31 00:51 PDT"
                .into(),
            rule.clone(),
            "> ".into(),
            rule,
            "  ? for shortcuts                                        Gemini 3.1 Pro \u{00B7} high"
                .into(),
        ];
        let reading = aterm_phase::agy::usage_card_weekly(&rows).expect("agy /usage weekly");
        assert_eq!(reading.text, "weekly 78% left");
        assert_eq!(reading.reset.as_deref(), Some("2026-10-29 09:31 PDT"));

        let mut scratch = RenderInput::empty();
        scratch.rows = rows.len();
        scratch.cols = rows.iter().map(String::len).max().expect("rows");
        scratch.cells = rows.iter().map(|row| row_of(row, [90, 90, 90])).collect();
        scratch.snapshot_seq = 1;
        let region = PaneTextRegion {
            strip: 0,
            row_off: 0,
            col_off: 0,
            rows: scratch.rows,
            cols: scratch.cols,
        };
        let mut cache = PaneTextCache::default();
        let original = cache.rows(0, 7, &scratch, region);
        for (model, words, reset) in [
            (
                Some("Gemini 3.1 Pro"),
                "weekly 78% left",
                "2026-10-29 09:31 PDT",
            ),
            (
                Some("Claude Opus 4.6"),
                "weekly 100% left",
                "2026-10-31 00:51 PDT",
            ),
            (
                Some("Gemini 3.1 Pro"),
                "weekly 78% left",
                "2026-10-29 09:31 PDT",
            ),
            (
                Some("Claude Opus 4.6"),
                "weekly 100% left",
                "2026-10-31 00:51 PDT",
            ),
            (None, "weekly 78% left", "2026-10-29 09:31 PDT"),
        ] {
            cache.note_facts(
                0,
                7,
                &FooterFacts {
                    model: model.map(str::to_owned),
                    ..FooterFacts::default()
                },
                None,
                true,
                FactMask::default(),
            );
            assert_eq!(
                cache.usage_weekly(0, Some(aterm_phase::Program::Agy)),
                Some((words, Some(reset))),
                "unchanged quota rows follow the current model: {model:?}"
            );
            assert!(
                Arc::ptr_eq(&original, &cache.rows(0, 7, &scratch, region)),
                "a model refresh does not rebuild the unchanged screen rows"
            );
        }

        fn pdt_offset(zone: Option<&str>, _unix: i64) -> Option<i64> {
            match zone {
                None | Some("America/Los_Angeles") => Some(-7 * 3600),
                Some("UTC") => Some(0),
                _ => None,
            }
        }
        // 2026-10-29 09:31 PDT = 2026-10-29 16:31:00 UTC = 1_793_291_460.
        let reset_unix = 1_793_291_460;
        let before = || footer::ReadClock {
            read_at: std::time::UNIX_EPOCH,
            now: reset_unix - 3600,
            offset_at: &pdt_offset,
        };
        let (weekly, reset) =
            muse_weekly_observation((&reading.text, reading.reset.as_deref()), None, &before);
        assert_eq!(weekly, Some("weekly 78% left"));
        assert_eq!(reset, Some(("2026-10-29 09:31 PDT", Some(reset_unix))));

        let after = || footer::ReadClock {
            read_at: std::time::UNIX_EPOCH,
            now: reset_unix + 60,
            offset_at: &pdt_offset,
        };
        let (expired_weekly, expired_reset) =
            muse_weekly_observation((&reading.text, reading.reset.as_deref()), None, &after);
        assert_eq!(expired_weekly, None);
        assert_eq!(
            expired_reset,
            Some(("2026-10-29 09:31 PDT", Some(reset_unix)))
        );
    }

    #[test]
    fn agy_resolver_uses_process_presence_and_keeps_task_fallback() {
        const PID: i32 = 4299;
        thread_local! {
            static HOME_DIR: std::cell::RefCell<PathBuf> = const { std::cell::RefCell::new(PathBuf::new()) };
            static PROC_ENV: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
            static FILE_READS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
            static PROC_FILES: std::cell::RefCell<Vec<PathBuf>> = const { std::cell::RefCell::new(Vec::new()) };
        }
        fn test_home() -> Option<PathBuf> {
            Some(HOME_DIR.with(|h| h.borrow().clone()))
        }
        fn test_files(pid: u32) -> Option<Vec<PathBuf>> {
            assert_eq!(pid, u32::try_from(PID).unwrap());
            FILE_READS.with(|reads| {
                assert_eq!(
                    reads.replace(1),
                    0,
                    "resume and excerpts share one observation"
                );
            });
            Some(PROC_FILES.with(|files| files.borrow().clone()))
        }
        fn test_args(_: i32) -> Option<atpkg::caller_shell::ProcArgs> {
            Some(atpkg::caller_shell::ProcArgs {
                argv: vec!["agy".to_owned()],
                env: PROC_ENV.with(|e| e.borrow().clone()),
                ..Default::default()
            })
        }

        let dir = aterm_tempfile::Builder::new()
            .prefix("aterm-agy-footer-")
            .tempdir()
            .expect("private agy home");
        let home = dir.path().join("home");
        let cfg = home.join(".gemini/antigravity-cli");
        std::fs::create_dir_all(&cfg).unwrap();
        std::fs::write(
            cfg.join("settings.json"),
            br#"{"model":"Gemini 4 Argon (High)"}"#,
        )
        .unwrap();
        std::fs::write(
            cfg.join("cli.log"),
            b"I1023 17:50:20.000000 1 server_oauth.go:224] applyAuthResult: email=dev@example.com, authMethod=consumer\n",
        )
        .unwrap();
        HOME_DIR.with(|h| *h.borrow_mut() = home.clone());

        let run_with_env = |extra_env: Option<&str>| {
            let mut env = vec![
                format!("HOME={}", home.display()),
                format!("AGY_CLI_CONFIG_DIR={}", cfg.display()),
            ];
            if let Some(extra) = extra_env {
                env.push(extra.to_owned());
            }
            PROC_ENV.with(|e| *e.borrow_mut() = env);
            FILE_READS.with(|reads| reads.set(0));

            let timeline = Arc::new(Mutex::new(SessionTimeline::default()));
            {
                let mut held = timeline.lock().unwrap();
                held.note_foreground_group(PID);
                held.set_program(PID, Some("agy".to_owned()));
            }
            let mut identity = unread(PID, &home.join("no-claude"));
            identity.dir = None;
            resolve_facts_at_with_cwd(
                &timeline,
                &mut identity,
                &mut footer::TailCache::default(),
                &mut footer::FooterCache::default(),
                &mut RosterMemo::default(),
                (
                    Asks {
                        files: test_files,
                        tap: NO_TAP,
                        args: test_args,
                        roster: || None,
                        home: test_home,
                        stored_login: |_, _, _| false,
                    },
                    |_| None,
                ),
                &footer::ReadClock {
                    read_at: std::time::UNIX_EPOCH,
                    now: 1_792_000_000,
                    offset_at: &|_, _| Some(0),
                },
            );
            let held = timeline.lock().unwrap();
            (held.claude_footer_shown(), held.claude_excerpts().cloned())
        };

        let (shown, excerpts) = run_with_env(None);
        let shown = shown.expect("agy footer facts");
        assert_eq!(shown.model.as_deref(), Some("Gemini 4 Argon"));
        assert_eq!(shown.effort.as_deref(), Some("high"));
        assert_eq!(
            shown.subscription.as_ref().and_then(|s| s.email.as_deref()),
            Some("dev@example.com")
        );
        assert!(
            excerpts.is_none(),
            "interactive agy session without transcript or task leaves excerpts None so survey falls back to screen rows"
        );

        let tasks_dir = crate::task_record::tasks_dir().expect("tasks_dir");
        std::fs::create_dir_all(&tasks_dir).unwrap();
        let task_file = tasks_dir.join(format!("test-agy-footer-{}.task", std::process::id()));
        std::fs::write(&task_file, b"fix the footer\n").unwrap();
        let task_env = format!("{}={}", crate::survey::TASK_FILE_ENV, task_file.display());
        let (_, tasked_excerpts) = run_with_env(Some(&task_env));
        let _ = std::fs::remove_file(&task_file);
        assert_eq!(
            tasked_excerpts.and_then(|e| e.task),
            Some("fix the footer".to_owned())
        );

        let cid = "dfaaf1c5-98ff-4412-9226-76412af952a5";
        let logs_dir = cfg.join("brain").join(cid).join(".system_generated/logs");
        std::fs::create_dir_all(&logs_dir).unwrap();
        let transcript_bytes = br#"{"step_index":0,"source":"USER_EXPLICIT","type":"USER_INPUT","status":"DONE","content":"<USER_REQUEST>\npopulate agy survey\n</USER_REQUEST>"}
{"step_index":1,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","content":"Populated agy survey excerpts.","tool_calls":[{"name":"run_command","args":{}}]}
"#;
        std::fs::write(logs_dir.join("transcript.jsonl"), transcript_bytes).unwrap();
        std::fs::write(
            cfg.join("history.jsonl"),
            format!(
                r#"{{"display":"populate agy survey","timestamp":1,"workspace":"{}","conversationId":"{cid}"}}"#,
                home.display()
            ),
        )
        .unwrap();
        let (unheld_shown, unheld_excerpts) = run_with_env(None);
        assert!(
            unheld_excerpts.is_none() && unheld_shown.unwrap().resume.is_none(),
            "a transcript on disk is not process ownership"
        );
        let lock = aterm_agent::harness::upgrade_agy::presence_lock(&cfg, cid);
        std::fs::create_dir_all(lock.parent().unwrap()).unwrap();
        std::fs::write(&lock, b"").unwrap();
        PROC_FILES.with(|files| {
            *files.borrow_mut() = vec![std::fs::canonicalize(&lock).unwrap()];
        });
        let (resumed_shown, transcript_excerpts) = run_with_env(None);
        let resumed_shown = resumed_shown.expect("agy footer facts with held transcript");
        assert!(
            resumed_shown
                .resume
                .as_deref()
                .is_some_and(|r| r.ends_with(&format!("agy --conversation={cid}")))
        );
        let transcript_excerpts = transcript_excerpts.expect("agy transcript excerpts");
        assert_eq!(
            transcript_excerpts.transcript_len,
            Some(transcript_bytes.len() as u64)
        );
        assert_eq!(
            transcript_excerpts.excerpts.prompts[0].text,
            "populate agy survey"
        );
        assert_eq!(
            transcript_excerpts.excerpts.said.as_deref(),
            Some("Populated agy survey excerpts.")
        );
        assert_eq!(
            transcript_excerpts.excerpts.doing.as_deref(),
            Some("run_command")
        );

        let mut switch_tl = SessionTimeline::default();
        switch_tl.note_foreground_group(PID);
        switch_tl.set_program(PID, Some("agy".to_owned()));
        let mut first_facts = resumed_shown.clone();
        first_facts.subscription = Some(aterm_agent::harness::profile::SubscriptionDisplay {
            email: Some("dev@example.com".to_owned()),
            weekly: Some("weekly 78% left".to_owned()),
            weekly_figure: None,
            ..Default::default()
        });
        assert!(switch_tl.set_claude_footer(PID, None, Some(first_facts.clone())));
        assert!(!switch_tl.muse_cards_need_account());

        let mut switched_anon = first_facts.clone();
        switched_anon.subscription.as_mut().unwrap().email = Some("second@example.com".to_owned());
        assert!(switch_tl.set_claude_footer(PID, None, Some(switched_anon)));
        assert!(switch_tl.muse_cards_need_account());
        assert_eq!(
            switch_tl
                .claude_footer()
                .and_then(|f| f.subscription.as_ref()?.weekly.as_deref()),
            None,
            "anonymous screen weekly from the previous agy account is cleared on switch"
        );

        let fig = aterm_agent::harness::profile::parse_agy_usage_json(
            br#"{"status":"SUCCESS","command":{"name":"usage","data":{"groups":[{"name":"Gemini Models","buckets":[{"id":"gemini-weekly","name":"Weekly Limit Remaining","window":"weekly","remaining_fraction":0.91,"reset_time":"2026-10-29T16:31:22Z"}]}]}}}"#,
            false,
            1_792_000_000,
        )
        .expect("live figure");
        let mut switched_live = first_facts;
        switched_live.subscription = Some(aterm_agent::harness::profile::SubscriptionDisplay {
            email: Some("second@example.com".to_owned()),
            weekly: fig.words(1_792_000_000),
            weekly_figure: Some(fig),
            ..Default::default()
        });
        assert!(switch_tl.set_claude_footer(PID, None, Some(switched_live)));
        assert_eq!(
            switch_tl
                .claude_footer()
                .and_then(|f| f.subscription.as_ref()?.weekly.as_deref()),
            Some("weekly 91% left"),
            "live WeeklyFigure for the new agy account is preserved after switch"
        );
        PROC_FILES.with(|files| files.borrow_mut().clear());
        let (released_shown, released_excerpts) = run_with_env(None);
        assert!(
            released_excerpts.is_none() && released_shown.unwrap().resume.is_none(),
            "released presence is not cached ownership"
        );
    }

    #[test]
    fn muse_resolver_populates_model_effort_resume_and_excerpts_from_session_log() {
        const PID: i32 = 4399;
        thread_local! {
            static HOME_DIR: std::cell::RefCell<PathBuf> = const { std::cell::RefCell::new(PathBuf::new()) };
            static PROC_FILES: std::cell::RefCell<Vec<PathBuf>> = const { std::cell::RefCell::new(Vec::new()) };
            static PROC_ENV: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
        }
        fn test_files(_: u32) -> Option<Vec<PathBuf>> {
            Some(PROC_FILES.with(|f| f.borrow().clone()))
        }
        fn test_home() -> Option<PathBuf> {
            Some(HOME_DIR.with(|h| h.borrow().clone()))
        }
        fn test_args(_: i32) -> Option<atpkg::caller_shell::ProcArgs> {
            Some(atpkg::caller_shell::ProcArgs {
                argv: vec!["muse".to_owned()],
                env: PROC_ENV.with(|e| e.borrow().clone()),
                ..Default::default()
            })
        }

        let scratch = aterm_tempfile::TempDir::new().unwrap();
        let (root, _) = super::rig::claude_home_at(
            scratch.path().join("muse-footer-resolver"),
            PID,
            1_790_607_848,
        );
        let home = root.join("home");
        let cfg = home.join(".config/muse");
        std::fs::create_dir_all(&cfg).unwrap();
        std::fs::write(
            cfg.join("settings.json"),
            br#"{"model":"Muse Spark (High)"}"#,
        )
        .unwrap();
        std::fs::write(
            cfg.join("auth.json"),
            br#"{"providers":{"meta":{"mechanism":"oauth","user_email":"muse@example.com"}}}"#,
        )
        .unwrap();
        let sid = "01a16480-f908-7d10-a78e-5d95372dfb98";
        let sdir = home.join(".local/share/muse/sessions/2026/10/23").join(sid);
        std::fs::create_dir_all(&sdir).unwrap();
        std::fs::write(sdir.join(".session.lock"), format!("pid={PID}\nhost=mac\n")).unwrap();
        let session_log = br#"{"payload_type":"runtime.session","payload":{"kind":"run","run_id":"r1","event":{"kind":"started","prompt":"inspect muse footer"}}}
{"payload_type":"runtime.session","payload":{"kind":"run","run_id":"r1","event":{"kind":"assistant_message_committed","text":"Muse footer and survey excerpts ready."}}}
{"payload_type":"runtime.session","payload":{"kind":"task","run_id":"r1","task_id":"t1","event":{"kind":"proposed","task_kind":"tool.run_command"}}}
"#;
        std::fs::write(sdir.join("session.jsonl"), session_log).unwrap();

        HOME_DIR.with(|h| *h.borrow_mut() = home.clone());
        PROC_FILES.with(|f| {
            *f.borrow_mut() = vec![std::fs::canonicalize(sdir.join(".session.lock")).unwrap()]
        });
        PROC_ENV.with(|e| *e.borrow_mut() = vec![format!("HOME={}", home.display())]);

        let timeline = Arc::new(Mutex::new(SessionTimeline::default()));
        {
            let mut held = timeline.lock().unwrap();
            held.note_foreground_group(PID);
            held.set_program(PID, Some("muse".to_owned()));
        }
        // Even if identity.dir points to a directory with a synthetic `<PID>.json` for Claude,
        // `uses_claude_registry` is false for `Program::Muse` so it reads Muse's own facts.
        let mut identity = unread(PID, &root.join("claude"));
        resolve_facts_at_with_cwd(
            &timeline,
            &mut identity,
            &mut footer::TailCache::default(),
            &mut footer::FooterCache::default(),
            &mut RosterMemo::default(),
            (
                Asks {
                    files: test_files,
                    tap: NO_TAP,
                    args: test_args,
                    roster: || None,
                    home: test_home,
                    stored_login: |_, _, _| false,
                },
                |_| Some(home.clone()),
            ),
            &footer::ReadClock {
                read_at: std::time::UNIX_EPOCH,
                now: 1_792_000_000,
                offset_at: &|_, _| Some(0),
            },
        );
        let held = timeline.lock().unwrap();
        let shown = held.claude_footer_shown().expect("muse footer facts");
        assert_eq!(shown.model.as_deref(), Some("Muse Spark"));
        assert_eq!(shown.effort.as_deref(), Some("high"));
        assert_eq!(
            shown.subscription.as_ref().and_then(|s| s.email.as_deref()),
            Some("muse@example.com")
        );
        assert_eq!(
            shown.resume.as_deref(),
            Some(format!("muse resume {sid}").as_str())
        );
        let excerpts = held.claude_excerpts().expect("muse excerpts");
        assert_eq!(excerpts.transcript_len, Some(session_log.len() as u64));
        assert_eq!(excerpts.excerpts.prompts[0].text, "inspect muse footer");
        assert_eq!(
            excerpts.excerpts.said.as_deref(),
            Some("Muse footer and survey excerpts ready.")
        );
        assert_eq!(excerpts.excerpts.doing.as_deref(), Some("run_command"));
        drop(held);
        let _ = std::fs::remove_dir_all(&root);
    }
}
