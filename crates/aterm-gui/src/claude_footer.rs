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
use aterm_agent::harness::observe::{self, Observation};
use aterm_agent::harness::profile::{
    SubscriptionDisplay, USAGE_SHOWN_S, WEEKLY_OUT, WeeklyFigure, WeeklyGap,
};
use aterm_agent::harness::tap;
use aterm_core::render::RenderInput;
use aterm_core::terminal::RenderCell;
use aterm_effects::cli_marks::{Build, CliMark};
use aterm_effects::glass_inks::{GlassInks, HoverPart};
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
            | aterm_phase::Program::Muse
            | aterm_phase::Program::Kimi => true,
            aterm_phase::Program::Codex
            | aterm_phase::Program::Grok
            | aterm_phase::Program::Generic => false,
        })
}

/// The line of a program's composer that carries aterm's Autopilot chip ([`chip_line`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChipLine {
    /// The composer's bottom rule, beside the footer's facts ([`drawn_for`]).
    BottomRule,
}

/// The line `program`'s composer lays the Autopilot chip on, `None` for a program aterm draws no
/// chip for.
pub(crate) const fn chip_line(program: aterm_phase::Program) -> Option<ChipLine> {
    match program {
        aterm_phase::Program::Claude
        | aterm_phase::Program::Agy
        | aterm_phase::Program::Muse
        | aterm_phase::Program::Kimi => Some(ChipLine::BottomRule),
        aterm_phase::Program::Codex
        | aterm_phase::Program::Grok
        | aterm_phase::Program::Generic => None,
    }
}

/// Whether a composer is left bare but for the top rule's switch under `switch`: while the master
/// switch or its agent's own holds the harness off, or a click turning it on has not read back
/// ([`crate::autopilot::HarnessSwitch::is_off`]), every program's composer is the program's own
/// but for the switch, drawn off and pressable to turn the harness on. No lockup, label goal,
/// top-effort key, card or banner, border palette, line D (facts, lights, chips), line E,
/// agents panel, mode-row rewrite or transcript restyling (2026-11-09, owner: the switch off
/// disables all harness UX for the pane).
pub(crate) fn bare_while_off(switch: Option<crate::autopilot::HarnessSwitch>) -> bool {
    switch.is_some_and(crate::autopilot::HarnessSwitch::is_off)
}

/// Whether the top rule's switch stands in `agent`'s composer: Claude Code's, agy's, Muse
/// Code's and Kimi Code's top rules carry it ([`App::splice_top_line`]); a program the timeline
/// does not name has no top rule aterm reads.
pub(crate) const fn switch_stands(agent: Option<aterm_phase::Program>) -> bool {
    matches!(
        agent,
        Some(
            aterm_phase::Program::Claude
                | aterm_phase::Program::Agy
                | aterm_phase::Program::Muse
                | aterm_phase::Program::Kimi
        )
    )
}

impl App {
    /// Whether `agent`'s composer under `view` is left bare of the harness: where its switch
    /// stands ([`switch_stands`]) and holds the harness off ([`bare_while_off`]), but for the
    /// switch; and, with no host, where the policy supervises nothing for `agent` (`[harness]
    /// enabled = false`, or its own switch off): no host can turn it on, so nothing of the
    /// harness is drawn, the switch included. A pane whose switch cannot stand keeps line D's
    /// `⊘ off` chip as its way back (`claude_lights`' `press_autopilot_chip`).
    pub(crate) fn pane_bare(
        &self,
        view: crate::autopilot::AutopilotView,
        agent: Option<aterm_phase::Program>,
    ) -> bool {
        (switch_stands(agent) && bare_while_off(crate::autopilot::HarnessSwitch::of(view)))
            || self.unhosted_off(view, agent)
    }

    /// With no host, the policy supervises nothing for `agent` ([`Self::pane_bare`]).
    fn unhosted_off(
        &self,
        view: crate::autopilot::AutopilotView,
        agent: Option<aterm_phase::Program>,
    ) -> bool {
        view == crate::autopilot::AutopilotView::Unhosted && !self.unhosted_glass(agent)
    }
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

/// What one pane's top line is spliced with ([`App::splice_top_line`]).
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
    /// With no host the policy supervises nothing here ([`App::pane_bare`]): no lockup.
    unhosted_off: bool,
    palette: Option<aterm_effects::harness_card::CardPalette>,
    cell: (u16, u16),
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
    /// Panes whose composer is on the glass with no plain rule glyph to write into
    /// (`(session, term_row)`).
    bare: Vec<(u64, usize)>,
    /// The rules' compositions, written as [`rule_edits`].
    rules: Vec<RuleWrite>,
    /// agy's glass-only transcript markdown edits.
    agy_edits: Vec<AgyRowEdit>,
    /// Claude's mode rows rewritten to its jobs ([`collect_mode_row`]).
    mode_rows: Vec<RowWrite>,
    mode_cells: Vec<RenderCell>,
    /// The quota rows this frame lays on line E ([`quota::QuotaWrite`]).
    quota_rows: Vec<quota::QuotaWrite>,
    /// The composer top rules' compositions ([`ComposedTop`]).
    tops: Vec<TopWrite>,
    /// The top-effort key's targets each top rule would put on the glass ([`TopHit`]).
    top_hits: Vec<TopHit>,
    /// Sessions whose top rule showed Claude's top-effort tag for a process not yet noted as having
    /// shown it.
    top_seen: Vec<(u64, i32)>,
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
        self.mode_rows.clear();
        self.mode_cells.clear();
        self.quota_rows.clear();
        self.tops.clear();
        self.top_hits.clear();
        self.top_seen.clear();
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
    /// The palette the glass wears on the pane ([`crate::autopilot::harness_theme`]), `None`
    /// with no harness theme.
    palette: Option<aterm_effects::harness_card::CardPalette>,
    /// The pane's width, across which each cell takes its column's hue
    /// ([`aterm_effects::glass_inks::field`]).
    cols: usize,
}

/// A visible Claude Code pane and the lights' reading of its rows.
struct SeenPane {
    leaf_index: usize,
    session: u64,
    lights: LightsRead,
    /// The tab's standing permission click as the App's table holds it this frame.
    click: Option<crate::permission_click::ClickShown>,
    /// What a click's restart would end of Claude's running jobs (`1 shell`,
    /// [`footer::ModeRow::job_words`]), read only while the mode light's title shows: under the
    /// pointer or the keyboard, or while the tab's click stands; empty otherwise.
    jobs: String,
    switch: Option<crate::autopilot::HarnessSwitch>,
    autopilot: Option<(
        Option<aterm_agent::supervise::Autopilot>,
        crate::autopilot::AutopilotView,
    )>,
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
    /// The top rules' ⬆️ ring contents, shared by the leaves ([`RingContents`]).
    rings: RingContents,
    /// Each live session's grace on line E ([`quota::Grace`]), by session: it outlives the leaf's
    /// footer, which a tab switch, a scroll back or a closed split replaces.
    graces: HashMap<u64, quota::Grace>,
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
    /// The harness card's reading of `rows`.
    card: std::cell::OnceCell<crate::claude_card::CardReading>,
    /// The last reading's card, until this rebuild's reading is taken.
    card_before: std::cell::Cell<Option<crate::claude_card::CardBefore>>,
    /// The agents panel's reading of `rows` and the frame they were read from
    /// ([`PaneTextCache::agents_panel`]).
    panel: std::cell::OnceCell<Option<Arc<crate::claude_panel::PanelRead>>>,
    /// The last reading's panel, until this rebuild's is taken.
    panel_before: std::cell::Cell<Option<Arc<crate::claude_panel::PanelRead>>>,
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

/// A composer top rule's plain run read off frame row `frame_row` for pane ground `bg` ([`CachedPaneText::top_run`]).
#[derive(Debug, Clone, Copy)]
struct KeptTop {
    frame_row: usize,
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
    /// Claude's mode row under the bottom rule ([`collect_mode_row`]).
    mode: Option<footer::ModeRow>,
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
                aterm_phase::phase::FrameReach::of_or_hosted(rows)
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
            aterm_phase::phase::FrameReach::of_or_hosted(rows)
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
    let mode = rule.and_then(|read| {
        #[cfg(test)]
        crate::work_counts::mode_row_read();
        crate::reader_guard::read_or_none(session, "Claude Code mode row", rows, || {
            footer::mode_row_rewrite_under(rows, read.rule)
        })
        .flatten()
    });
    Readings {
        reach,
        rule,
        lights,
        mode,
    }
}

impl PaneTextCache {
    /// A removed leaf must not retain a transcript.
    fn trim(&mut self, leaves: usize) {
        self.panes.truncate(leaves);
        self.footers.truncate(leaves);
    }

    /// A pane that no longer shows a Claude footer must not keep its old transcript resident just
    /// because another pane still has one; a session gone (`live` false) keeps no grace.
    fn retain_active(
        &mut self,
        active: impl Fn(usize) -> bool,
        card: impl Fn(usize) -> bool,
        live: impl Fn(u64) -> bool,
    ) {
        self.graces.retain(|&session, _| live(session));
        for (index, cached) in self.panes.iter_mut().enumerate() {
            if !active(index) && !card(index) {
                *cached = None;
            }
        }
        for (index, footer) in self.footers.iter_mut().enumerate() {
            if active(index) {
                continue;
            }
            if footer.from.is_some() || footer.rule.key.is_some() || footer.top.key.is_some() {
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
            card,
            card_before: std::cell::Cell::new(card_before),
            panel: std::cell::OnceCell::new(),
            panel_before: std::cell::Cell::new(panel_before),
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
                || aterm_phase::phase::context_left(&cached.rows),
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
    fn top_run(
        &self,
        leaf_index: usize,
        (session, program): (u64, aterm_phase::Program),
        scratch: &RenderInput,
        region: PaneTextRegion,
        top_rule: usize,
        blank: RenderCell,
    ) -> (Option<TopRun>, Option<(usize, usize)>) {
        let frame_row = region.strip + region.row_off + top_rule;
        let muse = program == aterm_phase::Program::Muse;
        let Some(Some(cached)) = self.panes.get(leaf_index) else {
            return (read_top_run(scratch, region, top_rule, blank, muse), None);
        };
        match cached.top_run.get() {
            Some(kept) if kept.frame_row == frame_row && kept.bg == blank.bg => {
                (kept.run, kept.tag)
            }
            _ => {
                let run = read_top_run(scratch, region, top_rule, blank, muse);
                let tag = crate::reader_guard::read_or_none(
                    session,
                    "Claude Code top rule",
                    &cached.rows,
                    || footer::top_rule_tag(cached.rows.get(top_rule)?),
                )
                .flatten()
                .map(|(a, b)| (region.col_off + a, region.col_off + b));
                let mut noted = cached.kept_cells.borrow_mut();
                if cells_vouch(scratch) {
                    let span = region.col_off..region.col_off + region.cols;
                    noted.top.note(scratch, frame_row, &span);
                } else {
                    noted.top.forget();
                }
                cached.top_run.set(Some(KeptTop {
                    frame_row,
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
    fn top_line(&self, leaf_index: usize, program: aterm_phase::Program) -> Option<usize> {
        top_line(program, self.panes.get(leaf_index)?.as_ref()?)
    }

    /// Leaf `leaf_index`'s top rule composed from `key` and the cells of Claude Code's label zone
    /// `tail` ([`PaneFooter::compose_top`]).
    fn compose_top(&mut self, leaf_index: usize, key: TopKey, tail: &[RenderCell]) -> &ComposedTop {
        let Self {
            footers,
            marks,
            rings,
            ..
        } = self;
        footers[leaf_index].compose_top(key, tail, marks, rings)
    }

    /// Leaf `leaf_index`'s composed top rule's sprites: its mark's ([`mark_layer`]) and, under the
    /// pointer, its ⬆️'s ring ([`ring_layer`]).
    fn top_layers(&self, leaf_index: usize) -> impl Iterator<Item = &aterm_core::render::FxLayer> {
        self.footers
            .get(leaf_index)
            .into_iter()
            .flat_map(|f| f.top.mark.iter().chain(f.top.ring.iter()))
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

    /// The cells of leaf `leaf_index`'s composed quota row ([`quota::QuotaRow::cells`]).
    fn quota_cells(&self, leaf_index: usize) -> &[RenderCell] {
        self.footers
            .get(leaf_index)
            .map_or(&[], |f| f.quota.cells())
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
}

/// One pane's footer as earlier frames composed it (a leaf's, in [`PaneTextCache`]).
#[derive(Debug, Default)]
struct PaneFooter {
    session: u64,
    /// What [`Self::shown`] is made of: the facts the timeline holds for the group in front, the
    /// launch card it keeps for them, the usage switch and the facts the person hid.
    from: Option<(FooterFacts, Option<aterm_phase::LaunchCard>, bool, FactMask)>,
    /// What the rule shows ([`with_usage_switch`]).
    shown: FooterFacts,
    /// Bumped on every change of `shown`.
    shown_gen: u64,
    /// Bumped on every change of the facts `shown` is made of, not on [`Self::line_e`]'s: the
    /// quota row's view is laid once per generation ([`quota::QuotaRow`]).
    view_gen: u64,
    /// Line E is the glass's, or a grace keeps line D's unit back ([`Self::note_line_e`]):
    /// `shown`'s [`FooterFacts::line_e`]. The grace is the session's ([`PaneTextCache::graces`]).
    line_e: bool,
    /// Line E as the frames before composed it.
    quota: quota::QuotaRow,
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
    /// The active goal the top line shows ([`Self::note_goal`]), and its [`goal_term`].
    goal: Option<(Box<str>, u64)>,
    /// The holder's token and the level its effort hint last named ([`Self::note_hint`]).
    hint: Option<(u64, &'static str)>,
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
    /// Whether `cells` end with the run's last cell, its close: a rule that wears the arc is
    /// written whole, its lead and its close among it ([`compose_rule_spans`]).
    close: bool,
    /// Where the facts' cells start in `cells` ([`FactWrite`]).
    facts_at: usize,
    spans: footer::FactSpans,
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
    /// The glass's tokens on the pane ([`GlassInks`]).
    glass: GlassInks,
    /// Where the rule sits across its pane.
    at: RuleAt,
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
        shown.line_e = self.line_e;
        let model_changed = shown.model != self.shown.model;
        self.shown = shown;
        self.shown_gen += 1;
        self.view_gen += 1;
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

    /// Take in the active goal the timeline holds for the pane's agent
    /// ([`crate::session_timeline::SessionTimeline::goal`]).
    fn note_goal(&mut self, goal: Option<&str>) {
        if self.goal.as_ref().map(|(text, _)| &**text) != goal {
            self.goal = goal.map(|text| (Box::from(text), goal_term(Some(text))));
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

    /// Keep `level`, the effort Claude's own hint names on a row the holder `token` drew: an open
    /// effort takes it while that holder runs, the hint on the screen or gone, so facts published
    /// only after Claude dropped its hint (a slow first launch) still take it.
    const fn note_hint(&mut self, token: u64, level: &'static str) {
        self.hint = Some((token, level));
    }

    /// The level [`Self::note_hint`] kept for holder `token`.
    fn hint(&self, token: u64) -> Option<&'static str> {
        self.hint
            .filter(|(kept, _)| *kept == token)
            .map(|(_, level)| level)
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

    /// The composer's top line composed from `key` and the cells of Claude Code's label zone
    /// `tail` ([`compose_top`], [`mark_layer`]).
    fn compose_top(
        &mut self,
        key: TopKey,
        tail: &[RenderCell],
        marks: &mut MarkContents,
        rings: &mut RingContents,
    ) -> &ComposedTop {
        let hide_goal = key
            .switch
            .is_some_and(|s| s.harness == Some(aterm_phase::CliHarness::Agy) && s.face.is_off());
        let goal = if hide_goal {
            0
        } else {
            self.goal.as_ref().map_or(0, |(_, term)| *term)
        };
        if self.top.key != Some(key) || self.top.goal != goal {
            let text = (!hide_goal)
                .then_some(self.goal.as_ref())
                .flatten()
                .map(|(text, _)| &**text);
            compose_top(&key, &mut self.top, text, tail);
            self.top.mark = mark_layer(&key, &self.top, self.session, marks);
            self.top.ring = ring_layer(&key, &self.top, self.session, rings);
            self.top.key = Some(key);
            self.top.goal = goal;
        }
        &self.top
    }

    /// Whether the rule of `room` cells spares the cells `block`'s mode name takes
    /// ([`crate::claude_lights::Block::word`]): with the name it lays every fact it lays without
    /// it, and a title where it lays one, a narrower form of it at most (the title gives way
    /// first, `footer::fit_rule`).
    fn spares(&self, room: usize, block: &crate::claude_lights::Block) -> bool {
        let widths = |chips: usize| footer::LightsWidth {
            chips,
            title: block.title.len(),
            lean: block.lean.each_ref().map(Vec::len),
            short: block.short_title.len(),
            reason: block.reason,
        };
        let rest = block.holds.saturating_sub(block.word);
        let without = footer::fit_rule(&self.shown, room, Some(widths(rest)));
        let with = footer::fit_rule(&self.shown, room, Some(widths(block.holds)));
        without.chips
            && with.chips
            && with.segments == without.segments
            && (with.title != footer::TitleFit::None || without.title == footer::TitleFit::None)
    }

    /// The rule's composition in `room` cells with the chips' `block` ([`compose_rule_spans`]).
    fn compose(
        &mut self,
        room: usize,
        block: Option<&crate::claude_lights::Block>,
        rule: RenderCell,
        blank: RenderCell,
        (glass, at): (GlassInks, RuleAt),
        lit: Option<FactKind>,
    ) -> Composed {
        let key = RuleKey {
            shown_gen: self.shown_gen,
            room,
            rule,
            blank,
            glass,
            at,
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
            composed.paint = compose_rule_spans(&fit, block, rule, blank, (&glass, at), room, lit);
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
            close: composed.paint.close,
            facts_at: composed.paint.facts_at,
            spans: composed.paint.spans,
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
    /// The cells end with the run's close ([`RulePaint::close`]).
    close: bool,
    facts_at: usize,
    spans: footer::FactSpans,
}

impl Composed {
    /// The pane column of the composition's first cell on a run ending at pane column `end`:
    /// right-aligned before the run's close, or through it where it is written.
    const fn start(&self, end: usize) -> usize {
        (end + self.close as usize).saturating_sub(1 + self.cells)
    }
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

    /// Wakes the loop for `session` ([`Wake::SessionLogMoved`]) if `applied` transcript steps
    /// changed footer-visible facts, moved the log's wake, or moved its harness pairing's read
    /// wait.
    pub(crate) fn after(&self, session: u64, applied: &crate::session_log::transcript::Applied) {
        if (applied.owed && self.clause) || applied.settle.is_some() || applied.waits {
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

struct FooterUseHost<'a> {
    identity: &'a Identity,
    argv: &'a [String],
    env: &'a [String],
}

impl aterm_agent::harness::provider_read::FooterHost for FooterUseHost<'_> {
    fn owns(&self, argv: &[String], env: &[String]) -> bool {
        self.identity.pid > 0 && self.argv == argv && self.env == env
    }
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
    /// How the process authenticates once settings, provider and profile-store evidence are read
    /// ([`crate::harness_netprobe::auth_route_of_agent`]): only they move it off its account's own
    /// credential.
    route: accounts::AuthRoute,
    /// Why no relaunch line of this image's argv in its directory would carry the usage tap,
    /// judged beside [`Self::route`] ([`tap_line_refusal`]: its options and settings;
    /// its home and login are judged at each read, [`tap_refusal`]); `None` where one would.
    unoffered: Option<tap::Refusal>,
    /// Exact captured exec environment, fingerprinted without retaining credentials.
    env_stamp: Option<[u8; 32]>,
    /// The login address the process's login file named at the first read of this image
    /// (`started`): a later read that names another says the login moved under the running
    /// process (another tab's `/login`), which the file's modification time cannot say, since
    /// Claude Code rewrites its home profile while it runs.
    first_seen: Option<(Option<u64>, String)>,
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
    /// The accounts outlook over it, replayed at most once a minute
    /// ([`crate::accounts_outlook::OutlookMemo`]).
    outlook: crate::accounts_outlook::OutlookMemo,
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

    /// The quota views of the pane whose account is `tab` at wall second `now`
    /// ([`crate::accounts_outlook::OutlookMemo::quota`]), over the roster [`Self::at`] gives.
    fn quota(
        &mut self,
        (instant, now): (std::time::Instant, i64),
        load: impl FnOnce() -> Option<accounts::Roster>,
        tab: &str,
        (own, movable): (Option<aterm_agent::harness::outlook::Figure>, bool),
    ) -> Option<crate::accounts_outlook::QuotaViews> {
        self.at(instant, load);
        self.outlook
            .quota((instant, now), self.roster.as_ref(), tab, own, movable)
    }
}

/// The roster beside the `aterm.toml` this window loads ([`accounts::default_roster_path`]).
fn live_roster() -> Option<accounts::Roster> {
    if cfg!(test) {
        return None;
    }
    roster_at(
        &accounts::default_roster_path()?,
        crate::harness_host::harness_state,
    )
}

/// The roster at `path` (`None` where it does not load): an unconfigured one as it reads, never
/// asking for the harness's `state`; a configured one bound to the logins kept there, or `None`
/// where it cannot be.
fn roster_at(
    path: &std::path::Path,
    state: impl FnOnce() -> Option<PathBuf>,
) -> Option<accounts::Roster> {
    let roster = accounts::Roster::load(path).ok()?;
    if roster.rows.is_empty() {
        return Some(roster);
    }
    let now = aterm_time::unix_secs().and_then(|now| i64::try_from(now).ok())?;
    accounts::Store::at(&state()?).bound(&roster, now).ok()
}

/// What a read asks outside Claude Code's own files.
#[derive(Clone, Copy)]
struct Asks {
    args: fn(i32) -> Option<atpkg::caller_shell::ProcArgs>,
    files: fn(u32) -> Option<Vec<PathBuf>>,
    roster: fn() -> Option<accounts::Roster>,
    home: fn() -> Option<PathBuf>,
    route: fn(u32, &[String], &str) -> accounts::AuthRoute,
    tap: TapAsks,
}

/// What a read asks of the usage tap ([`Asks::tap`]).
#[derive(Clone, Copy)]
struct TapAsks {
    /// The window's seat ([`TapSeat`]), asked at each read whose profile passed its gate, and
    /// at each read of a process that carries the tap, for its effort ([`own_tap_effort`]).
    seat: fn() -> TapSeat,
    /// Whether the settings chain a relaunch line of the process's argv in its directory reads
    /// admits the tap (`upgrade_drive::tap_settings_admit`), asked once per image beside
    /// [`Asks::route`].
    settings: fn(&[String], &str) -> bool,
    /// Whether aterm's restart in place is offered for process `pid` in tab `tab` now
    /// ([`crate::restart_act::offered_for`]), asked at a read whose footer would promise it.
    restart: fn(&str, u32) -> bool,
    /// Whether the relaunch line aterm would type for process `pid` in tab `tab` has room for
    /// the tap ([`crate::restart_act::tap_room_for`]), asked where a restart is offered.
    room: fn(&str, u32) -> bool,
}

/// A window with no usage tap: no seat, every settings chain admitting it, every restart offered
/// with room for the tap.
#[cfg(test)]
const NO_TAP: TapAsks = TapAsks {
    seat: TapSeat::default,
    settings: |_, _| true,
    restart: |_, _| true,
    room: |_, _| true,
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
    route: crate::harness_netprobe::auth_route_of_agent,
    tap: TapAsks {
        seat: live_tap,
        settings: crate::harness_netprobe::tap_settings_admit_line,
        restart: crate::restart_act::offered_for,
        room: crate::restart_act::tap_room_for,
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
        route: aterm_agent::harness::accounts::AuthRoute::Outside,
        unoffered: None,
        env_stamp: None,
        first_seen: None,
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
    // This build's model catalog, which the model card and the effort slider read, is read
    // here, off the main thread.
    let version = timeline
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .claude_footer_shown()
        .and_then(|facts| facts.version);
    crate::claude_facts::note_catalog(version.as_deref(), std::time::Instant::now());
    // The default permission mode of this agent's settings, which the permission chip's click
    // and box read, is read here too, off the main thread.
    if let Ok(pid) = u32::try_from(pgid) {
        crate::permission_click::note_default(pid, std::time::Instant::now());
    }
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

/// What a Claude Code process really runs on ([`observe::claude`]), read afresh from the login
/// file its own exec environment `args` selects (the pass's one read of it, already checked to be
/// this image's); unread where no home is known.
fn claude_observation(
    args: &atpkg::caller_shell::ProcArgs,
    asks: &Asks,
    route: accounts::AuthRoute,
    roster: Option<&accounts::Roster>,
) -> Observation {
    let Some(home) = (asks.home)() else {
        return Observation::Unread;
    };
    observe::claude(&args.env, &home, route, roster)
}

/// The account title's clause where the login file contradicts the roster's belief:
/// `believed m22 (brontosaurus…), observed unicorn… from ~/.claude.json`, and where the file named
/// another address when this image first read it (`first`), `; it named first@… when the
/// process started`: a login moved under the running process. No age: the clause is a fact of
/// the pass's read, and the footer's fingerprint hashes it, so it moves only when they do.
fn account_detail(
    shown: &accounts::FooterAccount,
    roster: &accounts::Roster,
    seen: &Observation,
    first: Option<&str>,
) -> Option<String> {
    let believed = shown.believed.as_deref()?;
    let observed = seen.seen()?;
    let named = roster
        .row(believed)
        .and_then(accounts::Row::expected_identity_email)
        .map_or_else(
            || believed.to_owned(),
            |email| format!("{believed} ({email})"),
        );
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let path = match home
        .as_deref()
        .and_then(|h| observed.path.strip_prefix(h).ok())
    {
        Some(rest) => format!("~/{}", rest.display()),
        None => observed.path.display().to_string(),
    };
    let moved = first
        .filter(|first| !accounts::email_matches(first, &observed.email))
        .map(|first| format!("; it named {first} when the process started"))
        .unwrap_or_default();
    Some(format!(
        "believed {named}, observed {} from {path}{moved}",
        observed.email
    ))
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
        Some(
            aterm_phase::Program::Agy | aterm_phase::Program::Muse | aterm_phase::Program::Kimi,
        ) => (false, false),
        Some(
            aterm_phase::Program::Codex
            | aterm_phase::Program::Grok
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
                    identity.route = accounts::AuthRoute::Outside;
                    if identity.launch_for == Some(entry.started_at) {
                        let asked = u32::try_from(identity.pid).ok().zip(entry.cwd.to_str());
                        identity.route = asked
                            .map_or(accounts::AuthRoute::Outside, |(pid, cwd)| {
                                (asks.route)(pid, &identity.argv, cwd)
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
                .filter(|p| {
                    matches!(
                        p,
                        aterm_phase::Program::Agy
                            | aterm_phase::Program::Muse
                            | aterm_phase::Program::Kimi
                    )
                });
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
                aterm_phase::Program::Kimi => "kimi",
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
            let mut goal = None;
            if prog == aterm_phase::Program::Muse
                && let (Some(args), Some(home)) = (args.as_ref(), home.as_deref())
            {
                match aterm_agent::harness::profile::muse_subscription_for(
                    &args.argv,
                    &args.env,
                    home,
                    &sub,
                    clock.now,
                    &FooterUseHost {
                        identity,
                        argv: &args.argv,
                        env: &args.env,
                    },
                ) {
                    Some(read) => sub = read,
                    // Its login store names no account now (signed out, an API key, another
                    // provider): the account the last pass showed is not this process's, and
                    // neither are its plan and week. A screen card's words that no account
                    // carried stand.
                    None if sub.email.is_some() => {
                        sub = aterm_agent::harness::profile::SubscriptionDisplay::default();
                    }
                    None => {}
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
                goal = aterm_agent::harness::profile::muse_goal_for(
                    muse_pid,
                    &args.argv,
                    &args.env,
                    home,
                    files.as_deref(),
                    fold.store_goal(),
                );
            }
            if prog == aterm_phase::Program::Agy
                && let (Some(args), Some(home)) = (args.as_ref(), home.as_deref())
            {
                // A turn this session served is what marks its login working in the account
                // state: evidence already on disk, never a probe.
                let served_at = aterm_agent::harness::profile::agy_served_at(
                    &args.argv,
                    &args.env,
                    home,
                    files.as_deref(),
                    identity.started,
                );
                if let Some(read) = aterm_agent::harness::profile::agy_subscription_in_dir(
                    &args.argv,
                    &args.env,
                    cwd.as_deref(),
                    home,
                    &sub,
                    prev_card_model.as_deref().or(model.as_deref()),
                    (clock.now, served_at),
                    &FooterUseHost {
                        identity,
                        argv: &args.argv,
                        env: &args.env,
                    },
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
                account: None,
                account_email: None,
                account_detail: None,
                route: None,
                account_progress: None,
                account_progress_busy: false,
                top_effort: None,
                off: footer::FactMask::default(),
                subscription: Some(sub),
                plan: None,
                context: prev_ctx,
                goal,
                quota: None,
                quota_observed: None,
                line_e: false,
            });
        }
    }
    // One read of the process's exec environment a pass, for its observed login and its
    // profile alike; another image's environment says nothing of this one.
    let args = if is_claude {
        (asks.args)(identity.pid).filter(|args| {
            args.argv == identity.argv && Some(exec_env_stamp(&args.env)) == identity.env_stamp
        })
    } else {
        None
    };
    // What Claude Code says its session's effort is, now, outranks what the transcript says the
    // turn's requests were sent with ([`own_tap_effort`]): the footer names the effort the
    // session is set to, as Claude Code's own `/effort` does.
    if let (Some(facts), Some(args)) = (facts.as_mut(), args.as_ref())
        && let Some(level) = own_tap_effort(identity, args, asks, clock.now)
    {
        facts.effort = Some(level);
        (facts.effort_open, facts.effort_for) = (false, None);
    }
    // Observation beats belief (`docs/DESIGN-account-discovery-2026-11-09.md`): the login file
    // this process reads names its account, read afresh on every pass.
    let seen = match (args.as_ref(), identity.login.as_ref()) {
        (Some(args), Some(who)) => claude_observation(
            args,
            &asks,
            who.route,
            roster.at(std::time::Instant::now(), asks.roster),
        ),
        _ => Observation::Unread,
    };
    // The address this image's login file named at its first read: a later one that differs
    // is a login that moved under the running process.
    if let Some(email) = seen.email()
        && identity
            .first_seen
            .as_ref()
            .is_none_or(|(started, _)| *started != identity.started)
    {
        identity.first_seen = Some((identity.started, email.to_owned()));
    }
    let first = identity
        .first_seen
        .as_ref()
        .filter(|(started, _)| *started == identity.started)
        .map(|(_, email)| email.clone());
    if let (Some(facts), Some(who)) = (facts.as_mut(), identity.login.as_ref()) {
        // Claude Code's roster judges a login of Claude Code's registry alone: an agy or Muse
        // Code pane carries an environment too, but no row of this roster names its account.
        let listed = uses_claude_registry
            .then(|| roster.at(std::time::Instant::now(), asks.roster))
            .flatten();
        let shown = listed.and_then(|listed| accounts::footer_account(listed, who, &seen));
        facts.account = shown.as_ref().and_then(|shown| shown.label.clone());
        // The address the login file names is the account's name whether or not a roster could
        // be read (`seen` is `Unread` for any program but Claude Code).
        facts.account_email = shown
            .as_ref()
            .and_then(|shown| shown.email.clone())
            .or_else(|| seen.email().map(str::to_owned));
        facts.account_detail = shown
            .as_ref()
            .and_then(|shown| account_detail(shown, listed?, &seen, first.as_deref()));
        // The settings evidence only weakens the environment's route (as [`own_label`] reads it).
        facts.route = Some(match identity.route {
            accounts::AuthRoute::Stored | accounts::AuthRoute::VaultToken { .. } => who.route,
            accounts::AuthRoute::Outside => accounts::AuthRoute::Outside,
        });
        // The vault token facts every surface maps, read here at most once a minute (§5.9).
        crate::token_shown::refresh(clock.now);
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
        args.as_ref().and_then(|args| {
            // A refused auth-source check says nothing, and a registry write
            // missed during an exec cannot borrow the prior image's verdict (`args` is this
            // image's). A profile read leaves a token route out: its profile may be stale (§5.7).
            if !identity.route.is_stored() {
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
                (roster.at(std::time::Instant::now(), asks.roster), &seen),
                asks,
                clock.now,
            );
        }
        facts.subscription = subscription.clone();
        facts.plan = plan;
        // The quota view of this pane's account (`docs/DESIGN-harness-glass-rainbow-2026-11-05.md`
        // §2, §5.1), built of the outlook this thread replays at most once a minute, hashed
        // into the fingerprint, never the raw clock. Only under the label the account gate
        // passed (`sub.label`): a pane whose gate refused says no row's figures, as its weekly
        // says none. Its own figure is the weekly's: a wall's `out`, never an inferred 100%.
        let views = subscription.as_ref().and_then(|sub| {
            let label = sub.label.clone()?;
            let own = if sub.weekly.as_deref() == Some(WEEKLY_OUT) {
                Some(aterm_agent::harness::outlook::Figure::Out)
            } else {
                sub.weekly_figure
                    .map(|f| aterm_agent::harness::outlook::Figure::of_weekly(&f, clock.now))
            };
            let views = roster.quota(
                (std::time::Instant::now(), clock.now),
                asks.roster,
                &label,
                (own, identity.route.on_account()),
            );
            // The tier this process's gated profile read named, as its row's `plan` row (§4.2).
            if gated.is_some()
                && let Some(title) = sub.plan.as_deref()
            {
                roster.outlook.note_plan(&label, title, clock.now);
            }
            views
        });
        (facts.quota, facts.quota_observed) =
            views.map_or((None, None), |views| (Some(views.view), views.observed));
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
                (roster.at(std::time::Instant::now(), asks.roster), &seen),
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
/// says the wall ([`WEEKLY_OUT`]), never a percentage; else the newest figure still shown, its
/// own or the store's; else why none is ([`WeeklyGap`]).
fn weekly_into(
    sub: &mut SubscriptionDisplay,
    facts: &FooterFacts,
    identity: &Identity,
    gated: Option<Option<&str>>,
    (roster, seen): (Option<&accounts::Roster>, &Observation),
    asks: Asks,
    now: i64,
) {
    let shown = |figure: Option<WeeklyFigure>| figure.filter(|f| f.words(now).is_some());
    let tapped = tap::carried_by(&identity.argv);
    sub.label = gated.and_then(|_| own_label(identity, roster, seen, asks));
    let seat = gated.map(|_| (asks.tap.seat)());
    let tap = gated
        .zip(seat.as_ref())
        .and_then(|(tab, seat)| tab.filter(|_| tapped).zip(seat.dir.as_deref()))
        .and_then(|(tab, dir)| own_tap_reading(identity, tab, dir));
    // Which row Claude shows for the newest run (the quota row's claim, glass design §5.2), read
    // here beside the figure so the frame reads no file.
    sub.tap_own = tap.as_ref().is_some_and(tap::Reading::claimable);
    sub.model_id = tap.as_ref().and_then(|reading| reading.model.clone());
    // The five-hour window rides on the same reading, under the same gate; the newer of it and
    // the `/usage` cache's is what the Account card's `5-hour` row says.
    let five = tap.as_ref().and_then(|reading| {
        let window = reading.five_hour.as_ref()?;
        let at = reading.at;
        let (used_x10, resets_at) = (window.x10(), window.resets_at);
        own_tap_figure(identity, reading.clone()).map(|_| WeeklyFigure {
            used_x10,
            resets_at,
            at,
        })
    });
    if gated.is_some() {
        sub.five_hour = WeeklyFigure::newer(sub.five_hour, five);
    }
    let reading = tap.and_then(|reading| own_tap_figure(identity, reading));
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
                own_stored_week(identity, (roster, seen), asks, seat.store.as_deref()?, now)
            });
            if walled || store.is_some_and(|week| week.walled) {
                sub.weekly = Some(WEEKLY_OUT.into());
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
            if reading
                .is_some_and(|f| aterm_agent::harness::profile::usage_read_age(f.at, now).is_none())
            {
                WeeklyGap::ClockInvalid
            } else if stale {
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
        } else if let Some((tab, pid)) = gated
            .flatten()
            .zip(u32::try_from(identity.pid).ok())
            .filter(|(tab, pid)| (asks.tap.restart)(tab, *pid))
        {
            if (asks.tap.room)(tab, pid) {
                WeeklyGap::Untapped
            } else {
                WeeklyGap::Unoffered(tap::Refusal::Room)
            }
        } else {
            WeeklyGap::Unrestarted
        }
    });
}

/// The newest reading this process's usage tap wrote for tab `tab` in `dir` since the process
/// started ([`tap::Reading::read`]), whatever its age.
fn own_tap_reading(identity: &Identity, tab: &str, dir: &std::path::Path) -> Option<tap::Reading> {
    let pid = u32::try_from(identity.pid).ok()?;
    let started = i64::try_from(identity.started?).ok()?;
    tap::Reading::read(dir, tab, pid, started).map(|(reading, _)| reading)
}

/// THE EFFORT CLAUDE CODE SAYS THIS PROCESS'S SESSION IS SET TO, from its own usage tap: the
/// `effort.level` of the status-line payload its tap last recorded ([`tap::Reading::effort`]),
/// named as the footer names levels ([`footer::effort_level`]; a level a later build adds, as
/// Claude Code spells it). Claude Code runs the tap with the level its own `/effort` shows at
/// that moment: when that level changes (2.1.322 re-runs its status line 300 ms after one) and
/// every [`tap::TAP_REFRESH_S`] seconds besides, so it follows a switch typed while a turn
/// runs; the transcript names the effort that turn's requests are sent with, and records the
/// switch only once the turn's reply lands.
///
/// Read for a process whose own line carries aterm's tap ([`tap::carried_by`]) in the tab its
/// exec environment `args` names, from the file that tab and pid name, written since the
/// process started ([`tap::Reading::read`]), and only while that file is heard
/// ([`tap::SILENT_S`] of `now`, the bound [`tap::Silence`] judges a tap by). `None` otherwise
/// (no tap, no reading yet, a tap gone silent, a build or model that names no effort), and the
/// transcript's effort stands: never a level nothing reported.
fn own_tap_effort(
    identity: &Identity,
    args: &atpkg::caller_shell::ProcArgs,
    asks: Asks,
    now: i64,
) -> Option<String> {
    if !tap::carried_by(&identity.argv) {
        return None;
    }
    let tab = env_value(&args.env, aterm_types::domain::ENV_PARENT_SESSION_ID)?;
    let dir = (asks.tap.seat)().dir?;
    let pid = u32::try_from(identity.pid).ok()?;
    let started = i64::try_from(identity.started?).ok()?;
    let (reading, written) = tap::Reading::read(&dir, &tab, pid, started)?;
    if now.saturating_sub(written) > tap::SILENT_S {
        return None;
    }
    let level = reading.effort?;
    Some(footer::effort_level(&level).map_or(level, str::to_owned))
}

/// This process's own usage-tap figure: the week of its tap's `reading` ([`own_tap_reading`]),
/// for its own login in the default home only. A reading names no account, so after a `/login`
/// to another account the week the process last held reads as the new account's until a reply
/// brings the new one's.
fn own_tap_figure(identity: &Identity, reading: tap::Reading) -> Option<WeeklyFigure> {
    let login = identity.login.as_ref().filter(|login| login.default_home)?;
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
/// weekly observation time (`week_read_at`), else its `at`. An unknown clock or
/// reading beyond the display skew is excluded before comparing sources;
/// [`store_week`] and `fact_menu::account_menu_rows_from` judge older history.
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
    [tapped, sampled].map(|figure| {
        figure.filter(|figure| {
            aterm_agent::harness::profile::usage_read_age(figure.at, now).is_some()
                && window.is_none_or(|window| window.admits_reading(figure.at))
        })
    })
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
    (roster, seen): (Option<&accounts::Roster>, &Observation),
    asks: Asks,
    store: &std::path::Path,
    now: i64,
) -> Option<StoreWeek> {
    let label = own_label(identity, roster, seen, asks)?;
    let (state, _) = accounts::Store::at(store).load(now).ok()?;
    Some(store_week(state.accounts.get(&label)?, now))
}

/// The roster label of this process's own account, for a process in the machine's default Claude
/// home and `HOME` only (the one whose readings the store keeps): the row its login file names
/// (`accounts::observed_label` over `roster`, or the machine's own row where it lists none). The
/// account menu's row it names is the only one the footer's week reaches
/// (`SubscriptionDisplay::label`).
fn own_label(
    identity: &Identity,
    roster: Option<&accounts::Roster>,
    seen: &Observation,
    asks: Asks,
) -> Option<String> {
    let login = identity.login.as_ref().filter(|login| login.default_home)?;
    let home = (asks.home)()?;
    if !tap::own_records(&home, identity.dir.as_deref()?) {
        return None;
    }
    let own = accounts::Roster::own();
    let roster = roster.map(|roster| if roster.rows.is_empty() { &own } else { roster })?;
    // The settings evidence only weakens the environment's route; `judged_label` then credits
    // a token route only to the row whose entry its marker names.
    let who = accounts::ProcessLogin {
        route: match identity.route {
            accounts::AuthRoute::Stored | accounts::AuthRoute::VaultToken { .. } => login.route,
            accounts::AuthRoute::Outside => accounts::AuthRoute::Outside,
        },
        ..login.clone()
    };
    // Observation beats belief: the row the login file names (`seen`, this pass's), never the
    // believed row where the two disagree; an account no row lists has no row whose week the
    // footer could take. An environment route reads no login file.
    let unread = Observation::Unread;
    let seen = if who.route == accounts::AuthRoute::Outside {
        &unread
    } else {
        seen
    };
    accounts::observed_label(roster, &who, seen).map(str::to_owned)
}

/// One known reset occurrence has arrived.
fn muse_weekly_expired(reset_at: Option<i64>, now: i64) -> bool {
    reset_at.is_some_and(|at| at <= now)
}

fn muse_weekly_reset_at(reset: &str, clock: &footer::ReadClock<'_>) -> Option<i64> {
    let spec = aterm_agent::supervise::limit::parse_reset(reset)?;
    let local = (clock.offset_at)(None, clock.now)?;
    Some(aterm_agent::supervise::limit::reset_at_local(
        &spec,
        clock.now,
        &|unix| (clock.offset_at)(None, unix).or(Some(local)),
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
        account,
        account_email,
        account_detail,
        // The footer draws no route; a survey row and a status line read it.
        route: _,
        account_progress,
        account_progress_busy,
        top_effort: _,
        off: _,
        subscription,
        plan: _,
        context,
        // The leaf's own goal term carries it, for every program ([`goal_term`]).
        goal: _,
        quota,
        quota_observed,
        line_e,
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
    (model_open, effort_open, effort_for, owner, account, off).hash(&mut h);
    (account_email, account_detail).hash(&mut h);
    subscription.hash(&mut h);
    account_progress.hash(&mut h);
    account_progress_busy.hash(&mut h);
    context.hash(&mut h);
    (quota, quota_observed, line_e).hash(&mut h);
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

/// Where a footer rule sits across its pane ([`compose_rule_spans`]): the pane column past the
/// last cell of the run it may write (`footer::rule_run`) and the pane's width, so each cell takes
/// its column's hue ([`aterm_effects::glass_inks::field`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RuleAt {
    end: usize,
    cols: usize,
}

impl RuleAt {
    /// A rule across a whole pane `cols` wide.
    #[cfg(test)]
    const fn across(cols: usize) -> Self {
        Self { end: cols, cols }
    }

    /// The palette position of pane column `col`.
    fn t(self, col: usize) -> f32 {
        aterm_effects::glass_inks::field(col, self.cols)
    }
}

/// The cells aterm writes into `room` cells of the rule for one pane, in the glass's tokens
/// (`docs/DESIGN-harness-glass-rainbow-2026-11-05.md` §4.1): a fact's mark in the mark ink of its
/// column, a value and a quiet word in theirs; the account unit by its pieces' tones, its name
/// and `↺` in the lit ink, its `·` in the quiet ink, its figures and times in the value ink and a
/// projected gap's words in the warn ink. The facts are terminal text: Normal script, each glyph
/// in the cells [`footer::set_facts`] gives it, regular but for the parts laid bold (a warning's
/// words, `↺`). Under a palette the rule's own `─` (its lead, the run between the chips and the
/// facts, and its close) wear the arc of their columns, so the whole run is written; with none,
/// the run between stays Claude's. A run with no room (`footer::RULE_LEAD + 1` cells or fewer)
/// takes no cells either way: the arc's lead and close would not fit inside it.
fn compose_rule_spans(
    fit: &footer::RuleFit,
    block: Option<&crate::claude_lights::Block>,
    rule: RenderCell,
    blank: RenderCell,
    (glass, at): (&GlassInks, RuleAt),
    room: usize,
    lit: Option<FactKind>,
) -> RulePaint {
    let arc = glass.palette().is_some() && room > 0;
    let mut out: Vec<RenderCell> = Vec::with_capacity(room + footer::RULE_LEAD + 1);
    if arc {
        out.extend(std::iter::repeat_n(rule, footer::RULE_LEAD));
    }
    let lead = out.len();
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
        let at = out.len();
        out.extend_from_slice(&block.lights);
        // The rule threads between a worded chip and the next, in its own ink.
        for gap in block.rule_gaps() {
            out[at + gap] = rule;
        }
        let gap = title.len().min(2);
        out.extend_from_slice(&title[title.len() - gap..]);
        title_at = out.len()..out.len() + (title.len() - gap);
        out.extend_from_slice(&title[..title.len() - gap]);
        out.push(blank);
    }
    let mut facts: Vec<RenderCell> = Vec::new();
    let mut spans = footer::FactSpans::default();
    let mut clusters = Vec::new();
    let mut combining = Vec::new();
    if !fit.segments.is_empty() {
        // Line D's facts are terminal text (`docs/DESIGN-harness-glass-rainbow-2026-11-05.md`
        // §4.3): Normal script, each glyph in its own cells, regular weight but where a part is
        // laid bold (a warning's words and `↺`, `footer::Segment::bold`).
        let laid = |fg: u32| RenderCell {
            fg: rgb_of(fg),
            bold: false,
            script: aterm_core::terminal::Script::Normal,
            ..blank
        };
        let value = laid(glass.value());
        // A stub is quieter than its value by its ink alone ([`GlassInks::quiet`]).
        let stub = laid(glass.quiet());
        let warn = laid(glass.warn());
        let set = footer::set_facts(&fit.segments);
        // The facts are laid against the run's close, at pane column `at.end - 1`: their cell
        // `c` (a blank first) sits at this pane column.
        let fact_col = |c: usize| (at.end + c).saturating_sub(set.cells + 3);
        spans = footer::fact_spans(&set, &fit.segments);
        // Every span of the lit fact lights: one fact can lie in two (the wall `⧗ until 3pm` and
        // the account unit `⎆ main …` are both the account, one target the pointer and the
        // keyboard light and a click opens as one).
        let lit_cells = |kind: FactKind| {
            spans
                .iter()
                .flatten()
                .filter(move |span| span.kind == kind)
                .map(|span| usize::from(span.cells.0)..usize::from(span.cells.1))
        };
        let is_lit =
            |cell: usize| lit.is_some_and(|kind| lit_cells(kind).any(|c| c.contains(&cell)));
        facts = vec![blank; set.cells + 2];
        for glyph in &set.glyphs {
            let lit = is_lit(glyph.cell);
            let t = at.t(fact_col(1 + glyph.cell));
            // Line D's inks (`docs/DESIGN-harness-glass-rainbow-2026-11-05.md` §1.2): marks in
            // the field's hue, values in the value ink, the account unit's name and `↺` lit,
            // its `·` quiet, a projected gap's words warn. A lit fact wears the glass's one
            // hover (§1.5): every ink the lit one, whatever its column, but a warning's.
            let warns = !glyph.mark && !glyph.quiet && glyph.tone == footer::Tone::Warn;
            let rest = match (glyph.mark, glyph.quiet, glyph.tone) {
                (true, _, _) => laid(glass.mark(t)),
                (false, true, _) | (false, false, footer::Tone::Quiet) => stub,
                (false, false, footer::Tone::Warn) => warn,
                (false, false, footer::Tone::Lit) => laid(glass.lit()),
                (false, false, footer::Tone::Value) => value,
            };
            let ink = if lit {
                let part = if warns {
                    HoverPart::Warn
                } else {
                    HoverPart::Text
                };
                crate::claude_lights::hovered_cell(glass, rest, part)
            } else {
                rest
            };
            let cell = RenderCell {
                ch: glyph.ch,
                bold: glyph.bold,
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
        }
        // A lit fact's one underline runs under each of its spans' cells, from the span's first
        // glyph to its last, the blanks between them too.
        for c in lit.into_iter().flat_map(lit_cells).flatten() {
            let cell = &mut facts[1 + c];
            if cell.underline == aterm_core::terminal::UnderlineStyle::None {
                *cell = crate::claude_lights::hovered_cell(glass, *cell, HoverPart::Text);
            }
        }
        clusters = set.clusters;
        combining = set.combining;
    }
    let mut kept = 0..0;
    if arc {
        out.extend(std::iter::repeat_n(
            rule,
            (lead + room).saturating_sub(out.len() + facts.len()),
        ));
    } else if out.len() > lead {
        let glyphs = room.saturating_sub(out.len() + facts.len());
        kept = out.len()..out.len() + glyphs;
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
    if arc {
        out.push(rule);
        // Every one of the rule's own `─` laid wears the arc of its column.
        let start = at.end.saturating_sub(out.len());
        for (i, cell) in out.iter_mut().enumerate() {
            if *cell == rule {
                cell.fg = rgb_of(glass.rule(at.t(start + i)));
            }
        }
    }
    RulePaint {
        cells: out,
        lights_at,
        title: title_at,
        glass: kept,
        close: arc,
        facts_at,
        spans,
        clusters,
        combining,
    }
}

/// The quiet ink a greyed word (a hidden fact's stub, the key's ghost) wears on `blank`'s pane
/// beside a rule drawn in `rule` ([`GlassInks::quiet`]). Its one caller is a unix test.
#[cfg(all(test, unix))]
pub(crate) fn quiet_ink(rule: RenderCell, blank: RenderCell) -> [u8; 3] {
    rgb_of(rule_glass(None, rule, blank).quiet())
}

/// The glass's tokens a rule drawn in `rule` on `blank`'s pane wears under `palette`.
#[cfg(test)]
fn rule_glass(
    palette: Option<aterm_effects::harness_card::CardPalette>,
    rule: RenderCell,
    blank: RenderCell,
) -> GlassInks {
    GlassInks::of(
        palette,
        pack_rgb(blank.bg),
        pack_rgb(blank.fg),
        pack_rgb(rule.fg),
    )
}

/// The place of a rule whose `room` is a whole pane's run ([`RuleAt`]).
#[cfg(test)]
const fn rule_at_room(room: usize) -> RuleAt {
    RuleAt::across(room + footer::RULE_LEAD + 1)
}

/// [`compose_rule_spans`]' cells and where the chips start under `palette`, on a pane whose run
/// is the room's, as the tests read a composition.
#[cfg(test)]
fn compose_rule(
    fit: &footer::RuleFit,
    block: Option<&crate::claude_lights::Block>,
    rule: RenderCell,
    blank: RenderCell,
    palette: Option<aterm_effects::harness_card::CardPalette>,
    room: usize,
) -> (Vec<RenderCell>, Option<usize>) {
    let glass = rule_glass(palette, rule, blank);
    let paint = compose_rule_spans(
        fit,
        block,
        rule,
        blank,
        (&glass, rule_at_room(room)),
        room,
        None,
    );
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
    /// Whether the last footer splice rewrote any row's columns.
    pub(crate) const fn has_glass_shifts(&self) -> bool {
        self.write.is_some() && self.glass.len > 0
    }

    /// Maps glass column `col` on `row` back to its underlying terminal grid column.
    pub(crate) fn grid_col(&self, row: u16, col: u16) -> u16 {
        if self.write.is_none() {
            return col;
        }
        self.glass.grid_col(row, col)
    }

    /// Whether window grid cell `(row, col)` is one the glass's rewrite of a row vacated: a press
    /// there lands on nothing, never on what Claude drew under it.
    pub(crate) fn swallows(&self, row: u16, col: u16) -> bool {
        self.write.is_some() && self.glass.swallows(row, col)
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
pub(crate) fn touches<T>(
    channel: &[Vec<(usize, T)>],
    row: usize,
    span: &std::ops::Range<usize>,
) -> bool {
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
    let restamp = scratch.shifted_fill_seq != 0 && scratch.host_prepend_blessing().keeps_lane();
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
        let now = std::time::SystemTime::now();
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
            let off = self.fact_mask_known(
                session,
                timeline
                    .agent()
                    .program
                    .as_deref()
                    .and_then(aterm_phase::program_of),
            );
            let kept = facts.and_then(|f| {
                ws.claude_footer_texts
                    .borrow()
                    .footers
                    .get(index)?
                    .fingerprint_of(session, f, timeline.claude_card(), usage, off)
            });
            let leaf = kept.unwrap_or_else(|| fingerprint(facts, card, usage, off));
            // Line E's claim, and a grace that still keeps line D's unit back: its lapse redraws.
            let line_e = ws
                .claude_footer_texts
                .borrow()
                .graces
                .get(&session)
                .map_or(0, |g| g.term(now));
            let shows = facts.is_some();
            let (agent, group) = (
                crate::harness_host::agent_of(timeline.agent()),
                timeline.agent().program_pgid,
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
            let mut goal = goal_term(timeline.goal());
            let cli = TopCli::from_timeline(&timeline).term();
            drop(timeline);
            // A standing permission click's light, which its lane moves off any frame.
            let click = self.claude_click_shown(session).map_or(0, |shown| {
                use std::hash::{Hash as _, Hasher as _};
                let mut h = std::collections::hash_map::DefaultHasher::new();
                shown.hash(&mut h);
                h.finish() | 1
            });
            let agent = crate::harness_host::admitted_as(&entry.ctx, agent, group);
            let harness = if shows {
                let (word, view, supervised, _) =
                    self.session_autopilot_for(session, agent, &claims);
                if self.pane_bare(view, agent) {
                    goal = 0;
                }
                let theme = crate::autopilot::harness_theme(
                    view,
                    supervised,
                    word,
                    self.unhosted_glass(agent),
                );
                // Project current evidence before deciding whether to reuse the painted footer.
                // SessionLights receives these words only when a splice actually runs.
                let continuation = view
                    .mode()
                    .filter(|mode| *mode != aterm_agent::supervise::Autopilot::Off)
                    .map_or(0, |_| {
                        use std::hash::{Hash as _, Hasher as _};
                        let mut h = std::collections::hash_map::DefaultHasher::new();
                        self.continuation_words(session).hash(&mut h);
                        h.finish() | 1
                    });
                ws.claude_lights
                    .autopilot_term(session, word, view, self.chip_glass(session))
                    ^ continuation.rotate_left(29)
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
            fp = fp.rotate_left(7)
                ^ leaf
                ^ line_e.rotate_left(13)
                ^ harness.rotate_left(29)
                ^ key.rotate_left(41)
                ^ goal.rotate_left(19)
                ^ cli.rotate_left(23)
                ^ click.rotate_left(31);
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
        // The keyboard's selection reads as the pointer does: one hover (§1.5).
        let lit = ws.claude_lights.top_effort_lit(session);
        let ghost = (tag.is_none() && evidence && !latched).then_some(lit);
        let mark = pressing
            .then(|| crate::autopilot::PENDING_GLYPH.chars().next())
            .flatten();
        KeyFace {
            ghost,
            tag,
            tag_lit: lit && tag.is_some(),
            mark: mark.filter(|_| ghost.is_some() || tag.is_some()),
        }
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
            unhosted_off,
            palette,
            cell,
        } = *at;
        let Some(top) = texts.top_line(leaf_index, program) else {
            return;
        };
        if self.chrome_owns_terminal_span(
            wid,
            region.row_off + top,
            region.col_off..region.col_off + region.cols,
        ) {
            return;
        }
        let (Some(run), tag) = texts.top_run(
            leaf_index,
            (session, program),
            &ws.input_scratch,
            region,
            top,
            blank,
        ) else {
            return;
        };
        let ground = blank;
        let hide_lockup = bare_while_off(switch.map(|s| s.face)) || unhosted_off;
        let frame_row = region.strip + region.row_off + top;
        let footer = texts.footer(leaf_index, session);
        let typed = !hide_lockup && program == aterm_phase::Program::Claude && footer.key_typed();
        let face = if hide_lockup {
            KeyFace::default()
        } else {
            self.key_face(
                ws,
                session,
                program_pgid,
                typed.then(|| footer.shown().top_effort.is_some()),
                footer.published().and_then(|f| f.model.as_deref()),
                tag,
            )
        };
        if face.tag.is_some()
            && self.claude_facts.get(&session).and_then(|f| f.top_seen) != Some(program_pgid)
        {
            lists.top_seen.push((session, program_pgid));
        }
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
        let key = TopKey {
            frame_row,
            term_row: region.row_off + top,
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
            field: PaneField {
                col_off: pane.col_off,
                cols: pane.cols,
            },
            face,
            cell,
            clip: pane_grid_px(cell, pane),
        };
        // Claude Code's label zone as the grid holds it: a laid switch moves it whole.
        let tail = run
            .tail
            .filter(|t| t.plain)
            .and_then(|t| {
                ws.input_scratch
                    .cells
                    .get(frame_row)?
                    .get(t.col..t.col + t.len)
            })
            .unwrap_or_default();
        let composed = texts.compose_top(leaf_index, key, tail);
        let hit = |cols: (usize, usize), ghost: bool| TopHit {
            frame_row: key.frame_row,
            term_row: key.term_row,
            cols,
            session,
            ghost,
        };
        if let Some(cols) = composed.ghost {
            lists.top_hits.push(hit(cols, true));
        } else if let Some(cols) = composed.tag.or(face.tag) {
            lists.top_hits.push(hit(cols, false));
        }
        if let (Some(laid), Some(paint)) = (composed.switch.clone(), switch) {
            lists
                .border_toggles
                .push(crate::claude_lights::BorderSwitch {
                    frame_row: key.frame_row,
                    term_row: key.term_row,
                    cols: laid.cols,
                    ink: laid.ink,
                    mark: laid.label.as_ref().map(|(cols, _)| cols.clone()),
                    arrow: laid.arrow.clone(),
                    session,
                    face: paint.face,
                    paint,
                    program,
                });
        }
        if !composed.cells.is_empty() {
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
            ws.pet_chrome.painted_over(ws.input_scratch.content_seq);
            std::mem::take(&mut ws.claude_footer_lists)
        }) else {
            return;
        };
        let grid = self.glass_grid(wid);
        self.splice_claude_footer_with(wid, plan, route, &mut lists, clock, grid);
        self.settle_glass_hover(wid, grid, plan);
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
        // The agents' transcripts keep the instance's accent until their markdown has tokens
        // of its own; the composer's lines take none of it ([`GlassInks`]).
        let markdown_accent = crate::chrome_band::band_colors(self.theme).accent;
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
                // A rewound pane shows the past: no live glass over it, and no reading of it.
                if self.pool.is_rewound(session) {
                    continue;
                }
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
                        let switch_off = self.pane_bare(view, agent);
                        if view.wears_glass(supervised).is_some() && !switch_off {
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
                                markdown_accent,
                                &mut lists.agy_edits,
                            );
                            lists.agy_edits.retain(|e| {
                                !self.chrome_owns_terminal_span(
                                    wid,
                                    e.frame_row.saturating_sub(region.strip),
                                    region.col_off..region.col_off + region.cols,
                                )
                            });
                        }
                    }
                    continue;
                }
                let (
                    held,
                    agent,
                    flights,
                    gate,
                    identified,
                    muse_cards_need_account,
                    top_cli,
                    kept_hint,
                ) = {
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
                    ws.claude_footer_texts
                        .borrow_mut()
                        .footer(leaf_index, session)
                        .note_goal(timeline.goal());
                    let held = timeline.claude_footer().is_some_and(|facts| {
                        let mut texts = ws.claude_footer_texts.borrow_mut();
                        texts.note_facts(
                            leaf_index,
                            session,
                            facts,
                            timeline.claude_card(),
                            usage,
                            self.fact_mask_known(
                                session,
                                timeline
                                    .agent()
                                    .program
                                    .as_deref()
                                    .and_then(aterm_phase::program_of),
                            ),
                        );
                        true
                    });
                    let gate = timeline.card_gate();
                    (
                        held,
                        (
                            crate::harness_host::agent_of(timeline.agent()),
                            timeline.agent().program_pgid,
                        ),
                        timeline.claude_side().and_then(|side| side.flights.clone()),
                        gate,
                        crate::harness_host::identified_agent(timeline.agent()),
                        timeline.muse_cards_need_account(),
                        TopCli::from_timeline(&timeline),
                        gate.and_then(|gate| timeline.effort_hint(gate.token)),
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
                // Codex owns every cell of its interface, including after a hosted CLI
                // left cached footer facts or a card in the same tab.
                if identified == Some(aterm_phase::Program::Codex) {
                    if ws.claude_card.keeps(session) {
                        cards.push(crate::claude_card::drawerless_leaf(
                            leaf_index, session, pane,
                        ));
                    }
                    continue;
                }
                if !held && gate.is_none() {
                    // No drawer and no facts: a card this window keeps for the session left
                    // by its identity (Claude stopped or gone) bounds its next show
                    // (`claude_card::RESHOW_MS`).
                    if ws.claude_card.keeps(session) {
                        cards.push(crate::claude_card::drawerless_leaf(
                            leaf_index, session, pane,
                        ));
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
                // Claude's own effort hint (`◐ medium · /effort`) says the effort it runs at. It is
                // read only off a row the holder drew (`admits_reading`), and its level is kept for
                // that holder whether or not the facts are in yet. Where the facts leave the effort
                // open (a resume or a fresh launch, before the transcript names one) and the
                // screen's card names none, that level is the card's effort, so `◆` shows it and
                // keeps it once the hint goes, facts published only after it went included. Where
                // no frame read it (no frame was drawn while the hint was up), the level an output
                // read off the live screen for the same holder stands in
                // (`App::read_claude_effort_hint`).
                if let Some((row, level)) = texts
                    .panes
                    .get(leaf_index)
                    .and_then(Option::as_ref)
                    .filter(|cached| cached.key.session == session)
                    .and_then(|cached| cached.readings.mode.as_ref())
                    .and_then(footer::ModeRow::effort_said)
                    && check.admits_reading(
                        texts,
                        leaf_index,
                        session,
                        aterm_phase::Program::Claude,
                        None,
                        Some(row),
                    )
                {
                    texts
                        .footer(leaf_index, session)
                        .note_hint(check.token(), level);
                }
                let said = texts
                    .footers
                    .get(leaf_index)
                    .and_then(|footer| footer.hint(check.token()))
                    .or(kept_hint);
                let effort_open =
                    |footer: &PaneFooter| footer.published().is_some_and(|facts| facts.effort_open);
                if let Some(footer) = texts.footers.get(leaf_index).filter(|_| held)
                    && let Some(owner) = footer.card_owner()
                    && let Some((program, read)) = texts.footer_card(leaf_index, session)
                {
                    let card = &read.card;
                    let hinted = *program == aterm_phase::Program::Claude
                        && card.effort.is_none()
                        && effort_open(footer);
                    let hint = said.filter(|_| hinted);
                    let effort = card.effort.as_deref().or(hint);
                    // A card naming no effort keeps the one a hint gave once the hint goes.
                    let same_as_kept = footer.kept_card().is_some_and(|kept| {
                        kept.model == card.model
                            && (kept.effort.as_deref() == effort || hinted && effort.is_none())
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
                        if let Some(level) = hint {
                            card.effort = Some(level.to_owned());
                        }
                        let owner = owner.clone();
                        self.keep_screen_card(
                            &entry.ctx.timeline,
                            texts,
                            (leaf_index, session),
                            &owner,
                            card,
                            usage,
                            &mut lists.log_owed,
                        );
                    }
                } else if let Some(footer) = texts.footers.get(leaf_index).filter(|_| held)
                    && let Some(owner) = footer.card_owner()
                    && let Some(level) = said.filter(|_| effort_open(footer))
                    && footer
                        .kept_card()
                        .is_none_or(|kept| kept.effort.as_deref() != Some(level))
                    && let Some(facts) = footer.published()
                    && let Some(model) = facts
                        .effort_for
                        .as_deref()
                        .or(footer.kept_card().map(|kept| kept.model.as_str()))
                        .or(footer.shown().model.as_deref())
                {
                    // No card on the screen (a resume): the hint makes one of the facts' build
                    // and model, and a new level it says (an `/effort` since) replaces it.
                    let version = facts
                        .version
                        .as_deref()
                        .or(footer.kept_card().map(|kept| kept.version.as_str()))
                        .unwrap_or_default();
                    let card = aterm_phase::LaunchCard {
                        version: version.to_owned(),
                        model: model.to_owned(),
                        effort: Some(level.to_owned()),
                    };
                    let owner = owner.clone();
                    self.keep_screen_card(
                        &entry.ctx.timeline,
                        texts,
                        (leaf_index, session),
                        &owner,
                        card,
                        usage,
                        &mut lists.log_owed,
                    );
                }
                if held && texts.panes.get(leaf_index).is_some_and(Option::is_some) {
                    let shown = texts.footers[leaf_index].shown();
                    // A login the account state says is signed out takes nothing from the
                    // screen: a launch card or a usage card still in view is what was true
                    // before the sign-out, and the footer says what is true now.
                    let want_acct = matches!(
                        agent,
                        Some(aterm_phase::Program::Agy | aterm_phase::Program::Muse)
                    ) && shown
                        .subscription
                        .as_ref()
                        .is_some_and(|sub| !sub.signed_out);
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
                        let has_live_agy_figure = matches!(
                            agent,
                            Some(aterm_phase::Program::Agy | aterm_phase::Program::Muse)
                        ) && shown
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
                                        self.fact_mask_known(
                                            session,
                                            timeline
                                                .agent()
                                                .program
                                                .as_deref()
                                                .and_then(aterm_phase::program_of),
                                        ),
                                    );
                                }
                            }
                        }
                    }
                }
                let switch_off = self.pane_bare(view, agent);
                cards.push(crate::claude_card::card_leaf(
                    texts,
                    self.config.claude_footer_card() && !switch_off,
                    leaf_index,
                    session,
                    pane,
                    blank,
                    view,
                    supervised,
                    &mut check,
                ));
                if !held {
                    continue;
                }
                let version = texts.footers[leaf_index].shown().version.as_deref();
                let Some(readings) = texts.readings(leaf_index) else {
                    continue;
                };
                // A composer another holder drew (the last screen of a process that exited, before
                // the one in front draws its own) stays as it was drawn, as an older composer in
                // history does: the glass decorates only a composer the current holder drew, by
                // its fence ([`crate::claude_card::FenceFrame::disowns`]: none of its rows in the
                // holder's spans). Its lights are not read for this holder.
                if let Some((top, bottom)) = readings.reach.and_then(|r| r.rules())
                    && check.disowns(top, bottom + 1)
                {
                    lists.bare.push((session, row_off + bottom));
                    lists.seen.push(SeenPane {
                        leaf_index,
                        session,
                        lights: None,
                        click: self.claude_click_shown(session),
                        jobs: String::new(),
                        switch: crate::autopilot::HarnessSwitch::of(view),
                        autopilot: None,
                    });
                    continue;
                }
                let read = readings.rule;
                if read.is_some_and(|r| !r.mode_row) {
                    texts.note_drift(leaf_index, session, version);
                }
                let on_glass = read.filter(|_| !switch_off).map(|r| r.rule).filter(|&r| {
                    !self.chrome_owns_terminal_span(wid, row_off + r, col_off..col_off + pane_cols)
                });
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
                if let Some(r) = on_glass
                    && rule.is_none()
                {
                    lists.bare.push((session, row_off + r));
                }
                let harness_on = view.wears_glass(supervised).is_some() && !switch_off;
                let click = self.claude_click_shown(session);
                // Claude's running jobs, read past the ⬆️'s own need for them: its words name them
                // for a move held by them, and the mode light's title for what a click's restart
                // ends while that title shows (under the pointer or the keyboard, or with the
                // tab's click standing), on every mode's row.
                let up_jobs = top_cli.up.shows() || top_cli.kept.shows();
                let light_jobs = click.is_some() || ws.claude_lights.light_titled(session);
                let jobs = if up_jobs || light_jobs {
                    readings
                        .mode
                        .zip(texts.panes.get(leaf_index).and_then(Option::as_ref))
                        .and_then(|(mode, cached)| {
                            cached.rows.get(mode.row).map(|row| mode.job_words(row))
                        })
                        .unwrap_or_default()
                } else {
                    String::new()
                };
                let switch = crate::autopilot::HarnessSwitch::of(view).map(|face| {
                    let (face, hover) = ws.claude_lights.switch_shown(session, face);
                    let paint = SwitchPaint::of(face, hover, agent, top_cli);
                    // What a press would wait for, read off the live screen only while the
                    // pointer is on a Claude Code ⬆️ that a press acts on.
                    let press = (hover == HoverAt::Arrow
                        && paint.up.offers()
                        && paint.up.agent.presses())
                    .then(|| crate::term_try_lock(&entry.term).and_then(|t| press_wait_on(&t)))
                    .flatten();
                    paint
                        .with_jobs(if up_jobs { &jobs } else { "" })
                        .with_press(press)
                        .refused_with(ws.claude_lights.up_refused(session))
                });
                let unhosted_on = self.unhosted_glass(agent);
                let theme = crate::autopilot::harness_theme(view, supervised, word, unhosted_on);
                let border = theme.filter(|_| switch.is_some() && !switch_off);
                let gate = agent
                    .is_some_and(|prog| rewrites_mode_row(prog, border.is_some()) && !switch_off);
                let top_rule = readings.reach.and_then(|r| r.rules()).map(|(top, _)| top);
                if let Some(
                    prog @ (aterm_phase::Program::Claude
                    | aterm_phase::Program::Agy
                    | aterm_phase::Program::Muse
                    | aterm_phase::Program::Kimi),
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
                            build: Build::Vendor,
                            region,
                            pane,
                            blank,
                            switch,
                            unhosted_off: self.unhosted_off(view, agent),
                            palette: border,
                            cell,
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
                            markdown_accent,
                            &mut lists.agy_edits,
                        );
                        lists.agy_edits.retain(|e| {
                            !self.chrome_owns_terminal_span(
                                wid,
                                e.frame_row.saturating_sub(region.strip),
                                region.col_off..region.col_off + region.cols,
                            )
                        });
                    }
                    if matches!(
                        prog,
                        aterm_phase::Program::Claude | aterm_phase::Program::Agy
                    ) && (prog == aterm_phase::Program::Claude || harness_on)
                        && !switch_off
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
                    // On the bypass row the glass rewrites, Claude's effort hint goes only where
                    // the session's facts name an effort for the bottom rule's `◆`, whether the
                    // rule has room to show it or the person hid it by a click; else it stays on
                    // the row. Any other mode's row stands whole, its hint with it.
                    let names_effort =
                        rule.is_some() && texts.footers[leaf_index].shown().effort.is_some();
                    if gate
                        && let Some(mode) = readings.mode
                        && let Some(mode) = if names_effort {
                            Some(mode)
                        } else {
                            mode.keeping_effort()
                        }
                        && let Some(Some(cached)) = texts.panes.get(leaf_index)
                    {
                        collect_mode_row(
                            &ws.input_scratch,
                            &cached.rows,
                            region,
                            mode,
                            |term_row| {
                                self.chrome_owns_terminal_span(
                                    wid,
                                    term_row,
                                    region.col_off..region.col_off + region.cols,
                                )
                            },
                            blank,
                            &mut lists.mode_rows,
                            &mut lists.mode_cells,
                        );
                    }
                }
                // Line E, the quota row (glass design §5.2): claimed while the row under the
                // bottom rule is the tap's own and a rung of the view fits it, laid where its
                // cells are plain. Until a claim, and for the grace after the last, line D gives
                // up its account unit; agy's, a CLI's with no line E and a harness switched off
                // never claim, and line D keeps the unit.
                let claimable = matches!(agent, None | Some(aterm_phase::Program::Claude))
                    && !crate::autopilot::HarnessSwitch::of(view)
                        .is_some_and(crate::autopilot::HarnessSwitch::is_off);
                let rows = texts
                    .panes
                    .get(leaf_index)
                    .and_then(Option::as_ref)
                    .map(|cached| cached.rows.clone());
                let claim = match read.zip(rows.as_deref()) {
                    Some((r, rows)) if claimable => {
                        texts.footers[leaf_index].claim_quota_row(&quota::RowAt {
                            scratch: &ws.input_scratch,
                            rows,
                            region,
                            rule: r.rule,
                            chrome_owned: self.chrome_owns_terminal_span(
                                wid,
                                row_off + r.rule + 1,
                                region.col_off..region.col_off + region.cols,
                            ),
                        })
                    }
                    _ => quota::Claim::Not,
                };
                let (line_e, wake) = texts.graces.entry(session).or_default().note(
                    || clock().read_at,
                    claimable.then_some(claim != quota::Claim::Not),
                );
                let footer = &mut texts.footers[leaf_index];
                footer.note_line_e(line_e);
                if let Some(left) = wake {
                    crate::claude_lights::wake_after(session, left);
                }
                if let (quota::Claim::Laid(span), Some(r)) = (claim, read) {
                    let rule_fg = rule
                        .as_ref()
                        .map_or_else(|| mix(blank.bg, blank.fg, 0.35), |(_, _, _, rc)| rc.fg);
                    let glass = GlassInks::of(
                        border,
                        pack_rgb(blank.bg),
                        pack_rgb(blank.fg),
                        pack_rgb(rule_fg),
                    );
                    let lit = ws.claude_lights.lit_fact(session, FactMask::default())
                        == Some(FactKind::Account);
                    lists.quota_rows.extend(footer.compose_quota_row(
                        session,
                        leaf_index,
                        span,
                        (region, strip + row_off + r.rule + 1),
                        (glass, blank, lit),
                        cell,
                        pane_grid_px(cell, pane),
                    ));
                }
                lists.seen.push(SeenPane {
                    leaf_index,
                    session,
                    lights: readings.lights,
                    click,
                    jobs: if light_jobs { jobs } else { String::new() },
                    switch: crate::autopilot::HarnessSwitch::of(view),
                    autopilot: Some((word, view)),
                });
                let Some((frame_row, term_row, run, rule)) = rule else {
                    continue;
                };
                lists.panes.push(FooterPane {
                    session,
                    leaf_index,
                    frame_row,
                    term_row,
                    col_off,
                    run,
                    rule,
                    blank,
                    palette: border,
                    cols: pane_cols,
                });
            }
            let seen = &lists.seen;
            ws.claude_footer_texts.borrow_mut().retain_active(
                |index| seen.iter().any(|pane| pane.leaf_index == index),
                |index| cards.iter().any(|card| card.reads(index)),
                |session| self.pool.get(session).is_some(),
            );
        }
        for pane in lists.seen.drain(..) {
            self.observe_claude_lights(wid, pane.session, pane.lights, pane.click, pane.jobs);
            if let Some(autopilot) = pane.autopilot {
                self.note_claude_autopilot(wid, pane.session, autopilot);
            } else if let Some(ws) = self.windows.get_mut(&wid) {
                ws.claude_lights.note_switch_face(pane.session, pane.switch);
            }
        }
        for (session, term_row) in lists.bare.drain(..) {
            self.note_claude_footer_row(wid, session, term_row, 0);
        }
        self.compose_claude_cards(wid, cards);
        self.compose_claude_panels(wid, &mut lists.panels);
        let lines = self
            .windows
            .get(&wid)
            .is_some_and(|ws| ws.claude_card.lines().next().is_some());
        if lists.panes.is_empty()
            && !lines
            && lists.agy_edits.is_empty()
            && lists.mode_rows.is_empty()
            && lists.quota_rows.is_empty()
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
            self.note_claude_footer_row(wid, pane.session, pane.term_row, chips_room);
            self.note_claude_team(wid, pane.session);
            let glass = GlassInks::of(
                pane.palette,
                pack_rgb(pane.blank.bg),
                pack_rgb(pane.blank.fg),
                pack_rgb(pane.rule.fg),
            );
            let rule_at = RuleAt {
                end: pane.run.end,
                cols: pane.cols,
            };
            // The chips stand a blank after the rule's lead ([`compose_rule_spans`]).
            let chips_at = crate::claude_lights::ChipAt {
                glass,
                col: pane.run.start + footer::RULE_LEAD + 1,
                cols: pane.cols,
            };
            let mut block = self.claude_lights_block(
                wid,
                pane.session,
                pane.leaf_index,
                pane.blank,
                chips_at,
                (chips_room, true),
            );
            // The mode's name a click brought up takes only cells the rule spares: where it would
            // move or drop a fact or drop the title, the block is laid again without it.
            if let Some(laid) = block.take_if(|b| {
                b.word > 0
                    && !self.windows.get(&wid).is_some_and(|ws| {
                        ws.claude_footer_texts
                            .borrow_mut()
                            .footer_mut(pane.leaf_index)
                            .is_some_and(|footer| footer.spares(room, b))
                    })
            }) {
                self.recycle_claude_lights_block(wid, laid);
                block = self.claude_lights_block(
                    wid,
                    pane.session,
                    pane.leaf_index,
                    pane.blank,
                    chips_at,
                    (chips_room, false),
                );
            }
            let composed = self.windows.get(&wid).and_then(|ws| {
                let mut texts = ws.claude_footer_texts.borrow_mut();
                let footer = texts.footer_mut(pane.leaf_index)?;
                let lit = ws.claude_lights.lit_fact(pane.session, footer.shown().off);
                Some(footer.compose(
                    room,
                    block.as_ref(),
                    pane.rule,
                    pane.blank,
                    (glass, rule_at),
                    lit,
                ))
            });
            let Some(composed) = composed.filter(|c| c.cells > 0) else {
                if let Some(block) = block {
                    self.recycle_claude_lights_block(wid, block);
                }
                continue;
            };
            let col = pane.col_off + composed.start(pane.run.end);
            let Composed {
                lights_at,
                title,
                glass,
                facts_at,
                spans,
                ..
            } = composed;
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
            let card_edits = claude_card.lines().map(crate::claude_card::CardLine::edit);
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
            let mode_cells = &lists.mode_cells;
            let mode_rows = lists
                .mode_rows
                .iter()
                .map(|e| (e.frame_row, e.col, &mode_cells[e.cells.clone()], e.ground));
            let quota_rows = lists.quota_rows.iter().map(|q| {
                (
                    q.frame_row,
                    q.col,
                    texts.quota_cells(q.leaf_index),
                    q.ground,
                )
            });
            write_edits(
                claude_footer_undo,
                input_scratch,
                card_edits
                    .chain(rules)
                    .chain(tops)
                    .chain(agy)
                    .chain(mode_rows)
                    .chain(quota_rows),
            );
            // Line E's midnights show only over the row the write laid.
            for q in &lists.quota_rows {
                if claude_footer_undo.took(q.frame_row, q.col)
                    && let Some(layer) = &q.ticks
                {
                    claude_card.paint_layer(input_scratch, layer.clone());
                }
            }
            for e in &lists.mode_rows {
                if claude_footer_undo.took(e.frame_row, e.col) {
                    claude_footer_undo.glass.push(e.shift);
                }
            }
            // The mark and the ⬆️'s ring show only with the words the write laid over them.
            for t in &lists.tops {
                if claude_footer_undo.took(t.frame_row, t.col) {
                    for layer in texts.top_layers(t.leaf_index) {
                        claude_card.paint_layer(input_scratch, layer.clone());
                    }
                }
            }
            for r in claude_footer_undo.written() {
                let vendor = lists
                    .agy_edits
                    .iter()
                    .map(|e| (e.frame_row, e.col))
                    .chain(lists.mode_rows.iter().map(|e| (e.frame_row, e.col)))
                    .any(|at| at == (r.frame_row, r.col_off));
                if !vendor {
                    claude_card.paint_marks(input_scratch, r.frame_row, r.col_off, &r.painted);
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
                let at = (r.frame_row, r.col_off);
                if lists.agy_edits.iter().any(|e| (e.frame_row, e.col) == at) {
                    continue;
                }
                if lists.tops.iter().any(|t| (t.frame_row, t.col) == at) {
                    // The top rule aterm repaints with its lockup and switch is the host's
                    // line, held whole, strokes and all: a resident pet under it is drawn
                    // under the line, its head behind the strokes, where the person cannot
                    // see it (aterm-effects `PetBrain::over_the_hosts_line`).
                    pet_chrome.note(r.frame_row, r.col_off, 1, r.painted.len());
                } else if let Some(e) = lists.mode_rows.iter().find(|e| (e.frame_row, e.col) == at)
                {
                    // A rewrite of Claude Code's row: the words it lays are the host's, the rest
                    // it blanks out to the end of Claude's own words is glass. The pet read
                    // Claude's cells, so it is told that blank, or it takes the letters hidden
                    // under it for ink and seats itself out past them in empty-looking glass.
                    let words = e.words.min(r.painted.len());
                    pet_chrome.note_painted(r.frame_row, r.col_off, &r.painted[..words]);
                    pet_chrome.note_blanked(
                        r.frame_row,
                        r.col_off + words,
                        r.painted.len() - words,
                    );
                } else if let Some(q) = lists.quota_rows.iter().find(|q| (q.frame_row, q.col) == at)
                {
                    // Line E: the row's cells are painted, the tap's words past them blanked.
                    let line = q.line.min(r.painted.len());
                    pet_chrome.note_painted(r.frame_row, r.col_off, &r.painted[..line]);
                    pet_chrome.note_blanked(r.frame_row, r.col_off + line, r.painted.len() - line);
                } else {
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
                        part: None,
                    });
                }
            }
            // Line E is the account, in three parts (glass design §2.6).
            for q in lists
                .quota_rows
                .iter()
                .filter(|q| took((q.frame_row, q.col)))
            {
                for (cols, part) in q.parts() {
                    claude_lights.fact_hits.push(crate::claude_lights::FactHit {
                        frame_row: q.frame_row,
                        term_row: q.term_row,
                        cols,
                        session: q.session,
                        fact: crate::claude_lights::Fact::Account,
                        restores: FactMask::default(),
                        part: Some(part),
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
                        part: None,
                    });
                }
            }
            for b in lists.border_toggles.drain(..) {
                if !matches!(
                    b.face,
                    crate::autopilot::HarnessSwitch::Turning
                        | crate::autopilot::HarnessSwitch::TurningOff
                ) {
                    claude_lights.note_switch_face(b.session, Some(b.face));
                }
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

    /// Read Claude's effort hint (`◐ medium · /effort`) off `session`'s live screen at an output
    /// at `now`, frame or not, and keep its level on the timeline for the holder that drew it
    /// ([`crate::session_timeline::SessionTimeline::note_effort_hint`]): a launch or a relaunch
    /// no frame watched while Claude showed its hint (a background tab, an occluded window,
    /// another Space) names its effort on the first frame drawn after it. Read only while it can
    /// say something: a Claude Code drawer's gate is current, no level is kept for it yet, its
    /// facts are not in or leave the effort open, and the holder's start-up is not past
    /// ([`crate::session_timeline::SessionTimeline::effort_hint_looks`]: a default session whose
    /// hint no read found is not read for the life of the process); never while the person
    /// reads scrollback, Claude's DEC 2026 frame is unfinished or another thread holds the
    /// terminal. As in the frame, only a row the holder drew counts
    /// ([`crate::claude_card::GateCheck::admits_row`]).
    pub(crate) fn read_claude_effort_hint(&self, session: u64, now: std::time::Instant) {
        let Some(entry) = self.pool.get(session) else {
            return;
        };
        let claude = crate::holder_image::CardProgram::Claude.rows_kind();
        let gate = {
            let mut timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
            match timeline.card_gate() {
                Some(gate)
                    if gate.kind == claude
                        && timeline.effort_hint(gate.token).is_none()
                        && timeline
                            .claude_footer()
                            .is_none_or(|facts| facts.effort_open)
                        && timeline.effort_hint_looks(gate.token, now) =>
                {
                    gate
                }
                _ => return,
            }
        };
        #[cfg(test)]
        crate::work_counts::effort_hint_read();
        let (rows, fence) = {
            // Contended: the next output's wake reads again.
            let Some(t) = crate::term_try_lock(&entry.term) else {
                return;
            };
            if t.grid().display_offset() > 0 || crate::frame_unfinished_now(&t) {
                return;
            }
            let rows: Vec<String> = (0..usize::from(t.rows()))
                .map(|r| crate::claude_lights::row_text(&t, r))
                .collect();
            let fence = crate::claude_card::FenceFrame {
                holder: t.holder_view(),
                base_y: i64::try_from(t.grid().base_y()).unwrap_or(i64::MAX),
                alt: t.is_alternate_screen(),
                mutations: t.screen_mutations(),
            };
            (rows, fence)
        };
        let Some((row, level)) = effort_hint_on(session, &rows) else {
            return;
        };
        if crate::claude_card::GateCheck::new(Some(gate), fence).admits_row(claude, row) {
            let mut timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
            if timeline
                .card_gate()
                .is_some_and(|now| now.token == gate.token)
            {
                timeline.note_effort_hint(gate.token, level);
            }
        }
    }

    /// Keep `card`, read off a pane's screen for the facts `owner` holds open, as `session`'s
    /// launch card in its `timeline`, and note the facts it fills on leaf `leaf_index` of
    /// `texts`. The card's amend of the session log owes its watchers a wake, rung in this hold
    /// (`log_owed`), and the loop follows the log after the frame.
    #[expect(
        clippy::too_many_arguments,
        reason = "the session's timeline, the leaf's texts and where it sits, the card and whose \
                  facts it fills, the usage switch, and the log's owed wakes"
    )]
    fn keep_screen_card(
        &self,
        timeline: &std::sync::Mutex<crate::session_timeline::SessionTimeline>,
        texts: &mut PaneTextCache,
        (leaf_index, session): (usize, u64),
        owner: &footer::FactsOwner,
        card: aterm_phase::LaunchCard,
        usage: bool,
        log_owed: &mut Vec<u64>,
    ) {
        let mut timeline = timeline.lock().unwrap_or_else(|p| p.into_inner());
        #[cfg(test)]
        crate::work_counts::footer_timeline_locked();
        if let Some((pgid, _)) = timeline.claude_footer_identity()
            && timeline.note_claude_card(pgid, owner, card)
        {
            if timeline.slog_take_wake() {
                log_owed.push(session);
            }
            if let Some(facts) = timeline.claude_footer() {
                texts.note_facts(
                    leaf_index,
                    session,
                    facts,
                    timeline.claude_card(),
                    usage,
                    self.fact_mask_known(
                        session,
                        timeline
                            .agent()
                            .program
                            .as_deref()
                            .and_then(aterm_phase::program_of),
                    ),
                );
            }
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

/// The level Claude's effort hint names on `rows` and the row it is drawn on
/// ([`footer::ModeRow::effort_said`]), read as the frame reads a pane ([`read_pane`]).
fn effort_hint_on(session: u64, rows: &[String]) -> Option<(usize, &'static str)> {
    let reach = crate::reader_guard::read_or_none(session, "Claude Code footer", rows, || {
        aterm_phase::phase::FrameReach::of_or_hosted(rows)
    })?;
    let rule = read_footer(session, rows, |rows| read_rule_under(rows, reach))?;
    crate::reader_guard::read_or_none(session, "Claude Code mode row", rows, || {
        footer::mode_row_rewrite_under(rows, rule.rule)
    })
    .flatten()?
    .effort_said()
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
    note_vendor_drift_in(&LOGGED, session, version, rows);
}

/// [`note_vendor_drift`] against the rows already `logged` this run (its dedupe, by build and
/// the row's first three words, kept in RAM).
fn note_vendor_drift_in(
    logged: &Mutex<Vec<String>>,
    session: u64,
    version: Option<&str>,
    rows: &[String],
) {
    let unread =
        crate::reader_guard::read_or_none(session, "Claude Code drift check", rows, || {
            unread_pill_row(rows)
        })
        .flatten();
    let kept = || {
        crate::reader_guard::read_or_none(session, "Claude Code drift check", rows, || {
            unknown_mode_item_row(rows)
        })
        .flatten()
    };
    let (row, what) = match unread {
        Some(row) => (
            row,
            "a mode row this aterm does not read, so the lights stand down on it",
        ),
        None => match kept() {
            Some(row) => (
                row,
                "a mode row item this aterm does not know, kept as drawn",
            ),
            None => return,
        },
    };
    let build = version.unwrap_or("(unknown build)");
    let head: String = row.split_whitespace().take(3).collect::<Vec<_>>().join(" ");
    let key = format!("{build}\t{head}");
    let mut logged = logged.lock().unwrap_or_else(|p| p.into_inner());
    if logged.contains(&key) || logged.len() >= DRIFT_ROWS_LOGGED {
        return;
    }
    logged.push(key);
    // The row is the session's screen (an IDE selection, a task's name, a status line): the log
    // keeps its shape, its first glyph (the pill a fixture needs), its cells and its length.
    aterm_log::warn!(
        "claude footer: Claude Code {} drew {what}: first=U+{:04X} cols={} row={}",
        version.map_or("unknown", aterm_log::words::word),
        row.trim_start().chars().next().map_or(0, u32::from),
        footer::text_width(&row),
        aterm_log::words::Len(&row)
    );
}

/// The mode row under the composer whose rewrite keeps an item this build does not know
/// ([`footer::ModeRow::unknown`]).
fn unknown_mode_item_row(rows: &[String]) -> Option<String> {
    let (_, bottom) = aterm_phase::phase::composer_rules(rows)?;
    let mode = footer::mode_row_rewrite_under(rows, bottom).filter(footer::ModeRow::unknown)?;
    Some(rows[mode.row].trim_end().to_owned())
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
    /// Claude Code's own label zone right after the run ([`Tail`]).
    tail: Option<Tail>,
    /// The frame column past the rule's last cell: past the `─` that closes Claude's label zone
    /// where one follows the run, else the run's own end.
    end: usize,
}

impl TopRun {
    /// The cells a top line may lay its parts in ([`lay_top`]), the ghost's under `face` set
    /// aside where the tail stays.
    fn span(&self, face: KeyFace) -> TopSpan {
        let (tail, stuck) = match self.tail {
            Some(t) if t.plain => (t.len, false),
            Some(_) => (0, true),
            None => (0, false),
        };
        TopSpan {
            run: top_span(self.len, face),
            whole: if stuck { self.len } else { self.end - self.col },
            tail,
            ghost: face.ghost.is_some(),
            stuck,
        }
    }
}

/// Claude Code's label zone on its top rule (`footer::top_rule_labels`): its words with a blank
/// on either side (the top-effort key, sandboxed or not, ` fast mode `, a session name). A laid
/// harness switch moves it whole, cell for cell in Claude's own ink, to its own left
/// ([`compose_top`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Tail {
    /// Its first cell's frame column.
    col: usize,
    len: usize,
    /// Its cells' hash ([`tail_term`]): a change of Claude's own ink recomposes.
    term: u64,
    /// No cluster, combining mark or image touches it, and each wide glyph in it has both its
    /// halves: only a plain tail moves.
    plain: bool,
}

/// The cache term of a tail's `cells` ([`Tail::term`]).
fn tail_term(cells: &[RenderCell]) -> u64 {
    let mut h = aterm_hash::FNV64_OFFSET;
    for c in cells {
        for word in [
            u64::from(c.ch),
            u64::from(pack_rgb(c.fg)),
            u64::from(pack_rgb(c.bg)),
            u64::from(c.bold) | u64::from(c.italic) << 1 | (c.underline as u64) << 2,
        ] {
            h = aterm_hash::fnv_mix_u64(h, word);
        }
    }
    h | 1
}

/// The cells a top line lays its parts in ([`TopRun::span`], [`lay_top`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TopSpan {
    /// The plain run's cells, less the ghost's where it is drawn at the run's end ([`top_span`]).
    run: usize,
    /// The whole rule's cells from the run's start: Claude's tail and the `─` that closes it
    /// included (the plain run alone where no tail follows or the tail stays).
    whole: usize,
    /// The cells of Claude's tail that move left of a laid switch (0: none).
    tail: usize,
    /// aterm's ghost of the key is drawn ([`KeyFace::ghost`]).
    ghost: bool,
    /// Claude's tail is there but not plain: it stays where Claude drew it.
    stuck: bool,
}

impl TopSpan {
    /// A plain run of `len` cells with nothing after it.
    #[cfg(test)]
    const fn plain(len: usize) -> Self {
        Self {
            run: len,
            whole: len,
            tail: 0,
            ghost: false,
            stuck: false,
        }
    }

    /// The tail slot's cells, left of the switch: the ghost, a `─`, then Claude's tail (0: empty).
    const fn slot(&self) -> usize {
        match (self.ghost, self.tail) {
            (true, 0) => GHOST_CELLS,
            (true, tail) => GHOST_CELLS + 1 + tail,
            (false, tail) => tail,
        }
    }

    /// The tail slot and the `──` it keeps before the switch (0: an empty slot).
    const fn slot_block(&self) -> usize {
        match self.slot() {
            0 => 0,
            n => n + SLOT_RULE,
        }
    }
}

/// The rule's own `─` between the tail slot and the switch.
const SLOT_RULE: usize = 2;

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
        let span = TopSpan {
            run: top_span(voice.len, key.face),
            whole: voice.len,
            tail: 0,
            ghost: false,
            stuck: false,
        };
        lay_top(span, key.switch, lockup).is_some_and(|laid| laid.rung.lockup.is_some())
    });
    match whole {
        Some(VoiceRule { col, len, label }) => (
            TopRun {
                col,
                len,
                end: col + len,
                ..run
            },
            label,
        ),
        None => (run, 0),
    }
}

/// The ghost's first cell in a run of `len` cells under `face`, where it is drawn at the run's
/// end (no switch laid beside the tail).
fn ghost_at(len: usize, face: KeyFace) -> Option<usize> {
    face.ghost
        .filter(|_| len >= GHOST_RUN)
        .map(|_| len - GHOST_CELLS - 1)
}

/// The harness's span in a run of `len` cells under `face` ([`ghost_at`]).
fn top_span(len: usize, face: KeyFace) -> usize {
    ghost_at(len, face).unwrap_or(len)
}

/// Where a hosted program's top rule is on a pane's cached rows.
fn top_line(program: aterm_phase::Program, cached: &CachedPaneText) -> Option<usize> {
    use aterm_phase::Program;
    match program {
        Program::Claude | Program::Agy | Program::Muse | Program::Kimi => cached
            .readings
            .reach
            .and_then(|r| r.rules())
            .map(|(top, _)| top),
        Program::Codex | Program::Grok | Program::Generic => None,
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

/// The composer's top line: its longest plain run of rule cells (ties go right), and Claude Code's
/// label zone where it follows that run ([`Tail`]).
fn read_top_run(
    scratch: &RenderInput,
    region: PaneTextRegion,
    top_rule: usize,
    blank: RenderCell,
    muse: bool,
) -> Option<TopRun> {
    #[cfg(test)]
    crate::work_counts::footer_top_read();
    let frame_row = region.strip + region.row_off + top_rule;
    let frame_cells = scratch.cells.get(frame_row)?;
    let pane_end = frame_cells.len().min(region.col_off + region.cols);
    let pane_slice = frame_cells.get(region.col_off..pane_end)?;
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
    let untouched = |idx: usize| {
        let col = region.col_off + idx;
        !touches(&scratch.clusters, frame_row, &(col..col + 1))
            && !touches(&scratch.combining, frame_row, &(col..col + 1))
            && !touches(&scratch.images, frame_row, &(col..col + 1))
    };
    let mut best = 0..0;
    let mut cur_start = None;
    for (idx, cell) in pane_slice.iter().enumerate() {
        if *cell == rule && untouched(idx) {
            let s = *cur_start.get_or_insert(idx);
            if idx + 1 - s >= best.len() {
                best = s..idx + 1;
            }
        } else {
            cur_start = None;
        }
    }
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
    let tail = (!muse && !best.is_empty())
        .then(|| {
            let text: String = pane_slice.iter().map(|c| c.ch).collect();
            let (zone, close) = footer::top_rule_labels(&text)?;
            let cells = pane_slice.get(zone..close)?;
            (zone == best.end).then(|| {
                // A wide glyph (a CJK name, a one-character emoji) moves with its continuation:
                // the zone holds both halves of each, and no cell of it is a cluster's.
                let whole = cells.iter().enumerate().all(|(k, c)| {
                    let lead = aterm_grapheme::char_width(c.ch) == 2 && !c.wide;
                    (!c.wide || k > 0 && aterm_grapheme::char_width(cells[k - 1].ch) == 2)
                        && (!lead || cells.get(k + 1).is_some_and(|next| next.wide))
                });
                let plain =
                    (zone..=close).all(untouched) && whole && cells.iter().all(|c| c.ch != '\0');
                (
                    Tail {
                        col: region.col_off + zone,
                        len: close - zone,
                        term: tail_term(cells),
                        plain,
                    },
                    region.col_off + close + 1,
                )
            })
        })
        .flatten();
    Some(TopRun {
        col: region.col_off + best.start,
        len: best.len(),
        rule,
        voice,
        tail: tail.map(|(t, _)| t),
        end: tail.map_or(region.col_off + best.end, |(_, end)| end),
    })
}

/// A short text kept inline, so the keys that hold it stay `Copy` ([`SwitchPaint::cli_ver`]).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct Inline<const N: usize> {
    bytes: [u8; N],
    len: u8,
}

impl<const N: usize> Inline<N> {
    /// Nothing.
    pub(crate) const EMPTY: Self = Self {
        bytes: [0; N],
        len: 0,
    };

    /// `text`, cut after the last whole character that fits.
    pub(crate) fn new(text: &str) -> Self {
        let mut out = Self::EMPTY;
        let mut end = 0;
        for (at, ch) in text.char_indices() {
            let next = at + ch.len_utf8();
            if next > N || next > usize::from(u8::MAX) {
                break;
            }
            end = next;
        }
        out.bytes[..end].copy_from_slice(&text.as_bytes()[..end]);
        out.len = u8::try_from(end).unwrap_or(u8::MAX);
        out
    }

    /// `value`'s `Display`, cut as [`Self::new`] cuts, written in place: no allocation.
    pub(crate) fn of(value: impl std::fmt::Display) -> Self {
        use std::fmt::Write as _;
        let mut out = Self::EMPTY;
        // A cut ends the write: what is kept is whole characters.
        let _ = write!(out, "{value}");
        out
    }

    /// The text.
    pub(crate) fn as_str(&self) -> &str {
        std::str::from_utf8(&self.bytes[..usize::from(self.len)]).unwrap_or("")
    }

    pub(crate) const fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl<const N: usize> std::fmt::Write for Inline<N> {
    /// Appends `text`'s whole characters that fit; a cut is an error, which ends the write.
    fn write_str(&mut self, text: &str) -> std::fmt::Result {
        let len = usize::from(self.len);
        let room = N.min(usize::from(u8::MAX)).saturating_sub(len);
        let mut end = 0;
        for (at, ch) in text.char_indices() {
            let next = at + ch.len_utf8();
            if next > room {
                break;
            }
            end = next;
        }
        self.bytes[len..len + end].copy_from_slice(&text.as_bytes()[..end]);
        self.len = u8::try_from(len + end).unwrap_or(u8::MAX);
        if end < text.len() {
            Err(std::fmt::Error)
        } else {
            Ok(())
        }
    }
}

impl<const N: usize> Default for Inline<N> {
    fn default() -> Self {
        Self::EMPTY
    }
}

impl<const N: usize> std::fmt::Debug for Inline<N> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.as_str())
    }
}

/// The largest version accepted by codex-alab's bounded package metadata reader.
const CLI_VERSION_MAX: usize = 128;

/// What of a top line's harness the pointer or the keyboard is on ([`SwitchPaint::hover`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub(crate) enum HoverAt {
    #[default]
    None,
    /// The switch.
    Switch,
    /// The harness's label: its slot shows the CLI's whole build ([`label_whole`]) where the label
    /// leaves some of it out (a build past its numbers, or a tier that lays none of it).
    Label,
    /// The ⬆️ of a newer build of the CLI ([`TopUp`]): it is ringed, and the label's slot says
    /// when the move would land ([`up_words`]).
    Arrow,
}

/// What a top line's ⬆️ says of a newer build of the running CLI ([`TopUp`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(crate) enum UpState {
    /// No newer build waits for this tab: no arrow.
    #[default]
    None,
    /// A newer build is installed and its move waits: `⬆️`, a press asks for it now. A Claude
    /// Code tab's press is a one-shot restart its own lane moves
    /// ([`aterm_agent::harness::upgrade_drive::press`]): past its Autopilot or its harness
    /// switched off and the agent's own background work, so its words say what it waits for
    /// and what it ends ([`press_wait_on`]).
    Ready,
    /// A newer build waits, but nothing moves this tab now (another supervisor holds it, or for
    /// a CLI other than Claude Code its harness or its Autopilot is off): a quiet `↑`
    /// ([`HELD_ARROW`]), never the ⬆️ a press acts on; its words say when it would land and a
    /// press says why it cannot ([`TopUp::why`]), asking nothing.
    Held,
    /// The person asked for the move, or it is under way: `◌`, its words say so.
    Asked,
}

/// The newer build a top line's ⬆️ offers, read off the tab's upgrade row ([`TopUp::of`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(crate) struct TopUp {
    pub(crate) state: UpState,
    /// The build the move goes to, as the row states it (`2.1.310`).
    pub(crate) to: Inline<CLI_VERSION_MAX>,
    /// The move keeps the running build's numbers: its words name the build past them
    /// ([`aterm_phase::cli_version_build`]).
    pub(crate) revision: bool,
    /// When the move would land, in the lane's words ([`up_wait`]); empty where its wait has
    /// none.
    pub(crate) wait: &'static str,
    /// Why a Held arrow's move cannot be asked for now (`autopilot is off`); empty otherwise.
    pub(crate) why: &'static str,
    /// For a Claude Code row, whose press is a one-shot restart, what a press waits for as the
    /// lane's last look read the tab ([`press_wait_of`]; `""` for none); `None` for another
    /// CLI's, whose press is the tab menu's Upgrade Now.
    pub(crate) press: Option<&'static str>,
    /// The lane the row moves ([`aterm_agent::harness::upgrade_drive::Row::agent`]): a kept
    /// row's arrow shows only on a rule of that agent's CLI ([`SwitchPaint::of`]).
    pub(crate) agent: aterm_agent::harness::upgrade::Agent,
}

impl TopUp {
    /// The arrow of upgrade `row`: shown while the lane offers a build past the running one
    /// ([`aterm_phase::cli_version_offered`]: a later one, or a revision of it) that is pending,
    /// announced, or in a round that gave up unanswered, and the owner has not skipped it. Held
    /// while its tab is set aside (no worker moves it: [`held_words`]) but for an aside a press
    /// passes ([`press_passes`]), Asked once the owner's Now is written where it moves the tab
    /// or the move is under way, Ready otherwise. None for a row whose agent is not the
    /// foreground job in its tab ([`fronted`]): the rule is drawn by the program in front, so the
    /// row is another program's.
    pub(crate) fn of(row: &aterm_agent::harness::upgrade_drive::Row) -> Self {
        use aterm_agent::harness::upgrade::{GAVE_UP, Phase, Request};
        if !fronted(row) {
            return Self::default();
        }
        let skipped = row.request.skips_for(row.agent, &row.to);
        let waits = match &row.phase {
            Phase::Pending | Phase::Announced { .. } => true,
            Phase::Failed(why) => why == GAVE_UP,
            Phase::Exiting { .. } | Phase::Relaunched { .. } | Phase::Done => false,
        };
        let moving = matches!(row.phase, Phase::Exiting { .. } | Phase::Relaunched { .. });
        let passes = press_passes(row);
        let held = held_words(&row.aside).filter(|_| !passes);
        let state = if skipped || !aterm_phase::cli_version_offered(&row.to, &row.from) {
            UpState::None
        } else if moving {
            UpState::Asked
        } else if waits && held.is_some() {
            UpState::Held
        } else if waits && row.request == Request::Now && !passes {
            UpState::Asked
        } else if waits {
            UpState::Ready
        } else {
            UpState::None
        };
        if state == UpState::None {
            return Self::default();
        }
        let (wait, why) = match held.filter(|_| state == UpState::Held) {
            Some(held) => held,
            None => (up_wait(&row.wait), ""),
        };
        Self {
            state,
            to: Inline::new(&row.to),
            revision: !aterm_phase::cli_version_later(&row.to, &row.from),
            wait,
            why,
            press: row.agent.presses().then(|| press_wait_of(&row.wait)),
            agent: row.agent,
        }
    }

    /// The arrow of `row`, the last the host sent before it stood down with the harness off:
    /// Held until the harness is on, where the row would show one; a Claude Code row's as it
    /// stands, since a press moves its tab once with the harness off ([`press_passes`]).
    pub(crate) fn harness_off(row: &aterm_agent::harness::upgrade_drive::Row) -> Self {
        let up = Self::of(row);
        if !up.shows() || row.agent.presses() {
            return up;
        }
        let (wait, why) = HARNESS_OFF_WORDS;
        Self {
            state: UpState::Held,
            wait,
            why,
            ..up
        }
    }

    /// Whether the line lays the arrow.
    pub(crate) fn shows(&self) -> bool {
        self.state != UpState::None
    }

    /// Whether the arrow is the colour ⬆️ a press asks the move with ([`UpState::Ready`]).
    pub(crate) fn offers(&self) -> bool {
        self.state == UpState::Ready
    }

    /// The target build as its words name it: its numbers (`v2.1.310`), and the build past them
    /// where the move keeps the numbers (`v1.4.10 (R6500.1)`).
    fn target(&self) -> String {
        let raw = self.to.as_str();
        let core = aterm_phase::cli_version_core(raw).unwrap_or(raw);
        match aterm_phase::cli_version_build(raw).filter(|_| self.revision) {
            Some(build) => format!("v{core} ({build})"),
            None => format!("v{core}"),
        }
    }
}

/// When a held move would land and why a press cannot ask for it, for its tab's harness off.
const HARNESS_OFF_WORDS: (&str, &str) = (HELD[1].when, HELD[1].why);

/// Whether a press of `row`'s ⬆️ moves past the aside it stands under: a Claude Code tab's press
/// passes its Autopilot or its agent switched off, once
/// ([`aterm_agent::harness::upgrade_drive::press`]); it never passes another supervisor's hold.
fn press_passes(row: &aterm_agent::harness::upgrade_drive::Row) -> bool {
    use aterm_agent::harness::upgrade_drive::{AGENT_OFF, AUTOPILOT_OFF};
    row.agent.presses() && matches!(row.aside.as_str(), AGENT_OFF | AUTOPILOT_OFF)
}

/// What a press of a Claude Code tab's ⬆️ would wait for now on `t`'s live screen, read as the
/// press's own lane reads it ([`press_wait`]); `None` where the screen is not read (scrolled
/// back, mid-frame).
pub(crate) fn press_wait_on(t: &aterm_core::terminal::Terminal) -> Option<&'static str> {
    let draft = crate::harness_host::draft_shown(t)?;
    let rows: Vec<String> = (0..usize::from(t.rows()))
        .map(|r| crate::claude_lights::row_text(t, r))
        .collect();
    Some(press_wait(&rows, Some(t.title()), draft))
}

/// [`press_wait`]'s word for a row whose lane's last look waited on `wait`
/// ([`aterm_agent::harness::upgrade::wait_word`]): what of it a press waits for too, `""` for a
/// wait a press passes (the agent's own work, a break, the settle, a person's recent keys, an
/// Autopilot or a harness switched off). A status busy with no settle seen is read as a turn.
fn press_wait_of(wait: &str) -> &'static str {
    let wait = wait.strip_prefix("release:").unwrap_or(wait);
    let (head, status) = wait.split_once(':').unwrap_or((wait, ""));
    match (head, status) {
        ("box", _) | ("not-idle", "waiting") => "box",
        ("draft" | "left-typed", _) => "draft",
        ("limited", _) => "limited",
        ("login", _) => "login",
        ("held", _) => "held",
        ("busy" | "busy-thread", _) | ("not-idle", "busy") => "busy",
        _ => "",
    }
}

/// What a press of the ⬆️ waits for on screen `rows` under window title `title`, `draft` whether
/// its composer holds one: [`aterm_agent::harness::upgrade::press_gate`]'s word (a dialog, a
/// draft, a usage limit, a login, a running turn, the title's included:
/// [`aterm_agent::harness::upgrade_drive::turn_runs`]), `""` for a press that restarts at once.
fn press_wait(rows: &[String], title: Option<&str>, draft: bool) -> &'static str {
    use aterm_agent::harness::upgrade::{self, Gate};
    let facts = upgrade::Facts {
        composer_empty: !draft,
        approval_box: aterm_phase::prompt::prompt_box_span(rows).is_some(),
        busy_footer: aterm_agent::harness::upgrade_drive::turn_runs(rows, title),
        limited: upgrade::limited(rows),
        login: upgrade::login_wall(upgrade::Agent::Claude, rows),
        ..upgrade::Facts::default()
    };
    match upgrade::press_gate(&facts) {
        Gate::Go => "",
        Gate::Wait(wait) => wait,
    }
}

/// The words of a move its tab sets aside ([`held_words`]).
struct Held {
    /// The row's aside ([`aterm_agent::harness::upgrade_drive::Row::aside`]).
    aside: &'static str,
    /// When the move would land.
    when: &'static str,
    /// Why a press of the ⬆️ cannot ask for it.
    why: &'static str,
    /// [`Self::why`] in the fewest words, for a label slot too narrow for it.
    terse: &'static str,
    /// What lets the move be asked for, as the messages row says it after a press.
    then: &'static str,
}

/// Whether upgrade `row`'s agent was the foreground job in its tab at the lane's last look: a
/// row the lane set aside or held as not in front ([`aterm_agent::harness::upgrade_drive::
/// NOT_FOREGROUND`]: its process group is not the terminal's) names a program behind the one
/// drawing the rule, a suspended Claude Code under another, so no rule shows its arrow.
fn fronted(row: &aterm_agent::harness::upgrade_drive::Row) -> bool {
    use aterm_agent::harness::upgrade_drive::NOT_FOREGROUND;
    row.aside != NOT_FOREGROUND && row.wait != NOT_FOREGROUND
}

/// Every aside's words but not-in-front's, which no rule shows ([`fronted`]); an aside not named
/// here is [`HELD_ELSE`].
const HELD: [Held; 3] = {
    use aterm_agent::harness::upgrade_drive::{AGENT_OFF, AUTOPILOT_OFF, CLAIMED};
    [
        Held {
            aside: AUTOPILOT_OFF,
            when: "once autopilot is on",
            why: "autopilot is off",
            terse: "autopilot off",
            then: "Turn Autopilot on, then press the update arrow again",
        },
        Held {
            aside: AGENT_OFF,
            when: "once the harness is on",
            why: "the harness is off",
            terse: "harness off",
            then: "Turn the harness on, then press the update arrow again",
        },
        Held {
            aside: CLAIMED,
            when: "once its supervisor lets go",
            why: "another supervisor holds it",
            terse: "supervisor holds it",
            then: "Press the update arrow again once its supervisor lets go",
        },
    ]
};

/// The words of an aside [`HELD`] does not name.
const HELD_ELSE: Held = Held {
    aside: "",
    when: "",
    why: "it is set aside",
    terse: "set aside",
    then: "Press the update arrow again later",
};

/// When a move its tab's `aside` holds ([`aterm_agent::harness::upgrade_drive::Row::aside`])
/// would land, and why a press of the ⬆️ cannot ask for it: `None` for a row not set aside.
pub(crate) fn held_words(aside: &str) -> Option<(&'static str, &'static str)> {
    if aside.is_empty() {
        return None;
    }
    let held = HELD.iter().find(|h| h.aside == aside).unwrap_or(&HELD_ELSE);
    Some((held.when, held.why))
}

/// What lets a move held for `why` ([`TopUp::why`]) be asked for, as the messages row says it
/// after a press of its ⬆️ (`Turn Autopilot on, then press the update arrow again`).
pub(crate) fn held_then(why: &str) -> &'static str {
    HELD.iter()
        .find(|h| h.why == why)
        .unwrap_or(&HELD_ELSE)
        .then
}

/// Refusal words `why` in the fewest words, for a label slot too narrow for them: a held move's
/// (`autopilot off`), else the lane's
/// ([`crate::message_reporters::agent_upgrade_refused_terse`]).
fn terse_refusal(why: &str) -> Option<&'static str> {
    HELD.iter()
        .chain(std::iter::once(&HELD_ELSE))
        .find(|h| h.why == why)
        .map(|h| h.terse)
        .or_else(|| crate::message_reporters::agent_upgrade_refused_terse(why))
}

/// When a move held by `wait` ([`aterm_agent::harness::upgrade::wait_word`]) would land once
/// the owner asks for it, in the lane's words: an idle agent's at once, a turn's at its end,
/// background work's after it, a login's and a usage limit's after them, a person's typing and
/// draft once they are done (the words the bottom rule's upgrade note said before the ⬆️
/// carried them); `""` for a wait with no such words (a tab aterm cannot read). Only a running
/// turn is a turn's end: a busy footer, or a status that reads busy, as [`press_wait_of`] reads
/// one; a status waiting on a question is its dialog, a shell its background work, and a wait
/// for the tab to settle, for the agent to agree to the move, for its notice to be read or for
/// a status that lags or reads neither says what it waits for.
#[must_use]
pub(crate) fn up_wait(wait: &str) -> &'static str {
    use aterm_agent::harness::upgrade::{NEXT_POINT, NOTICE_SHOWN};
    let wait = wait.strip_prefix("release:").unwrap_or(wait);
    let (head, status) = wait.split_once(':').unwrap_or((wait, ""));
    match (head, status) {
        ("" | NEXT_POINT | "ready", _) => "now",
        ("background" | "background-terminal", _) | ("not-idle", "shell") => BACKGROUND_WAIT,
        ("busy" | "busy-thread", _) | ("not-idle", "busy") => TURN_WAIT,
        ("not-idle", "waiting") | ("box", _) => DIALOG_WAIT,
        ("not-idle" | "status-stale" | NOTICE_SHOWN, _) => IDLE_WAIT,
        ("settling", _) => SETTLE_WAIT,
        ("not-ready" | "awaiting-ready", _) => AGREE_WAIT,
        ("login", _) => "after login",
        ("limited", _) => "after the usage limit",
        ("attended", _) => "once you stop typing",
        ("draft" | "left-typed", _) => DRAFT_WAIT,
        ("held", _) => HOLD_WAIT,
        ("account", _) => ACCOUNT_WAIT,
        _ => "",
    }
}

/// [`up_wait`]'s words for a relaunch whose account cannot yet be checked at its shell (the
/// restart then ends nothing: `relaunch::WAIT_ACCOUNT`).
const ACCOUNT_WAIT: &str = "once its account can be checked";

/// [`up_wait`]'s words for a running turn.
const TURN_WAIT: &str = "at turn end";

/// [`up_wait`]'s words for an agent the lane has not yet read as idle, with no turn read running.
const IDLE_WAIT: &str = "once it is idle";

/// [`up_wait`]'s words for the quiet the lane waits for before it acts.
const SETTLE_WAIT: &str = "once its tab settles";

/// [`up_wait`]'s words for an agent asked to agree to the move.
const AGREE_WAIT: &str = "once it agrees to move";

/// [`up_wait`]'s words for a draft in the composer.
const DRAFT_WAIT: &str = "once your draft is sent or cleared";

/// [`up_wait`]'s words for a dialog on screen.
const DIALOG_WAIT: &str = "once its dialog is answered";

/// [`up_wait`]'s words for the tab's hold.
const HOLD_WAIT: &str = "once the tab's hold ends";

/// [`up_wait`]'s words `when` in the fewest, for a label slot too narrow for them
/// ([`up_forms`]): a wait for a draft, a dialog or a hold names what it waits for and no more
/// (`after your draft`); `None` for words as few already.
fn terse_wait(when: &str) -> Option<&'static str> {
    match when {
        DRAFT_WAIT => Some("after your draft"),
        DIALOG_WAIT => Some("after its dialog"),
        HOLD_WAIT => Some("after the hold"),
        _ => None,
    }
}

/// The running CLI's build a top line's label names ([`SwitchPaint::cli_ver`]), read off memory
/// the harness host and the session registry already filled: no file or process is read for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(crate) struct TopCli {
    /// The build as it states itself (`2.1.305`, `1.4.13-R6996.1`); empty where not known.
    pub(crate) ver: Inline<CLI_VERSION_MAX>,
    /// A newer build the tab's upgrade row offers ([`TopUp::of`]).
    pub(crate) up: TopUp,
    /// The arrow of the row the host last sent before it stood down ([`TopUp::harness_off`]),
    /// while its target is still past the running build: what a line with its harness off
    /// shows ([`SwitchPaint::of`]).
    pub(crate) kept: TopUp,
}

impl TopCli {
    /// What `timeline` knows of its agent's running build: the session registry's version (Claude
    /// Code's), then the build of a live upgrade row, then the build the running image's
    /// store directory names.
    pub(crate) fn from_timeline(timeline: &crate::session_timeline::SessionTimeline) -> Self {
        let live_from = timeline
            .upgrade()
            .filter(|row| upgrade_row_live(row) && fronted(row))
            .map(|row| row.from.as_str());
        let ver = timeline
            .claude_footer()
            .and_then(|facts| facts.version.as_deref())
            .filter(|v| !v.is_empty())
            .or(live_from.filter(|v| !v.is_empty()))
            .map(Inline::new)
            .or_else(|| timeline.running_build().map(Inline::of))
            .unwrap_or_default();
        let up = timeline.upgrade().map(TopUp::of).unwrap_or_default();
        let kept = timeline
            .upgrade_kept()
            .filter(|_| timeline.upgrade().is_none())
            .filter(|row| ver.is_empty() || same_cli_version(&row.from, ver.as_str()))
            .map(TopUp::harness_off)
            .unwrap_or_default();
        Self { ver, up, kept }
    }

    /// Its repaint term ([`App::claude_footer_fp`]).
    pub(crate) fn term(&self) -> u64 {
        let arrow = |up: &TopUp| {
            aterm_hash::fnv1a_64(up.to.as_str().as_bytes())
                ^ aterm_hash::fnv1a_64(up.wait.as_bytes()).rotate_left(1)
                ^ aterm_hash::fnv1a_64(up.why.as_bytes()).rotate_left(2)
                ^ aterm_hash::fnv1a_64(up.press.unwrap_or("-").as_bytes()).rotate_left(4)
                ^ (up.state as u64) << 60
                ^ u64::from(up.revision) << 59
                ^ (up.agent as u64) << 57
        };
        aterm_hash::fnv1a_64(self.ver.as_str().as_bytes())
            ^ arrow(&self.up).rotate_left(3)
            ^ arrow(&self.kept).rotate_left(7)
    }
}

/// Whether the running build still names a retained row's source. Simple dotted versions
/// accept their numeric aliases; revision and dev identities keep every component, since the
/// lane's ordering of one old move says nothing about a different image now running.
fn same_cli_version(from: &str, running: &str) -> bool {
    use aterm_agent::harness::upgrade::Version;
    if let (Some(from), Some(running)) = (Version::parse(from), Version::parse(running)) {
        return from == running;
    }
    fn bare(raw: &str) -> &str {
        raw.trim().strip_prefix('v').unwrap_or(raw.trim())
    }
    bare(from) == bare(running)
}

/// Whether an upgrade `row` still stands before its move: its `from` is the running build.
fn upgrade_row_live(row: &aterm_agent::harness::upgrade_drive::Row) -> bool {
    use aterm_agent::harness::upgrade::Phase;
    matches!(
        row.phase,
        Phase::Pending | Phase::Announced { .. } | Phase::Exiting { .. } | Phase::Failed(_)
    )
}

/// The harness switch's paint on one pane's composer top rule ([`compose_top`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SwitchPaint {
    pub(crate) face: crate::autopilot::HarnessSwitch,
    /// What the pointer or the keyboard is on.
    pub(crate) hover: HoverAt,
    /// The harness ([`aterm_phase::CliHarness`]): its version is in the label (`ALab v0.13`);
    /// `None` draws the switch with no label.
    pub(crate) harness: Option<aterm_phase::CliHarness>,
    /// The running CLI's version as its build states it ([`TopCli::ver`]): the label shows its
    /// numbers ([`aterm_phase::cli_version_core`]).
    pub(crate) cli_ver: Inline<CLI_VERSION_MAX>,
    /// A newer build of the CLI the line's ⬆️ offers ([`TopUp`]).
    pub(crate) up: TopUp,
    /// Claude's running jobs as its mode row counts them (`1 shell`,
    /// [`footer::ModeRow::job_words`]): what a move held by background work waits on, and what
    /// a press's restart ends ([`ends_words`]); empty where none is read.
    pub(crate) jobs: Inline<48>,
    /// What a press of a Claude Code ⬆️ would wait for now, read off its tab's live screen
    /// while the pointer is on it ([`press_wait_on`]): `Some("")` for a press that restarts at
    /// once; `None` where it is not read.
    pub(crate) press: Option<&'static str>,
    /// Why the owner's last press of the ⬆️ was refused, for a few seconds after
    /// (`claude_lights::REFUSAL_SHOWN`); empty otherwise.
    pub(crate) refused: Inline<64>,
    /// High Contrast forces the chrome's tones: the switch wears brackets (`[● on]`).
    pub(crate) forced: bool,
}

impl SwitchPaint {
    /// The paint of `face` for `agent`'s harness, its CLI's build `cli`, the pointer on `hover`:
    /// the live row's arrow, or with its harness off and no live arrow the row last kept's
    /// ([`TopCli::kept`]), each only where that row's lane moves this agent's CLI.
    pub(crate) fn of(
        face: crate::autopilot::HarnessSwitch,
        hover: HoverAt,
        agent: Option<aterm_phase::Program>,
        cli: TopCli,
    ) -> Self {
        let harness = agent.and_then(aterm_phase::Program::cli_harness);
        // A row's arrow shows only on a rule of the CLI its lane moves, live or kept.
        let own = |up: TopUp| {
            Some(up)
                .filter(|up| harness == up.agent.program().cli_harness())
                .unwrap_or_default()
        };
        let live = own(cli.up);
        let up = if face.is_off() && !live.shows() {
            own(cli.kept)
        } else {
            live
        };
        Self {
            face,
            hover,
            harness,
            cli_ver: cli.ver,
            up,
            jobs: Inline::EMPTY,
            press: None,
            refused: Inline::EMPTY,
            forced: crate::chrome_band::forced_chrome().is_some(),
        }
    }

    /// This paint with Claude's running jobs `words` ([`Self::jobs`]); none where they do not
    /// fit whole.
    pub(crate) fn with_jobs(mut self, words: &str) -> Self {
        self.jobs = if words.len() <= 48 {
            Inline::new(words)
        } else {
            Inline::EMPTY
        };
        self
    }

    /// This paint with what a press of its ⬆️ would wait for ([`Self::press`]).
    pub(crate) const fn with_press(mut self, wait: Option<&'static str>) -> Self {
        self.press = wait;
        self
    }

    /// This paint with the ⬆️'s refusal `words` in its label's slot.
    pub(crate) fn refused_with(mut self, words: Option<&str>) -> Self {
        self.refused = words.map(Inline::new).unwrap_or_default();
        self
    }

    /// A bare paint of `face` for `harness`, as the tests lay it.
    #[cfg(test)]
    pub(crate) const fn bare(
        face: crate::autopilot::HarnessSwitch,
        harness: Option<aterm_phase::CliHarness>,
    ) -> Self {
        Self {
            face,
            hover: HoverAt::None,
            harness,
            cli_ver: Inline::EMPTY,
            up: TopUp {
                state: UpState::None,
                to: Inline::EMPTY,
                revision: false,
                wait: "",
                why: "",
                press: None,
                agent: aterm_agent::harness::upgrade::Agent::Claude,
            },
            jobs: Inline::EMPTY,
            press: None,
            refused: Inline::EMPTY,
            forced: false,
        }
    }

    /// Whether the pointer or the keyboard is on the switch.
    fn hovered(&self) -> bool {
        self.hover == HoverAt::Switch
    }

    /// The running CLI's version as the label shows it (`None`: not known).
    fn cli_core(&self) -> Option<&str> {
        aterm_phase::cli_version_core(self.cli_ver.as_str())
    }
}

/// The cells the harness switch takes: ` ○ off `, its widest face. Every face lays in this slot,
/// right-aligned, so a flip moves nothing ([`switch_face`]).
const SWITCH_SLOT: usize = 7;

/// One harness switch as laid ([`switch_face`]): its state's mark and word alone, inked alike
/// ([`switch_cell`]).
#[derive(Clone, Debug, PartialEq, Eq)]
struct SwitchFace {
    cells: [char; SWITCH_SLOT],
    len: usize,
}

impl SwitchFace {
    fn cells(&self) -> &[char] {
        &self.cells[..self.len]
    }

    /// The face's text.
    #[cfg(test)]
    fn text(&self) -> String {
        self.cells().iter().collect()
    }

    /// The face's words, its marks and blanks left out: `on` or `off`.
    #[cfg(a11y_tree)]
    fn words(&self) -> String {
        let text: String = self.cells().iter().collect();
        text.split_whitespace()
            .filter(|w| w.chars().any(char::is_alphanumeric))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// The harness switch: its state's mark and word alone, ` ● on ` lit and ` ○ off ` held off.
/// Turning shows the side it turns to with `◌`, the glass's in-flight mark (` ◌ on `, ` ◌ off `,
/// [`crate::autopilot::IN_FLIGHT`]). Under High Contrast the
/// face wears brackets (`[● on]`). A press on its ink (its mark to its word's end, never a side
/// blank) turns the switch.
fn switch_face(paint: SwitchPaint) -> SwitchFace {
    use crate::autopilot::HarnessSwitch;
    let (mark, word) = match paint.face {
        HarnessSwitch::On => ('\u{25cf}', "on"),
        HarnessSwitch::Off(_) => ('\u{25cb}', "off"),
        HarnessSwitch::Turning => (crate::autopilot::IN_FLIGHT, "on"),
        HarnessSwitch::TurningOff => (crate::autopilot::IN_FLIGHT, "off"),
    };
    let (open, close) = if paint.forced { ('[', ']') } else { (' ', ' ') };
    let mut f = SwitchFace {
        cells: [' '; SWITCH_SLOT],
        len: 0,
    };
    for ch in [open, mark, ' ']
        .into_iter()
        .chain(word.chars())
        .chain([close])
    {
        if f.len < SWITCH_SLOT {
            f.cells[f.len] = ch;
            f.len += 1;
        }
    }
    f
}

/// How one cell of a harness label is inked ([`label_face`], [`label_cell`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LabelInk {
    /// A blank.
    Pad,
    /// The mark, `ALab`: the hero's ramp, bold.
    Brand,
    /// The quiet words, fine print: the harness's version `v0.5` and the CLI's `(v2.1.305)`.
    Muted,
    /// The build the ⬆️'s move goes to in its words: a version, lit and bold.
    Lit,
}

/// The cells a label, or the words in its slot, may take: the widest label
/// (`ALab v0.5 (v<build>)`) or the widest build its hover shows alone (`claude v<build>`).
const LABEL_MAX: usize = {
    let mut widest = 0;
    let mut i = 0;
    while i < aterm_phase::CliHarness::ALL.len() {
        let harness = aterm_phase::CliHarness::ALL[i];
        let label = aterm_types::identity::COMPANY.len()
            + " v".len()
            + harness.version_str().len()
            + " (v".len()
            + CLI_VERSION_MAX
            + ")".len();
        let build = harness.cli().len() + " v".len() + CLI_VERSION_MAX;
        let cells = if label > build { label } else { build };
        if cells > widest {
            widest = cells;
        }
        i += 1;
    }
    widest
};

/// How many forms a label has, widest first ([`label_face`]): `ALab v0.5 (v2.1.305)`, then
/// `ALab v0.5`, then the mark alone, `ALab`. The CLI is never named in it: the lockup at the
/// rule's other end names it.
const LABEL_TIERS: usize = 3;

/// How many of the label's tiers show the running CLI's build: the widest alone.
const LABEL_CLI_TIERS: usize = 1;

/// Whether the label laid at `tier` shows the running CLI's build (`(v2.1.305)`): the widest
/// tier alone ([`label_face`]).
const fn label_says_cli(tier: usize) -> bool {
    tier < LABEL_CLI_TIERS
}

/// Whether the label laid at `tier` shows the harness's version (`v0.5`): every tier but the
/// last, which is the mark alone ([`label_face`]).
const fn label_says_version(tier: usize) -> bool {
    tier + 1 < LABEL_TIERS
}

/// One harness label as laid ([`label_face`]), or the words its slot shows ([`slot_words`]).
#[derive(Clone, Debug, PartialEq, Eq)]
struct LabelFace {
    cells: [(char, LabelInk); LABEL_MAX],
    len: usize,
}

impl LabelFace {
    const EMPTY: Self = Self {
        cells: [(' ', LabelInk::Pad); LABEL_MAX],
        len: 0,
    };

    fn cells(&self) -> &[(char, LabelInk)] {
        &self.cells[..self.len]
    }

    /// The label's words.
    #[cfg(any(test, a11y_tree))]
    fn text(&self) -> String {
        self.cells().iter().map(|(ch, _)| *ch).collect()
    }

    fn push(&mut self, text: &str, ink: LabelInk) {
        for ch in text.chars() {
            self.push_cell(ch, if ch == ' ' { LabelInk::Pad } else { ink });
        }
    }

    fn push_cell(&mut self, ch: char, ink: LabelInk) {
        if self.len < LABEL_MAX {
            self.cells[self.len] = (ch, ink);
            self.len += 1;
        }
    }

    /// `words` (each with its ink), a character that is not one cell wide dropped.
    fn of_words(words: &[(String, LabelInk)]) -> Self {
        let mut whole = Self::EMPTY;
        let mut buf = [0u8; 4];
        for (text, ink) in words {
            for ch in text.chars() {
                if footer::text_width(ch.encode_utf8(&mut buf)) == 1 {
                    whole.push_cell(ch, if ch == ' ' { LabelInk::Pad } else { *ink });
                }
            }
        }
        whole
    }

    /// These cells right-aligned in `width` cells, `None` where they do not fit: a slot shows
    /// its words whole or not at all ([`SlotWords::cells`]).
    fn right_aligned(self, width: usize) -> Option<Self> {
        if self.len > width || width > LABEL_MAX {
            return None;
        }
        let mut out = Self::EMPTY;
        for _ in self.len..width {
            out.push_cell(' ', LabelInk::Pad);
        }
        for &(ch, ink) in self.cells() {
            out.push_cell(ch, ink);
        }
        Some(out)
    }
}

/// The harness label at `tier` (`ALab v0.5 (v2.1.305)`, then `ALab v0.5`, then `ALab`), `None`
/// past its narrowest, where a tier says no more than the next, or for a paint with no harness.
/// The mark is the label's one bold word, in the hero's ramp; both versions are fine print. The
/// running CLI's version is left out where it is not known: never `(v?)`. An asked move keeps
/// it: the label never grows under the press.
fn label_face(paint: &SwitchPaint, tier: usize) -> Option<LabelFace> {
    let harness = paint.harness?;
    let cli = paint.cli_core();
    if tier >= LABEL_TIERS || (label_says_cli(tier) && cli.is_none()) {
        return None;
    }
    let mut f = LabelFace::EMPTY;
    f.push(aterm_types::identity::COMPANY, LabelInk::Brand);
    if label_says_version(tier) {
        f.push(" v", LabelInk::Muted);
        f.push(harness.version_str(), LabelInk::Muted);
    }
    if let Some(from) = cli.filter(|_| label_says_cli(tier)) {
        f.push(" (v", LabelInk::Muted);
        f.push(from, LabelInk::Muted);
        f.push(")", LabelInk::Muted);
    }
    Some(f)
}

/// The label's forms with the CLI's whole build, in the label's own inks, as its hover shows
/// them on a label laid at `tier`: `ALab v0.5 (v1.4.13-R6996.1)`. A label laid without the CLI's
/// build (`ALab v0.5`, or the mark alone) shows the build alone instead, `claude v2.1.305` then
/// `v2.1.305`, since the label with the build is the form the widest tier would have laid had it
/// fitted. `None` where the label shows the build's numbers and the build says no more than them
/// (`2.1.305`), or where the build is not known.
fn label_whole(paint: &SwitchPaint, tier: usize) -> Option<Vec<Vec<(String, LabelInk)>>> {
    let harness = paint.harness?;
    let raw = paint.cli_ver.as_str().trim();
    let raw = raw.strip_prefix('v').unwrap_or(raw);
    let core = paint.cli_core()?;
    if !label_says_cli(tier) {
        return Some(vec![
            vec![(format!("{} v{raw}", harness.cli()), LabelInk::Muted)],
            vec![(format!("v{raw}"), LabelInk::Muted)],
        ]);
    }
    (raw != core).then(|| {
        vec![vec![
            (aterm_types::identity::COMPANY.to_owned(), LabelInk::Brand),
            (format!(" v{}", harness.version_str()), LabelInk::Muted),
            (format!(" (v{raw})"), LabelInk::Muted),
        ]]
    })
}

/// The wait words of a move held by background work ([`up_wait`]): the ⬆️ says Claude's own
/// counts in their place where its mode row gives them ([`SwitchPaint::jobs`]).
const BACKGROUND_WAIT: &str = "after background jobs";

/// What the ⬆️ says under the pointer ([`up_words`]), in its parts and their inks.
struct UpWords {
    /// `update to `, `updating to ` once asked.
    verb: (String, LabelInk),
    /// The target build, bold ([`TopUp::target`]).
    target: (String, LabelInk),
    /// When the move would land and, for a Claude Code restart, what it ends (` · after 1 shell`,
    /// ` · now · ends 1 shell`); empty with no when.
    when: (String, LabelInk),
    /// [`Self::when`] with its wait in the fewest words, where it has them ([`terse_wait`]:
    /// ` · after your draft · ends 1 shell`).
    terse: Option<(String, LabelInk)>,
    /// [`Self::terse`] without what the restart ends, where it names some (` · after your
    /// draft`): the wait kept where no rule holds the shells as well.
    bare: Option<(String, LabelInk)>,
}

/// What the ⬆️ says under the pointer, in its inks: `update to ` and the target build, bold
/// ([`TopUp::target`]), then when the move would land (` · after 1 shell`, Claude's `jobs`
/// counts for its background work), or for a Claude Code press ([`TopUp::press`], or `press`
/// read off the live screen, [`SwitchPaint::press`]) when it would land and what its restart
/// ends ([`press_when`]); `updating to …` once asked, a Claude Code tab's naming what its
/// restart ends unless it waits for that work; `None` with no arrow.
fn up_words(up: &TopUp, jobs: &str, press: Option<&'static str>) -> Option<UpWords> {
    if !up.shows() {
        return None;
    }
    let ends = aterm_agent::harness::upgrade::RESTART_ENDS_BACKGROUND;
    let words = |verb: &str, when: String, terse: Option<(String, String)>| {
        let (terse, bare) = terse.map_or((None, None), |(terse, bare)| {
            let bare = (bare != terse).then_some((bare, LabelInk::Muted));
            (Some((terse, LabelInk::Muted)), bare)
        });
        UpWords {
            verb: (verb.to_owned(), LabelInk::Muted),
            target: (up.target(), LabelInk::Lit),
            when: (when, LabelInk::Muted),
            terse,
            bare,
        }
    };
    // A Claude Code press's words: the live screen's reading where one was taken, else the
    // lane's last look's.
    if let Some(press) = up
        .press
        .map(|seen| press.unwrap_or(seen))
        .filter(|_| up.state == UpState::Ready)
    {
        let when = match up_wait(press) {
            "" => "now",
            when => when,
        };
        return Some(words(
            "update to ",
            press_when(when, jobs, ends),
            terse_wait(when).map(|terse| {
                (
                    press_when(terse, jobs, ends),
                    press_when(terse, jobs, false),
                )
            }),
        ));
    }
    let verb = if up.state == UpState::Asked {
        "updating to "
    } else {
        "update to "
    };
    // A held move with no when says why it is held.
    let wait = match up.wait {
        "" if up.state == UpState::Held => up.why,
        wait => wait,
    };
    // A Claude Code tab's move under way is a restart: it ends the shells it does not wait for.
    let restart = up.state == UpState::Asked && up.press.is_some();
    let ends = restart && ends && wait != BACKGROUND_WAIT;
    let when = |wait: &str, ends: bool| {
        let when = match wait {
            "" => String::new(),
            BACKGROUND_WAIT if !jobs.is_empty() => format!(" \u{b7} after {jobs}"),
            wait => format!(" \u{b7} {wait}"),
        };
        when + &ends_words(jobs, ends)
    };
    Some(words(
        verb,
        when(wait, ends),
        terse_wait(wait).map(|terse| (when(terse, ends), when(terse, false))),
    ))
}

/// When a press of the ⬆️ would land, `when` in [`up_wait`]'s words (`now` for a press that
/// waits for nothing), and what its restart ends of Claude's background `jobs` where a restart
/// `ends` them ([`ends_words`]): ` · now · ends 1 shell`, ` · at turn end`.
fn press_when(when: &str, jobs: &str, ends: bool) -> String {
    format!(" \u{b7} {when}{}", ends_words(jobs, ends))
}

/// What a press's restart ends of Claude's background `jobs` (its mode row's count, `1 shell`)
/// where a restart `ends` them ([`aterm_agent::harness::upgrade::RESTART_ENDS_BACKGROUND`]):
/// ` · ends 1 shell`; empty with none, or where they live on.
pub(crate) fn ends_words(jobs: &str, ends: bool) -> String {
    if ends && !jobs.is_empty() {
        format!(" \u{b7} ends {jobs}")
    } else {
        String::new()
    }
}

/// [`up_words`] as one sentence, as the ⬆️'s button describes it.
pub(crate) fn up_said(up: &TopUp, jobs: &str, press: Option<&'static str>) -> Option<String> {
    let UpWords {
        verb, target, when, ..
    } = up_words(up, jobs, press)?;
    Some(verb.0 + &target.0 + &when.0)
}

/// The forms of the ⬆️'s words ([`up_words`]), the most said first, each shorter than the one
/// before where the words have a when: the verb, the target and when; the target and when (an
/// asked move's led by [`ASKED_MARK`], `→ v2.1.310 · after 1 shell`, so it never reads as the
/// offer before the press); the target and when in the fewest words where it has them
/// (`v2.1.311 · after your draft`), a restart's naming what it ends and then again without it;
/// then for a held move the target and why in the fewest words (`v2.1.310 · autopilot off`)
/// and those words alone, else the verb and the target where that is shorter than the form
/// before it (a short when, `· now`, says more in fewer cells than the verb), then the target
/// alone (an asked move's marked, then bare). Every form keeps what the arrow is for: the
/// build, or why it cannot move. The forms with a when are the ones a slot shows wherever they
/// fit ([`SlotWords`]): when the move lands and, for a Claude Code restart (all but the last of
/// a wait in the fewest words), what it ends, which the forms after them cut.
fn up_forms(up: &TopUp, jobs: &str, press: Option<&'static str>) -> Option<SlotWords> {
    let UpWords {
        verb,
        target,
        when,
        terse,
        bare,
    } = up_words(up, jobs, press)?;
    let asked = up.state == UpState::Asked;
    let marked = |form: Vec<(String, LabelInk)>| {
        if asked {
            std::iter::once((ASKED_MARK.to_owned(), LabelInk::Muted))
                .chain(form)
                .collect()
        } else {
            form
        }
    };
    let cells = |form: &[(String, LabelInk)]| -> usize {
        form.iter().map(|(text, _)| text.chars().count()).sum()
    };
    let mut forms = vec![vec![verb.clone(), target.clone(), when.clone()]];
    if !when.0.is_empty() {
        forms.push(marked(vec![target.clone(), when]));
        forms.extend(terse.map(|terse| marked(vec![target.clone(), terse])));
        forms.extend(bare.map(|bare| marked(vec![target.clone(), bare])));
    }
    // Every form that says when the move lands says what it must: the label's own cells are a
    // short version (`ALab v0.5 (v2.1.305)`), so a form with the when grows left into the
    // rule's room before a form without it takes them.
    let whole = if forms.len() > 1 { forms.len() } else { 0 };
    let terse = (up.state == UpState::Held)
        .then(|| terse_refusal(up.why))
        .flatten();
    match terse {
        Some(terse) => {
            forms.push(vec![target, (format!(" \u{b7} {terse}"), LabelInk::Muted)]);
            forms.push(vec![(terse.to_owned(), LabelInk::Muted)]);
        }
        None => {
            let named = vec![verb, target.clone()];
            if forms.len() > 1 && forms.last().is_some_and(|last| cells(&named) < cells(last)) {
                forms.push(named);
            }
            if asked {
                forms.push(marked(vec![target.clone()]));
            }
            forms.push(vec![target]);
        }
    }
    Some(SlotWords(forms, whole))
}

/// What leads an asked move's shorter forms in place of its verb `updating to` ([`up_forms`]):
/// the move under way, in one cell and a blank.
const ASKED_MARK: &str = "\u{2192} ";

/// The words a label's slot shows instead of the label ([`slot_words`]): their forms, the most
/// said first, each shown whole or not at all, and how many of the first say what they must (a
/// move's when, and for a Claude Code restart what it ends where one fits, [`up_forms`]; 0 where
/// every form will do).
struct SlotWords(Vec<Vec<(String, LabelInk)>>, usize);

impl SlotWords {
    /// The cells the slot shows in place of a label `label` cells wide that may grow left into
    /// `room` cells. The forms that say what they must come first, each in turn shown wherever it
    /// fits: right-aligned in the label's own cells, else grown left into the room to hold it
    /// and no more, so the most said of them (the verb's, `update to v2.1.311 · now · ends 1
    /// shell`) grows before a shorter one (`v2.1.311 · now · ends 1 shell`) takes the label's
    /// cells. Of the rest, the first that fits the label's own cells, right-aligned in them, so
    /// nothing beside the slot moves; where none does, the first that fits the room. `None`
    /// where no form fits: the label stands, and nothing in the slot is ever cut.
    fn cells(&self, label: usize, room: usize) -> Option<LabelFace> {
        // A form with a character the cells cannot hold (past [`LABEL_MAX`], or not one cell
        // wide) is never shown: it would be cut.
        let forms = |whole: bool| {
            self.0.iter().enumerate().filter_map(move |(at, form)| {
                let cells = LabelFace::of_words(form);
                let chars: usize = form.iter().map(|(text, _)| text.chars().count()).sum();
                ((at < self.1) == whole && cells.len == chars).then_some(cells)
            })
        };
        let fit = |cells: LabelFace| {
            if cells.len <= label {
                cells.right_aligned(label)
            } else {
                Some(cells)
            }
        };
        if let Some(whole) = forms(true).find(|cells| cells.len <= label.max(room)) {
            return fit(whole);
        }
        forms(false)
            .find(|cells| cells.len <= label)
            .or_else(|| forms(false).find(|cells| cells.len <= room))
            .and_then(fit)
    }
}

/// The words a label's slot shows instead of the label: why the ⬆️'s press was refused, for a
/// few seconds after (`couldn't update · <why>`, else the reason alone, else it in the fewest
/// words, [`terse_refusal`]); while the pointer is on the ⬆️ its words ([`up_forms`]); on the
/// label laid at `tier` its whole build ([`label_whole`]).
fn slot_words(paint: &SwitchPaint, tier: usize) -> Option<SlotWords> {
    if !paint.refused.is_empty() {
        let why = paint.refused.as_str();
        let words = |text: String| vec![(text, LabelInk::Muted)];
        let mut forms = vec![
            words(format!("couldn't update \u{b7} {why}")),
            words(why.to_owned()),
        ];
        forms.extend(terse_refusal(why).map(|terse| words(terse.to_owned())));
        // The lead says the press failed: it grows into the rule's room before the reason
        // alone takes the label's cells.
        return Some(SlotWords(forms, 1));
    }
    match paint.hover {
        HoverAt::Label => label_whole(paint, tier).map(|forms| SlotWords(forms, 0)),
        HoverAt::Arrow => up_forms(&paint.up, paint.jobs.as_str(), paint.press),
        HoverAt::None | HoverAt::Switch => None,
    }
}

/// The inks a top line's switch and label are drawn in ([`switch_cell`], [`label_cell`]), the
/// glass's tokens ([`GlassInks`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FaceInks {
    /// The harness's version, the ⬆️'s target build, the ⬆️, and the switch under the pointer,
    /// the keyboard or High Contrast.
    lit: [u8; 3],
    /// The switch at rest, the label's quiet words, and a held move's `↑`.
    muted: [u8; 3],
    /// The mark's letters ([`Self::brand`]).
    glass: GlassInks,
}

impl FaceInks {
    /// The inks of a top line in the glass's tokens.
    fn of(glass: &GlassInks) -> Self {
        Self {
            lit: rgb_of(glass.lit()),
            muted: rgb_of(glass.quiet()),
            glass: *glass,
        }
    }

    /// The mark's letter at `k` across it (`0.0..=1.0`): the launch card's settled hero
    /// ([`GlassInks::hero`]); with the harness off the pane wears no palette, so the lit ink.
    fn brand(&self, k: f32) -> [u8; 3] {
        rgb_of(self.glass.hero(k))
    }
}

/// A switch cell on `blank`'s ground ([`FaceInks`]): the quiet ink at rest, the lit ink under
/// High Contrast, which asks for the stronger tone, and never bold. While the pointer or the
/// keyboard is on the switch its `inked` cells (its mark to its word's end, [`inked`]) wear the
/// glass's one hover (§1.5). Every cell keeps the ground and no sprite lies under it, so the
/// switch draws no box.
fn switch_cell(
    ch: char,
    inked: bool,
    paint: &SwitchPaint,
    inks: &FaceInks,
    blank: RenderCell,
) -> RenderCell {
    let cell = RenderCell {
        ch,
        fg: if paint.forced { inks.lit } else { inks.muted },
        bold: false,
        underline: aterm_core::terminal::UnderlineStyle::None,
        ..blank
    };
    if inked && paint.hovered() {
        crate::claude_lights::hovered_cell(&inks.glass, cell, HoverPart::Text)
    } else {
        cell
    }
}

/// The cells of `face` its ink lies in: from its first glyph to its last, the blanks between
/// them too and neither side blank. A hover underlines these, and they are the element's hit.
fn inked(face: &[char]) -> std::ops::Range<usize> {
    let first = face.iter().position(|ch| *ch != ' ').unwrap_or(0);
    let last = face
        .iter()
        .rposition(|ch| *ch != ' ')
        .map_or(first, |k| k + 1);
    first..last
}

/// A label cell in its ink on `blank`'s ground ([`FaceInks`]); a brand letter takes the hero's
/// letter at `k` across the brand.
fn label_cell(ch: char, ink: LabelInk, k: f32, inks: &FaceInks, blank: RenderCell) -> RenderCell {
    let mut cell = RenderCell {
        ch,
        bold: false,
        underline: aterm_core::terminal::UnderlineStyle::None,
        ..blank
    };
    match ink {
        LabelInk::Pad => cell.ch = ' ',
        LabelInk::Brand => {
            cell.fg = inks.brand(k);
            cell.bold = true;
        }
        LabelInk::Muted => cell.fg = inks.muted,
        LabelInk::Lit => {
            cell.fg = inks.lit;
            cell.bold = true;
        }
    }
    cell
}

/// Adjusts `ink` along its hue to reach at least 4.5:1 contrast against `ground`.
pub(crate) fn legible_on(ink: [u8; 3], ground: [u8; 3]) -> [u8; 3] {
    floored_on(ink, ground, aterm_effects::cli_marks::NAME_CONTRAST)
}

/// A cell of Claude's that aterm lays again on its glass (a kept item of the mode row, the tag
/// moved left of the switch), its ink floored along its hue to read as words do on its own
/// ground ([`legible_on`]): Claude's dark-theme cyan and purple, carried onto a light page,
/// read there. An ink that reads already, and a blank's, which draws nothing, stay as Claude
/// drew them, so on a dark page every cell is Claude's.
fn relaid(c: &RenderCell) -> RenderCell {
    if c.ch == ' ' {
        return *c;
    }
    RenderCell {
        fg: legible_on(c.fg, c.bg),
        ..*c
    }
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
#[cfg(test)]
fn reads_on(ink: [u8; 3], ground: [u8; 3]) -> f32 {
    aterm_effects::color_math::contrast_ratio(pack_rgb(ink), pack_rgb(ground))
}

/// `c` as `0x00RRGGBB`.
const fn pack_rgb(c: [u8; 3]) -> u32 {
    u32::from_be_bytes([0, c[0], c[1], c[2]])
}

/// The mark a program's top line wears ([`aterm_effects::cli_marks`]).
const fn cli_mark(program: aterm_phase::Program) -> Option<CliMark> {
    match program {
        aterm_phase::Program::Claude => Some(CliMark::Claude),
        aterm_phase::Program::Agy => Some(CliMark::Agy),
        aterm_phase::Program::Muse => Some(CliMark::Muse),
        aterm_phase::Program::Kimi => Some(CliMark::Kimi),
        aterm_phase::Program::Codex
        | aterm_phase::Program::Grok
        | aterm_phase::Program::Generic => None,
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

/// Where a rung lays a top line's harness ([`Rung::field`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Field {
    /// The whole rule to its last cell: Claude Code's label zone and aterm's ghost move left of
    /// the switch, into the tail slot ([`TopSpan::slot`]).
    End,
    /// The plain run alone, left of a label zone that stays where Claude drew it (and of the ghost
    /// at the run's end): the switch where the scan put it.
    Run,
}

/// One rung of the narrow ladder ([`ladder`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Rung {
    /// Whether the rung lays the switch ([`switch_face`]).
    switch: bool,
    /// The label's tier ([`label_face`]), `None` with no label.
    label: Option<usize>,
    lockup: Option<Lockup>,
    lead: usize,
    /// The rule's own `─` the rung keeps before the switch and after the last of its parts.
    rule: usize,
    close: usize,
    field: Field,
    need: usize,
}

/// The fewest of the rule's own `─` between the lockup (or the line's start) and what follows it.
const RULE_MIN: usize = 3;

/// The cells after a laid harness: a blank, then `──` in the arc's end ink.
const CLOSE: usize = 3;

/// The cells a laid ⬆️ takes ([`TopUp`]): a blank, then its two.
const ARROW: usize = 3;

/// The most rungs a ladder has: a label tier each, the switch with the lockup, with its
/// icon and bare, and the last resort.
const LADDER_MAX: usize = LABEL_TIERS + 4;

/// A top line's harness as laid ([`lay_top`]): its rung, its switch and its label.
#[derive(Debug, Clone)]
struct Laid {
    rung: Rung,
    face: Option<SwitchFace>,
    label: Option<LabelFace>,
}

/// The narrow ladder of a top line whose switch is `switch`, its lockup `lockup` cells wide, on
/// `span`, widest rung first ([`lay_top`]). Words go first: the CLI's build, the label, then the
/// lockup's name, then its icon (with the ⬆️ laid beside a known build, the lockup's name before
/// the CLI's build); Claude's tail is never cut, and the last resort lays the switch in the run
/// left of it.
fn ladder(
    span: TopSpan,
    switch: Option<SwitchPaint>,
    lockup: Option<usize>,
) -> ([Rung; LADDER_MAX], usize) {
    let none = Rung {
        switch: false,
        label: None,
        lockup: None,
        lead: 0,
        rule: 0,
        close: 0,
        field: Field::Run,
        need: usize::MAX,
    };
    let mut rungs = [none; LADDER_MAX];
    let mut n = 0;
    let mut push = |rung: Rung| {
        rungs[n] = rung;
        n += 1;
    };
    let icon = aterm_effects::cli_marks::ICON_RUN;
    let Some(paint) = switch else {
        if let Some(cells) = lockup {
            for (lockup, cells, lead) in [
                (Lockup::Full, cells, 2),
                (Lockup::Icon, icon, 2),
                (Lockup::Icon, icon, 1),
            ] {
                push(Rung {
                    lockup: Some(lockup),
                    lead,
                    rule: 2,
                    need: lead + cells + if lead == 1 { 1 } else { 2 },
                    ..none
                });
            }
        }
        return (rungs, n);
    };
    let field = if span.stuck { Field::Run } else { Field::End };
    let slot = match field {
        Field::End => span.slot_block(),
        Field::Run => 0,
    };
    // The ⬆️ never drops while a rung is laid.
    let arrow = if paint.up.shows() { ARROW } else { 0 };
    let rung = |label: Option<(usize, usize)>, lockup: Option<(Lockup, usize)>| {
        let lead = if lockup.is_some() { 2 } else { 0 };
        Rung {
            switch: true,
            label: label.map(|(tier, _)| tier),
            lockup: lockup.map(|(l, _)| l),
            lead,
            rule: RULE_MIN,
            close: CLOSE,
            field,
            need: lead
                + lockup.map_or(0, |(_, cells)| cells)
                + RULE_MIN
                + slot
                + SWITCH_SLOT
                + label.map_or(0, |(_, len)| 1 + len)
                + arrow
                + CLOSE,
        }
    };
    let full = lockup.map(|cells| (Lockup::Full, cells));
    let iconed = lockup.map(|_| (Lockup::Icon, icon));
    // With the ⬆️ laid beside a known build, the lockup's name goes before the label's CLI build:
    // the running build stays beside the offer of the next one. With no build to keep, the name
    // stays as long as the label does.
    let named_tiers = if arrow > 0 && lockup.is_some() && paint.cli_core().is_some() {
        LABEL_CLI_TIERS
    } else {
        LABEL_TIERS
    };
    for tier in 0..LABEL_TIERS {
        if let Some(face) = label_face(&paint, tier) {
            if tier < named_tiers {
                push(rung(Some((tier, face.len)), full));
            } else {
                if tier == named_tiers
                    && let Some(before) = label_face(&paint, tier - 1)
                {
                    push(rung(Some((tier - 1, before.len)), iconed));
                }
                push(rung(Some((tier, face.len)), iconed));
            }
        }
    }
    if named_tiers == LABEL_TIERS {
        push(rung(None, full));
    }
    if lockup.is_some() {
        push(rung(None, iconed));
    }
    push(rung(None, None));
    // The last resort needs the switch's own cells, as hb-int's narrowest switch did: a switch
    // stands wherever one stood.
    push(Rung {
        switch: true,
        rule: 2,
        close: 2,
        field: Field::Run,
        need: 2 + switch_face(paint).len + arrow + 2,
        ..none
    });
    (rungs, n)
}

/// The cells `rung` may lay in on `span`.
const fn field_len(span: TopSpan, rung: &Rung) -> usize {
    match rung.field {
        Field::End => span.whole,
        Field::Run => span.run,
    }
}

/// The first rung of [`ladder`] that fits `span`, and its switch and label.
fn lay_top(span: TopSpan, switch: Option<SwitchPaint>, lockup: Option<usize>) -> Option<Laid> {
    let (rungs, n) = ladder(span, switch, lockup);
    let rung = rungs[..n]
        .iter()
        .copied()
        .find(|r| r.need <= field_len(span, r))?;
    let face = switch.filter(|_| rung.switch).map(switch_face);
    let label = rung
        .label
        .zip(switch)
        .and_then(|(tier, paint)| label_face(&paint, tier));
    Some(Laid { rung, face, label })
}

/// What a composer top line is composed from ([`compose_top`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TopKey {
    frame_row: usize,
    term_row: usize,
    /// The run and Claude Code's label zone after it ([`Tail::term`] keys its cells).
    run: TopRun,
    program: aterm_phase::Program,
    build: Build,
    /// The switch, the pointer's place on the harness and the running CLI's build.
    switch: Option<SwitchPaint>,
    palette: Option<aterm_effects::harness_card::CardPalette>,
    blank: RenderCell,
    /// Where the line sits across its pane, for each cell's hue.
    field: PaneField,
    face: KeyFace,
    cell: (u16, u16),
    clip: aterm_core::render::PxRect,
}

#[cfg(test)]
impl TopKey {
    /// The glass's tokens this line wears ([`compose_top`]).
    fn glass(&self) -> GlassInks {
        GlassInks::of(
            self.palette,
            pack_rgb(self.blank.bg),
            pack_rgb(self.blank.fg),
            pack_rgb(self.run.rule.fg),
        )
    }

    /// The marks ink a mark at frame column `col` of this line wears ([`GlassInks::mark`]).
    fn mark_ink_at(&self, col: usize) -> [u8; 3] {
        rgb_of(self.glass().mark(self.field.t(col)))
    }
}

/// A pane's span of frame columns, across which each glass cell takes its column's hue
/// ([`aterm_effects::glass_inks::field`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PaneField {
    /// The pane's first frame column.
    col_off: usize,
    /// Its width.
    cols: usize,
}

impl PaneField {
    /// The palette position of frame column `col`.
    fn t(self, col: usize) -> f32 {
        aterm_effects::glass_inks::field(col.saturating_sub(self.col_off), self.cols)
    }
}

/// What aterm draws for the top-effort key on a top rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct KeyFace {
    /// aterm's ` KEY off ` ghost in Claude's slot, the key off: whether the pointer or the
    /// keyboard's selection is on it.
    ghost: Option<bool>,
    /// Claude's own tag on the row.
    tag: Option<(usize, usize)>,
    /// The pointer or the keyboard's selection is on Claude's tag: it wears the glass's hover
    /// where a laid switch moved Claude's zone and aterm writes the tag's cells
    /// ([`compose_top`]); where the zone stays Claude's own cells, nothing of it is rewritten.
    tag_lit: bool,
    /// `◌` while a press of the key is in flight.
    mark: Option<char>,
}

/// The ghost's cells.
const GHOST_CELLS: usize = aterm_primer::CLAUDE_TOP_EFFORT_KEY.len() + 6;

/// The shortest run the ghost is drawn in at the run's end: itself, the `─` after it and the
/// rule's lead of `─` before it.
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
    /// Where Claude Code's top-effort tag stands once its label zone moved left of the switch:
    /// frame columns, its hit's ([`TopHit`]).
    tag: Option<(usize, usize)>,
    lockup: std::ops::Range<usize>,
    icon: Option<usize>,
    mark: Option<aterm_core::render::FxLayer>,
    /// The ⬆️'s ring under the pointer ([`ring_layer`]).
    ring: Option<aterm_core::render::FxLayer>,
    /// The [`goal_term`] of the goal it was composed with (0: none).
    goal: u64,
    /// Where the active goal was laid ([`lay_goal`]): frame columns.
    goal_cols: Option<std::ops::Range<usize>>,
}

/// Where [`compose_top`] laid a harness switch ([`switch_face`]) and its label ([`label_face`]).
#[derive(Debug, Clone, PartialEq, Eq)]
struct LaidSwitch {
    /// The switch's face, its side blanks included.
    cols: std::ops::Range<usize>,
    /// Its inked cells, its mark to its word's end ([`inked`]): a press anywhere on them turns
    /// it, and a hover underlines exactly these.
    ink: std::ops::Range<usize>,
    /// The label's cells and tier: a hover shows its whole words, a press does nothing.
    label: Option<(std::ops::Range<usize>, usize)>,
    /// The ⬆️'s cells with the blank on either side ([`TopUp`]): a press asks for the move now.
    pub(crate) arrow: Option<std::ops::Range<usize>>,
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

/// The ⬆️ of a newer build ([`TopUp`]), drawn with emoji presentation over two cells.
pub(crate) const UP_ARROW: char = '\u{2b06}';

/// A held move's arrow ([`UpState::Held`]): `↑`, one cell in the label's quiet ink.
pub(crate) const HELD_ARROW: char = '\u{2191}';

/// Compose the CLI's top line ([`top_line`]), laid from its right end: `──`, the label, the
/// switch, `──`, Claude Code's label zone moved whole from `tail_cells` (the grid's cells of it, as
/// [`Tail`] read them, each ink floored to read as words do, [`relaid`]) or aterm's ghost, the
/// arc rule with the agent's active `goal` on it ([`lay_goal`]), and the lockup.
#[expect(
    clippy::too_many_lines,
    reason = "one line's layout, right to left, read as one pass"
)]
fn compose_top(key: &TopKey, top: &mut ComposedTop, goal: Option<&str>, tail_cells: &[RenderCell]) {
    #[cfg(test)]
    crate::work_counts::footer_top_composed();
    top.cells.clear();
    top.glass = 0..0;
    top.switch = None;
    top.ghost = None;
    top.tag = None;
    top.lockup = 0..0;
    top.icon = None;
    top.goal_cols = None;
    let TopKey {
        program,
        build,
        switch,
        palette,
        blank,
        field: across,
        face,
        ..
    } = *key;
    let (mut run, voice_label) = laid_run(key);
    // A tail whose cells are not at hand as the scan read them stays where Claude drew it.
    if let Some(t) = run.tail.as_mut()
        && tail_cells.len() != t.len
    {
        t.plain = false;
    }
    top.col = run.col;
    let rule_cell = run.rule;
    let tint = palette;
    let glass = GlassInks::of(
        palette,
        pack_rgb(blank.bg),
        pack_rgb(blank.fg),
        pack_rgb(rule_cell.fg),
    );
    // A mark at cell `at` of the run, in the mark ink of its column.
    let mark_ink = |at: usize| rgb_of(glass.mark(across.t(run.col + at)));
    let span = run.span(face);
    let mark = cli_mark(program);
    let mark_cell = |at: usize| {
        face.mark.map(|ch| RenderCell {
            ch,
            fg: mark_ink(at),
            bold: false,
            ..rule_cell
        })
    };
    // The ghost at rest in the quiet ink; under the pointer or the keyboard's selection the
    // glass's one hover (§1.5) from its first glyph to its last, the blank between them too,
    // and neither blank at its ends.
    let ghost_cells = |lit: bool| {
        let fg = rgb_of(glass.quiet());
        ghost_chars().enumerate().map(move |(k, ch)| {
            let cell = RenderCell {
                ch,
                fg,
                bold: false,
                script: aterm_core::terminal::Script::Normal,
                underline: aterm_core::terminal::UnderlineStyle::None,
                ..rule_cell
            };
            if lit && (1..GHOST_CELLS - 1).contains(&k) {
                crate::claude_lights::hovered_cell(&glass, cell, HoverPart::Text)
            } else {
                cell
            }
        })
    };
    // The key's mark in the run, where nothing moves: the cell before the ghost at the run's end,
    // or two before Claude's tag when the tag opens its label zone.
    let mark_in_run = |ghost_end: Option<usize>| {
        face.mark.and_then(|_| match ghost_end {
            Some(at) => at.checked_sub(1),
            None => face
                .tag
                .and_then(|(start, _)| start.checked_sub(2)?.checked_sub(run.col))
                .filter(|&at| at < run.len),
        })
    };
    let Some(laid) = lay_top(span, switch, mark.map(|m| m.lockup_cells(build))) else {
        let ghost_end = ghost_at(run.len, face);
        let mark_at = mark_in_run(ghost_end);
        if let Some(first) = mark_at.or(ghost_end) {
            top.col = run.col + first;
            if let Some(mark) = mark_at.and_then(mark_cell) {
                top.cells.push(mark);
            }
            if let (Some(at), Some(lit)) = (ghost_end, face.ghost) {
                top.ghost = Some((run.col + at + 1, run.col + at + GHOST_CELLS - 1));
                top.cells.extend(ghost_cells(lit));
            }
        }
        return;
    };
    let rung = laid.rung;
    let field = field_len(span, &rung);
    let moved = rung.field == Field::End && rung.switch;
    let ghost_end = if moved { None } else { ghost_at(run.len, face) };
    // Right to left: the close, the ⬆️ and its blank, the label and its blank, the switch's
    // slot, `──`, the tail slot.
    let up = switch
        .map(|paint| paint.up)
        .filter(|up| up.shows() && rung.switch);
    let close_at = field - rung.close;
    let arrow_at = up.map(|_| close_at - 2..close_at);
    let x = arrow_at.as_ref().map_or(close_at, |a| a.start - 1);
    // The words the label's slot shows in its place take the label's own cells, in a form that
    // fits them, so a refusal or the label's hover moves nothing; only where no form fits, or
    // where a form that says when a move lands does not fit them but fits the room
    // ([`SlotWords::cells`]), does the slot grow left into the rule's room, the switch and
    // Claude's zone moving with it, and only as far as that form needs. Words that must be whole
    // and do not fit leave the label standing.
    let shown = laid.label.as_ref().zip(switch).and_then(|(label, paint)| {
        let room = label.len + field.saturating_sub(rung.need);
        slot_words(&paint, rung.label?)?.cells(label.len, room)
    });
    let label_at = shown.as_ref().or(laid.label.as_ref()).map(|l| x - l.len..x);
    let y = label_at.as_ref().map_or(x, |l| l.start - 1);
    let switch_slot = rung.switch.then(|| y - SWITCH_SLOT..y);
    let face_at = laid.face.as_ref().map(|f| y - f.len..y);
    let slot_len = if moved { span.slot() } else { 0 };
    let slot_at = switch_slot
        .as_ref()
        .filter(|_| slot_len > 0)
        .map(|c| c.start - SLOT_RULE - slot_len..c.start - SLOT_RULE);
    let ghost_slot = slot_at
        .as_ref()
        .filter(|_| span.ghost)
        .map(|s| s.start..s.start + GHOST_CELLS);
    let tail_slot = slot_at
        .as_ref()
        .filter(|_| span.tail > 0)
        .map(|s| s.end - span.tail..s.end);
    let next = slot_at
        .as_ref()
        .map(|s| s.start)
        .or(switch_slot.as_ref().map(|c| c.start))
        .unwrap_or(field);
    // Where the rule hands over to the harness: a face narrower than its slot leaves the slot's
    // first cells to the rule.
    let handover = slot_at
        .as_ref()
        .map(|s| s.start)
        .or(face_at.as_ref().map(|f| f.start))
        .unwrap_or(next);
    let mark_at = if moved {
        let keyed = face.ghost.is_some() || (face.tag.is_some() && tail_slot.is_some());
        face.mark
            .and_then(|_| slot_at.as_ref().filter(|_| keyed))
            .and_then(|s| s.start.checked_sub(1))
    } else {
        mark_in_run(ghost_end)
    };
    if let (Some(dst), Some(t), Some((a, b))) = (&tail_slot, run.tail, face.tag)
        && a >= t.col
    {
        let at = run.col + dst.start;
        top.tag = Some((at + a - t.col, at + b - t.col));
    }
    let lockup_at = rung.lockup.zip(mark).map(|(lockup, mark)| {
        let cells = match lockup {
            Lockup::Full => mark.lockup_cells(build),
            Lockup::Icon => aterm_effects::cli_marks::ICON_RUN,
        };
        rung.lead..rung.lead + cells
    });
    // The close's blank, before its `──`.
    let close_pad = (rung.switch && rung.close == CLOSE).then_some(close_at);
    // Drawn blank only beside the ⬆️'s two cells: an asked move's one-cell `◌` and a held
    // move's `↑` keep the blank after them in the arrow's second cell, and the close's runs as
    // rule (`◌ ───`, `↑ ───`), so the mark has one blank either side as the ⬆️ has.
    let close_blank = close_pad.filter(|_| up.is_none_or(|up| up.offers()));
    if let (Some(f), Some(at)) = (&laid.face, &face_at) {
        let col_start = run.col + at.start;
        let ink = inked(f.cells());
        top.switch = Some(LaidSwitch {
            arrow: arrow_at
                .as_ref()
                .map(|a| run.col + a.start - 1..run.col + a.end + usize::from(close_pad.is_some())),
            cols: col_start..col_start + f.len,
            ink: col_start + ink.start..col_start + ink.end,
            label: label_at
                .as_ref()
                .zip(rung.label)
                .map(|(l, tier)| (run.col + l.start..run.col + l.end, tier)),
        });
    }
    let goal_at = goal.and_then(|text| {
        let from = lockup_at
            .as_ref()
            .map_or(rung.lead, |r| r.end)
            .max(voice_label);
        lay_goal(text, from, mark_at.map_or(next, |m| m.min(next)))
    });
    let (lo, hi) = if tint.is_some() {
        (0, field)
    } else {
        let firsts = [
            lockup_at.as_ref().map(|r| r.start),
            mark_at,
            goal_at.as_ref().map(|g| g.cols.start),
            slot_at.as_ref().map(|s| s.start),
            face_at.as_ref().map(|f| f.start),
        ];
        let lasts = [
            lockup_at.as_ref().map(|r| r.end),
            mark_at.map(|m| m + 1),
            goal_at.as_ref().map(|g| g.cols.end),
            face_at.as_ref().map(|f| f.end),
            label_at.as_ref().map(|l| l.end),
            arrow_at.as_ref().map(|a| a.end),
            close_pad.map(|p| p + 1),
        ];
        let lo = firsts.into_iter().flatten().min().unwrap_or(0);
        // Where Claude's zone or the ghost moved, the whole rule is redrawn: the vendor's ` ─`
        // under the zone it left is the rule's again.
        let hi = if slot_len > 0 || ghost_end.is_some() {
            field
        } else {
            lasts.into_iter().flatten().max().unwrap_or(lo)
        };
        (lo, hi.max(voice_label))
    };
    top.col = run.col + lo;
    let cells = &mut top.cells;
    cells.resize(hi - lo, rule_cell);
    let pad = RenderCell { ch: ' ', ..blank };
    if tint.is_some() {
        let parts = [
            lockup_at.clone(),
            ghost_slot.clone(),
            tail_slot.clone(),
            face_at.clone(),
            label_at.as_ref().map(|l| l.start - 1..l.end),
            arrow_at.as_ref().map(|a| a.start - 1..a.end),
            close_blank.map(|p| p..p + 1),
        ];
        // Every `─` wears the arc of its own column ([`GlassInks::rule`]).
        for i in 0..field {
            if parts.iter().flatten().any(|r| r.contains(&i)) {
                continue;
            }
            cells[i - lo] = RenderCell {
                ch: '\u{2500}',
                fg: rgb_of(glass.rule(across.t(run.col + i))),
                ..rule_cell
            };
        }
    }
    if let (Some(at), Some(mark)) = (&lockup_at, mark) {
        for c in at.clone() {
            cells[c - lo] = pad;
        }
        top.lockup = run.col + at.start..run.col + at.end;
        top.icon = Some(run.col + at.start + 1);
        if rung.lockup == Some(Lockup::Full) {
            let ground = crate::claude_card::pack(blank.bg);
            let name_at = at.start + aterm_effects::cli_marks::ICON_RUN;
            // The name is the CLI's own word at rest and under the pointer alike: a hover never
            // swaps it for the product's name.
            for (k, ch) in mark.name(build).chars().enumerate() {
                cells[name_at + k - lo] = RenderCell {
                    ch,
                    fg: rgb_of(aterm_effects::cli_marks::name_fg(mark, k, ground)),
                    bold: true,
                    ..blank
                };
            }
        }
    }
    if let Some(paint) = switch {
        let inks = FaceInks::of(&glass);
        if let (Some(f), Some(at)) = (&laid.face, &face_at) {
            let ink = inked(f.cells());
            for (k, &ch) in f.cells().iter().enumerate() {
                cells[at.start + k - lo] = switch_cell(ch, ink.contains(&k), &paint, &inks, blank);
            }
        }
        if let (Some(label), Some(at)) = (&laid.label, &label_at) {
            cells[at.start - 1 - lo] = pad;
            let laid = shown.as_ref().unwrap_or(label).cells();
            // The brand's letters run the hero's ramp across the brand alone.
            let brand = laid
                .iter()
                .filter(|(_, ink)| *ink == LabelInk::Brand)
                .count();
            let mut nth = 0usize;
            let lit = if paint.hover == HoverAt::Label && paint.refused.is_empty() {
                let first = laid.iter().position(|(_, ink)| *ink != LabelInk::Pad);
                let last = laid.iter().rposition(|(_, ink)| *ink != LabelInk::Pad);
                first.zip(last).map_or(0..0, |(a, b)| a..b + 1)
            } else {
                0..0
            };
            for (k, &(ch, ink)) in laid.iter().enumerate() {
                let along = if brand > 1 {
                    nth as f32 / (brand - 1) as f32
                } else {
                    0.0
                };
                nth += usize::from(ink == LabelInk::Brand);
                let cell = label_cell(ch, ink, along, &inks, blank);
                cells[at.start + k - lo] = if lit.contains(&k) {
                    // The mark is a click target, so under the pointer its inked cells wear
                    // the glass's one hover (§1.5); the brand's letters keep the hero's ramp
                    // over the underline, as a warning keeps its ink.
                    let over =
                        crate::claude_lights::hovered_cell(&inks.glass, cell, HoverPart::Text);
                    if ink == LabelInk::Brand {
                        RenderCell {
                            fg: cell.fg,
                            ..over
                        }
                    } else {
                        over
                    }
                } else {
                    cell
                };
            }
        }
        if let (Some(at), Some(up)) = (&arrow_at, up) {
            cells[at.start - 1 - lo] = pad;
            let (lead, second) = match up.state {
                UpState::Asked => (
                    RenderCell {
                        ch: crate::autopilot::IN_FLIGHT,
                        fg: mark_ink(at.start),
                        bold: false,
                        ..pad
                    },
                    pad,
                ),
                // A held move's arrow, which a press cannot move: one quiet cell in the label's
                // ink, never the colour ⬆️ a press acts on, and no ring (`ring_layer`).
                UpState::Held => (
                    RenderCell {
                        ch: HELD_ARROW,
                        fg: inks.muted,
                        bold: false,
                        ..pad
                    },
                    pad,
                ),
                // A little arrow: the emoji drawn small on the rule's stroke, inside its ring
                // (`Script::small_emoji_rows`), not the cell's whole height.
                UpState::Ready | UpState::None => (
                    RenderCell {
                        ch: UP_ARROW,
                        fg: inks.lit,
                        bold: false,
                        emoji_presentation: true,
                        script: aterm_core::terminal::Script::Sub { nudge: 0 },
                        ..pad
                    },
                    RenderCell { wide: true, ..pad },
                ),
            };
            cells[at.start - lo] = lead;
            cells[at.start + 1 - lo] = second;
        }
        if let Some(p) = close_blank {
            cells[p - lo] = pad;
        }
    }
    if let (Some(at), Some(lit)) = (&ghost_slot, face.ghost) {
        for (k, cell) in ghost_cells(lit).enumerate() {
            cells[at.start + k - lo] = cell;
        }
        top.ghost = Some((run.col + at.start + 1, run.col + at.start + GHOST_CELLS - 1));
    }
    if let Some(at) = &tail_slot {
        for (k, cell) in tail_cells.iter().enumerate() {
            cells[at.start + k - lo] = relaid(cell);
        }
        // Claude's tag under the pointer or the keyboard's selection: the glass's one hover
        // (§1.5) over its cells, which are the key's hit.
        if let (true, Some((a, b))) = (face.tag_lit, top.tag) {
            for col in a..b {
                if let Some(cell) = (col - run.col)
                    .checked_sub(lo)
                    .and_then(|c| cells.get_mut(c))
                {
                    *cell = crate::claude_lights::hovered_cell(&glass, *cell, HoverPart::Text);
                }
            }
        }
    }
    if let Some(at) = mark_at
        && let Some(mark) = mark_cell(at)
    {
        cells[at - lo] = mark;
    }
    if let Some(laid) = &goal_at {
        let pad = RenderCell {
            ch: ' ',
            ..rule_cell
        };
        let lit = rgb_of(glass.lit());
        for (k, glyph) in laid.glyphs.iter().enumerate() {
            cells[laid.cols.start + k - lo] = match *glyph {
                GoalGlyph::Pad => pad,
                GoalGlyph::Mark => RenderCell {
                    ch: GOAL_MARK,
                    fg: mark_ink(laid.cols.start + k),
                    bold: false,
                    ..rule_cell
                },
                GoalGlyph::Text(ch) => RenderCell {
                    ch,
                    fg: lit,
                    bold: false,
                    ..rule_cell
                },
                GoalGlyph::Wide => RenderCell {
                    ch: ' ',
                    wide: true,
                    fg: lit,
                    bold: false,
                    ..rule_cell
                },
            };
        }
        top.goal_cols = Some(run.col + laid.cols.start..run.col + laid.cols.end);
    }
    if let (Some(at), Some(lit)) = (ghost_end, face.ghost) {
        top.ghost = Some((run.col + at + 1, run.col + at + GHOST_CELLS - 1));
        cells.extend(ghost_cells(lit));
    }
    if tint.is_none() && !top.lockup.is_empty() {
        let after = (top.lockup.end - run.col).max(voice_label);
        // The space after the goal stays the program's, up to the harness.
        let from = after.max(goal_at.as_ref().map_or(0, |g| g.cols.end));
        let ahead = [
            switch_slot.as_ref().map(|_| handover),
            mark_at,
            ghost_end.map(|_| field),
        ]
        .into_iter()
        .flatten()
        .min();
        if let Some(ahead) = ahead.filter(|&ahead| from < ahead && ahead <= hi) {
            top.glass = from - lo..ahead - lo;
        }
    }
}

/// The mark before the active goal on a top line ([`lay_goal`]): Claude Code's own `/goal` glyph.
pub(crate) const GOAL_MARK: char = '\u{25ce}';

/// The rule's own `─` kept on either side of a laid goal ([`lay_goal`]).
const GOAL_RULE: usize = 2;

/// The fewest of a goal's own cells worth laying ([`lay_goal`]): fewer say nothing readable.
const GOAL_MIN_TEXT: usize = 8;

/// One cell of a laid goal ([`lay_goal`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GoalGlyph {
    /// A space on the line's ground.
    Pad,
    /// [`GOAL_MARK`], in the mark's ink.
    Mark,
    /// A glyph of the goal's words (`…` where they are cut).
    Text(char),
    /// The right half of the wide glyph before it.
    Wide,
}

/// An active goal laid on a top line ([`lay_goal`]).
#[derive(Debug, Clone, PartialEq, Eq)]
struct LaidGoal {
    /// The run's cells it takes.
    cols: std::ops::Range<usize>,
    /// One per cell: ` ◎ <goal> `.
    glyphs: Vec<GoalGlyph>,
}

/// `goal` laid as ` ◎ <goal> ` in the run's free cells `from..to`, [`GOAL_RULE`] of the rule's
/// `─` left on either side, its words cut with `…` to fit; `None` where fewer than
/// [`GOAL_MIN_TEXT`] of its cells would show. A control or zero-width character takes no cell, a
/// wide one two.
fn lay_goal(goal: &str, from: usize, to: usize) -> Option<LaidGoal> {
    let start = from + GOAL_RULE;
    let room = to.checked_sub(start + GOAL_RULE)?;
    let words = room.checked_sub(4)?;
    let mut buf = [0u8; 4];
    let widths: Vec<(char, usize)> = goal
        .chars()
        .map(|ch| (ch, footer::text_width(ch.encode_utf8(&mut buf))))
        .filter(|&(_, w)| w > 0)
        .collect();
    let whole: usize = widths.iter().map(|&(_, w)| w).sum();
    if whole == 0 || words < GOAL_MIN_TEXT.min(whole) {
        return None;
    }
    let (budget, cut) = if whole <= words {
        (words, false)
    } else {
        (words - 1, true)
    };
    let mut glyphs = vec![GoalGlyph::Pad, GoalGlyph::Mark, GoalGlyph::Pad];
    let mut used = 0usize;
    for &(ch, w) in &widths {
        if used + w > budget {
            break;
        }
        glyphs.push(GoalGlyph::Text(ch));
        if w == 2 {
            glyphs.push(GoalGlyph::Wide);
        }
        used += w;
    }
    if cut {
        glyphs.push(GoalGlyph::Text('\u{2026}'));
    }
    glyphs.push(GoalGlyph::Pad);
    Some(LaidGoal {
        cols: start..start + glyphs.len(),
        glyphs,
    })
}

/// The cache and repaint term of an active goal (0: none).
pub(crate) fn goal_term(goal: Option<&str>) -> u64 {
    goal.map_or(0, |text| aterm_hash::fnv1a_64(text.as_bytes()) | 1)
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
    layer_id(session ^ 0x746F_702D_6D61_726B)
}

/// Computes the stable fx layer ID for `session`'s top composer ⬆️ ring.
pub(crate) const fn top_ring_id(session: u64) -> u64 {
    layer_id(session ^ 0x746F_702D_7377_6974)
}

/// A seed mixed into a layer ID (SplitMix64's finalizer).
const fn layer_id(seed: u64) -> u64 {
    let mut z = seed;
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// The contents of the ⬆️ rings' sprites ([`aterm_effects::up_ring::content`]): the last
/// [`Self::KEPT`] keys, so a hover back finds its bytes.
#[derive(Debug, Default)]
struct RingContents(
    Vec<(
        aterm_effects::up_ring::RingKey,
        Arc<aterm_core::render::FxContent>,
    )>,
);

impl RingContents {
    /// How many contents are kept.
    const KEPT: usize = 4;

    /// The content of `key`'s ring.
    fn content(
        &mut self,
        key: &aterm_effects::up_ring::RingKey,
    ) -> Arc<aterm_core::render::FxContent> {
        if let Some((_, kept)) = self.0.iter().find(|(k, _)| k == key) {
            return Arc::clone(kept);
        }
        let content = aterm_effects::up_ring::content(key);
        if self.0.len() >= Self::KEPT {
            self.0.remove(0);
        }
        self.0.push((*key, Arc::clone(&content)));
        content
    }
}

/// The ring's sprite for a top rule composed from `key` into `top` while the pointer is on its
/// ⬆️: under the arrow, from half a cell left of its first cell, in the hue of the rule it closes
/// (the rule's arc at the arrow's column, [`GlassInks::rule`]; none with no theme or the harness
/// off); `None` with the pointer elsewhere, or on an asked move's `◌`, which takes no press. The
/// switch itself has no sprite.
fn ring_layer(
    key: &TopKey,
    top: &ComposedTop,
    session: u64,
    rings: &mut RingContents,
) -> Option<aterm_core::render::FxLayer> {
    use crate::autopilot::HarnessSwitch;
    use aterm_effects::up_ring::{RingHue, RingKey};
    // Only a ⬆️ a press acts on is ringed: an asked move's `◌` takes no press, and a held move's
    // `↑` asks for nothing (`App::press_upgrade_arrow`).
    let paint = key
        .switch
        .filter(|p| p.hover == HoverAt::Arrow && p.up.offers())?;
    // The arrow's hit begins with its blank: its first cell is the next.
    let arrow = top.switch.as_ref()?.arrow.as_ref()?.start + 1;
    let pack = crate::claude_card::pack;
    let hue = match (paint.face, key.palette) {
        (HarnessSwitch::Off(_) | HarnessSwitch::TurningOff, _) | (_, None) => RingHue::Neutral,
        (_, Some(palette)) => RingHue::Rule(
            GlassInks::of(
                Some(palette),
                pack(key.blank.bg),
                pack(key.blank.fg),
                pack(key.run.rule.fg),
            )
            .rule(key.field.t(arrow)),
        ),
    };
    let ring = RingKey {
        cell: key.cell,
        ground: pack(key.blank.bg),
        fg: pack(key.blank.fg),
        hue,
    };
    let (cw, ch) = key.cell;
    let px = |cells: usize, side: u16| {
        i32::try_from(cells)
            .ok()
            .and_then(|c| c.checked_mul(i32::from(side)))
    };
    let x = px(arrow, cw)?.checked_sub(i32::from(cw) / 2)?;
    Some(aterm_effects::up_ring::layer(
        top_ring_id(session),
        rings.content(&ring),
        (x, px(key.frame_row, ch)?),
        key.clip,
    ))
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

/// Returns the accessibility label of border switch `b`: the label's widest words as the glass
/// draws them (`ALab v0.5 (v2.1.305)`, at every width, so every tier's visible words are in it)
/// where one is laid, then the switch's state.
#[cfg(a11y_tree)]
pub(crate) fn switch_words(b: &crate::claude_lights::BorderSwitch) -> String {
    let state = switch_face(b.paint).words();
    let lead = b
        .mark
        .as_ref()
        .and_then(|_| (0..LABEL_TIERS).find_map(|tier| label_face(&b.paint, tier)))
        .map(|face| face.text());
    match lead {
        Some(lead) if !state.is_empty() => format!("{lead} {state}"),
        Some(lead) => lead,
        None => state,
    }
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

/// Whether Claude's mode row may be rewritten on this pane's glass: Claude, bordered, the harness
/// on; [`collect_mode_row`] then takes a bypass row alone.
fn rewrites_mode_row(program: aterm_phase::Program, bordered: bool) -> bool {
    program == aterm_phase::Program::Claude && bordered
}

/// The repaint key's term for a pane's harness theme.
fn theme_term(theme: Option<aterm_effects::harness_card::CardPalette>) -> u64 {
    theme.map_or(0, |p| 1 + p as u64)
}

/// One rewritten row ([`collect_mode_row`]).
struct RowWrite {
    frame_row: usize,
    col: usize,
    cells: std::ops::Range<usize>,
    /// The cells, from `col`, the rewrite lays words in (the kept items and their separators);
    /// the rest of `cells` is blank glass over the program's hidden letters
    /// (`crate::pet_chrome::PetChrome::note_blanked`).
    words: usize,
    ground: RenderCell,
    shift: GlassShift,
}

/// Column map for a single window row the glass rewrote: Claude's mode row
/// ([`footer::ModeRow::grid_col`]), or the row its effort hint wrapped to, blanked.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct GlassShift {
    term_row: usize,
    /// The pane's column 0.
    col_off: usize,
    /// The mode row's map; `None` for a row blanked whole up to `blank_to`.
    mode: Option<footer::ModeRow>,
    blank_to: usize,
}

impl GlassShift {
    /// `None` outside this rewrite's span; `Some(None)` on a cell it vacated.
    fn grid_col(&self, row: usize, col: usize) -> Option<Option<usize>> {
        if row != self.term_row || col < self.col_off {
            return None;
        }
        let at = col - self.col_off;
        match &self.mode {
            Some(mode) => (at < mode.end).then(|| mode.grid_col(at).map(|c| self.col_off + c)),
            None => (at < self.blank_to).then_some(None),
        }
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

    fn mapped(&self, row: u16, col: u16) -> Option<Option<usize>> {
        self.rows[..self.len]
            .iter()
            .find_map(|s| s.grid_col(usize::from(row), usize::from(col)))
    }

    /// Maps glass column `col` on `row` back to its terminal grid column across rewritten rows;
    /// a vacated cell is its own column ([`Self::swallows`] keeps a press off it).
    pub(crate) fn grid_col(&self, row: u16, col: u16) -> u16 {
        self.mapped(row, col)
            .flatten()
            .and_then(|c| u16::try_from(c).ok())
            .unwrap_or(col)
    }

    /// Whether `(row, col)` is a cell a rewrite vacated: a press there lands on nothing.
    pub(crate) fn swallows(&self, row: u16, col: u16) -> bool {
        self.mapped(row, col) == Some(None)
    }
}

/// Whether frame row `frame_row`'s cells `written` are plain enough for the glass to rewrite:
/// no cluster, combining mark or image on them, out of the chrome, no selection, not the
/// cursor's row, not a wrapped one.
fn rewritable(
    scratch: &RenderInput,
    frame_row: usize,
    written: &std::ops::Range<usize>,
    chrome_owned: bool,
) -> bool {
    !(chrome_owned
        || scratch.any_selection()
        || (scratch.cursor_visible && scratch.cursor_row == frame_row)
        || !scratch.visual_to_logical_row(frame_row).is_empty()
        || touches(&scratch.clusters, frame_row, written)
        || touches(&scratch.combining, frame_row, written)
        || touches(&scratch.images, frame_row, written))
}

/// Collect the rewrite of Claude's mode row `mode` under pane rows `rows`' bottom rule
/// ([`footer::ModeRow`]): its kept items cell-copied in Claude's own styles, their inks floored to
/// read as words do on a page Claude's own do not ([`relaid`]), flush at the pane's column 0 and
/// joined by ` · ` in the ink of a `·` of Claude's, blanks after; and the row its effort hint
/// wrapped to, blanked. Only a bypass row is rewritten, its pill the chip's `yolo`
/// (`docs/DESIGN-yolo-binary-2026-11-04.md` §2): any other mode's row stands as Claude draws it,
/// its own pill naming plan or auto, so aterm names no mode but the bit. `false`, the row standing
/// as drawn, for a row of any other pill, where it is not plainly Claude's ([`rewritable`]), where
/// a cell differs from the text read, is wide or presented, or a dropped item has a ground of its
/// own (Claude's highlight), or where the window keeps [`GLASS_SHIFTS`] rows already.
#[allow(clippy::too_many_arguments)]
fn collect_mode_row(
    scratch: &RenderInput,
    rows: &[String],
    region: PaneTextRegion,
    mode: footer::ModeRow,
    chrome_owned: impl Fn(usize) -> bool,
    blank: RenderCell,
    out: &mut Vec<RowWrite>,
    cells: &mut Vec<RenderCell>,
) -> bool {
    let term_row = region.row_off + mode.row;
    let frame_row = region.strip + term_row;
    let from = region.col_off;
    let written = from..from + mode.end;
    if aterm_agent::harness::lights::Mode::of_indicator(mode.mode)
        != Some(aterm_agent::harness::lights::Mode::Bypass)
        || out.len() >= GLASS_SHIFTS
        || mode.end > region.cols
        || !rewritable(scratch, frame_row, &written, chrome_owned(term_row))
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
    let kept = |k: usize| mode.kept().iter().any(|i| (i.start..i.end).contains(&k));
    let plain = |span: &[RenderCell], text: &str, kept: &dyn Fn(usize) -> bool| {
        span.len() == text.chars().count()
            && span
                .iter()
                .zip(text.chars())
                .enumerate()
                .all(|(k, (c, ch))| {
                    c.ch == ch
                        && !c.wide
                        && !c.emoji_presentation
                        && !c.text_presentation
                        && (kept(k) || c.bg == blank.bg)
                })
    };
    let text = &rows[mode.row];
    let drawn = &text[..text
        .char_indices()
        .nth(mode.end)
        .map_or(text.len(), |(b, _)| b)];
    if !plain(span, drawn, &kept) {
        return false;
    }
    let sep = mode.sep.and_then(|c| span.get(c)).copied().unwrap_or(blank);
    let at = cells.len();
    for (i, k) in mode.kept().iter().enumerate() {
        if i > 0 {
            cells.extend(
                footer::MODE_ROW_SEP
                    .chars()
                    .map(|ch| RenderCell { ch, ..sep }),
            );
        }
        cells.extend(span[k.start..k.end].iter().map(relaid));
    }
    cells.extend(std::iter::repeat_n(blank, mode.end - mode.glass_end()));
    out.push(RowWrite {
        frame_row,
        col: from,
        cells: at..cells.len(),
        words: mode.glass_end(),
        ground: blank,
        shift: GlassShift {
            term_row,
            col_off: region.col_off,
            mode: Some(mode),
            blank_to: 0,
        },
    });
    if let Some((row, end)) = mode.effort {
        let term_row = region.row_off + row;
        let frame_row = region.strip + term_row;
        let written = from..from + end;
        let span = scratch
            .cells
            .get(frame_row)
            .and_then(|cells| cells.get(written.clone()));
        let text = &rows[row];
        if out.len() < GLASS_SHIFTS
            && end <= region.cols
            && rewritable(scratch, frame_row, &written, chrome_owned(term_row))
            && span.is_some_and(|span| plain(span, text.trim_end(), &|_| false))
        {
            let at = cells.len();
            cells.extend(std::iter::repeat_n(blank, end));
            out.push(RowWrite {
                frame_row,
                col: from,
                cells: at..cells.len(),
                words: 0,
                ground: blank,
                shift: GlassShift {
                    term_row,
                    col_off: region.col_off,
                    mode: None,
                    blank_to: end,
                },
            });
        }
    }
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
                                "yolo \u{00B7} "
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

/// Reconstructs the display string of `row` on the glass: each cell's character in order, a wide
/// glyph's right half its blank. Line D's facts are terminal text at pitch 1, so their cells read
/// as they are.
#[cfg(test)]
pub(crate) fn read_glass(row: &[RenderCell]) -> String {
    row.iter().map(|c| c.ch).collect()
}

#[cfg(any(debug_assertions, feature = "dev-seams"))]
#[path = "claude_footer_layout_scene.rs"]
pub(crate) mod layout_scene;

/// Line E, the quota row (`docs/DESIGN-harness-glass-rainbow-2026-11-05.md` §5.3).
#[path = "claude_footer_quota.rs"]
pub(crate) mod quota;

/// Benchmark and cost-test fixture for a single-pane footer pass.
#[cfg(any(test, feature = "bench-support"))]
#[path = "claude_footer_probe.rs"]
pub(crate) mod probe;

#[cfg(test)]
#[path = "claude_account_progress_gallery.rs"]
mod account_progress_gallery;

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
#[path = "claude_footer_goal_tests.rs"]
mod goal_tests;

#[cfg(test)]
#[path = "claude_footer_floor_tests.rs"]
mod floor_tests;

#[cfg(test)]
#[path = "claude_footer_outlook_tests.rs"]
mod outlook_tests;

#[cfg(all(test, unix))]
#[path = "claude_footer_yolo_tests.rs"]
mod yolo_tests;

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
#[path = "claude_footer_registry_tests.rs"]
mod registry_tests;

/// Test harness for resolving footer facts from a scratch Claude home directory.
#[cfg(test)]
#[path = "claude_footer_rig_tests.rs"]
pub(crate) mod rig;

#[cfg(test)]
#[path = "claude_footer_tests.rs"]
mod tests;
