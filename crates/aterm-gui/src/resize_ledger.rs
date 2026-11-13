// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Resize ledger tracking session grid resizes, their caller sites, and desync-risk state.

use std::cell::Cell;
use std::collections::VecDeque;
use std::fmt::Write as _;
use std::panic::Location;

use aterm_core::terminal::{ResizeReport, Terminal};

use crate::resize_epoch::{EpochBook, EpochVerdict};

/// How many resize rows the ledger keeps.
pub(crate) const LEDGER_ROWS_CAP: usize = 128;
/// How many runs the ledger keeps (drop-oldest).
pub(crate) const LEDGER_RUNS_CAP: usize = 64;
/// The widest gap between two resizes of one run.
pub(crate) const RUN_GAP_MS: u64 = 250;

fn run_gap_ms() -> u64 {
    static GAP: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *GAP.get_or_init(|| {
        aterm_types::dev_seam!("ATERM_RESIZE_RUN_GAP_MS")
            .and_then(|v| {
                let s = v.to_str()?;
                (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
                    .then(|| s.parse().ok())
                    .flatten()
            })
            .unwrap_or(RUN_GAP_MS)
    })
}
/// How long after a run's last resize a draw is judged.
pub(crate) const RISK_AFTER_MS: u64 = 1_000;
const GEOMS_KEPT: usize = 8;

/// Which kind of entry point asked for a window pass (`site=`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Cause {
    /// The input dispatch (`App::input_to_session`): an `InputEvent::Resize`.
    Input,
    /// The OS window changed size (`App::on_resize`).
    Window,
    /// The window's chrome rows changed: the tab strip appeared or folded.
    Chrome,
    /// One window's sessions re-gridded directly.
    Term,
    /// A pane pass under no other entry point.
    Pass,
    /// A tab a drag deferred, sized as it is drawn (`redraw_window`).
    Redraw,
    /// The Deck's head moved: a config reload changed its size setting, or the stats bars into or
    /// out of it (`App::apply_prepared_config_generation_unfenced`), or a restored frame the Deck
    /// sized in the form it no longer takes re-gridded its tabs at attach
    /// (`App::regrid_to_attach_frame`).
    Deck,
    /// A session restore placed every tab, and each pane took its window's grid
    /// (`App::regrid_restored_windows`): a handed-off shell keeps the grid its engine was parked at.
    Restore,
}

impl Cause {
    const fn word(self) -> &'static str {
        match self {
            Self::Input => "input",
            Self::Window => "window",
            Self::Chrome => "chrome",
            Self::Term => "term",
            Self::Pass => "pass",
            Self::Redraw => "redraw",
            Self::Deck => "deck",
            Self::Restore => "restore",
        }
    }
}

/// Who asked for a resize.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Site {
    /// A window pass: its entry point's kind and the source line that called that entry point.
    Caller {
        cause: Cause,
        at: &'static Location<'static>,
    },
    /// A cross-session `resize` from the control socket (`site=ctl-cross`).
    CtlCross,
    /// Backfilled from the engine journal with no host record (`site=-`).
    Unledgered,
}

impl Site {
    /// A window pass entered at `at` for `cause`.
    pub(crate) const fn caller(cause: Cause, at: &'static Location<'static>) -> Self {
        Self::Caller { cause, at }
    }
}

thread_local! {
    static SCOPE: Cell<Option<Site>> = const { Cell::new(None) };
}

/// Marks a resize entry point for the ledger's `site=`.
#[must_use = "the scope lasts as long as the guard"]
pub(crate) struct SiteScope {
    outer: bool,
}

impl SiteScope {
    /// Enter an entry point for `cause`, attributed to the caller's line.
    #[track_caller]
    pub(crate) fn enter(cause: Cause) -> Self {
        let at = Location::caller();
        SCOPE.with(|scope| {
            if scope.get().is_some() {
                Self { outer: false }
            } else {
                scope.set(Some(Site::caller(cause, at)));
                Self { outer: true }
            }
        })
    }

    /// The entry point this thread is inside, if any.
    pub(crate) fn current() -> Option<Site> {
        SCOPE.with(Cell::get)
    }
}

impl Drop for SiteScope {
    fn drop(&mut self) {
        if self.outer {
            SCOPE.with(|scope| scope.set(None));
        }
    }
}

/// Where a resize came from: its site, and the window's grid and chrome rows when a window drove it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Attribution {
    pub(crate) site: Site,
    /// The window's grid `(cols, rows)` when the resize ran.
    pub(crate) win: Option<(u16, u16)>,
    /// The window's chrome rows (the tab strip's) then.
    pub(crate) chrome: Option<u16>,
}

impl Attribution {
    /// A resize found only in the engine journal.
    pub(crate) const UNLEDGERED: Self = Self {
        site: Site::Unledgered,
        win: None,
        chrome: None,
    };

    /// A cross-session `resize` verb: no window drove it.
    pub(crate) const CTL_CROSS: Self = Self {
        site: Site::CtlCross,
        win: None,
        chrome: None,
    };
}

/// The engine facts a verdict needs, read under a `term_lock` hold the caller already takes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct EngineProbe {
    /// The engine's lifetime token (`Terminal::resize_lifetime`).
    pub(crate) lifetime: u64,
    /// The engine's newest resize ordinal (`Terminal::resize_ordinal`).
    pub(crate) ordinal: u64,
    /// The active grid's content sequence.
    pub(crate) content_seq: u64,
    /// `Terminal::full_clear_count`.
    pub(crate) full_clears: u64,
    /// `Terminal::screen_replaced_count`.
    pub(crate) screen_replaced: u64,
    /// `Terminal::render_debt`: what the alternate screen owes its program.
    pub(crate) debt: aterm_core::terminal::RenderDebt,
    /// The grid's `(rows, cols)` now.
    pub(crate) geom: (u16, u16),
    /// The program is on the main screen: it left the alternate screen it owed debt on.
    pub(crate) main_screen: bool,
}

impl EngineProbe {
    /// Sample `term` (the caller holds its lock).
    pub(crate) const fn sample(term: &Terminal) -> Self {
        Self {
            lifetime: term.resize_lifetime(),
            ordinal: term.resize_ordinal(),
            content_seq: term.content_seq(),
            full_clears: term.full_clear_count(),
            screen_replaced: term.screen_replaced_count(),
            debt: term.render_debt(),
            geom: (term.rows(), term.cols()),
            main_screen: !term.modes().alternate_screen,
        }
    }
}

/// The engine's journal as a resize path copied it, inside the same `term_lock` hold as the resize.
#[derive(Clone, Debug, Default)]
pub(crate) struct JournalCopy {
    reports: Vec<ResizeReport>,
    probe: EngineProbe,
}

impl JournalCopy {
    /// Copy every report the journal still holds (the caller holds `term`'s lock).
    pub(crate) fn take(term: &Terminal) -> Self {
        let (reports, _) = term.resize_journal_since(0);
        Self {
            reports,
            probe: EngineProbe::sample(term),
        }
    }
}

/// A run's verdict (`verdict=`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// Nothing moved (a trim, a same-size resize) or the primary screen.
    None,
    /// A primary-screen width change: the terminal rewrapped the rows itself.
    Reflow,
    /// Content displaced; the app has not drawn over it yet (or only just).
    Silent,
    /// A net-zero run the app drew over without clearing.
    DesyncRisk,
    /// A full clear or screen switch repainted the displaced screen.
    Healed,
    /// A run whose geometry stayed changed, drawn over since with no heal.
    Unverified,
}

impl Verdict {
    /// The wire token.
    pub(crate) const fn wire(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Reflow => "reflow",
            Self::Silent => "silent",
            Self::DesyncRisk => "desync-risk",
            Self::Healed => "healed",
            Self::Unverified => "unverified",
        }
    }

    const fn open(self) -> bool {
        matches!(self, Self::Silent | Self::DesyncRisk)
    }

    const fn watched(self) -> bool {
        matches!(self, Self::Silent | Self::DesyncRisk | Self::Unverified)
    }

    const fn joinable(self) -> bool {
        matches!(self, Self::None | Self::Reflow | Self::Silent)
    }
}

/// A session's render state (`status render=`, the `resizes` header).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Render {
    /// No displaced content on screen.
    #[default]
    Ok,
    /// Some content is displaced and the app has not drawn over it (yet).
    Displaced,
    /// The app drew over displaced content without clearing it.
    DesyncRisk,
    /// The app drew over content a net-changed run displaced, and nothing cleared it.
    Unverified,
    /// The engine owes rows, the geometry is back and the risk wait is over, but the program has not shown it clears its screen when resized.
    Unjudged,
}

impl Render {
    /// The wire token.
    pub(crate) const fn wire(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Displaced => "displaced",
            Self::DesyncRisk => "desync-risk",
            Self::Unverified => "unverified",
            Self::Unjudged => "unjudged",
        }
    }
}

/// One resize the ledger booked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LedgerRow {
    /// Per-session monotonic id (1-based), the `since=<id>` resume key.
    pub(crate) id: u64,
    /// When it was booked, on the timeline clock.
    pub(crate) t_ms: u64,
    /// `t_ms` is when the resize ran: the path that ran it booked it.
    pub(crate) timed: bool,
    /// What the engine did.
    pub(crate) report: ResizeReport,
    /// Who asked.
    pub(crate) attribution: Attribution,
    /// The run it belongs to.
    pub(crate) run: u64,
}

/// A maximal sequence of adjoining resizes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Run {
    pub(crate) id: u64,
    /// The first resize's time and the last's, on the timeline clock.
    t_ms: u64,
    t_end_ms: u64,
    n: u32,
    /// The alternate screen was active.
    alt: bool,
    /// `(rows, cols)` before the first resize and after the last.
    start: (u16, u16),
    end: (u16, u16),
    /// `start`, then each resize's `to`, at most [`GEOMS_KEPT`].
    geoms: Vec<(u16, u16)>,
    /// Geometries left out of `geoms`.
    elided: u32,
    /// Some resize changed the geometry.
    changed: bool,
    /// The summed row shift (negative = content moved up), rewraps excluded.
    displaced: i32,
    /// Rows pushed off the bottom, net of the ones a grow put back there from
    /// the engine's resize undo (`ResizeReport::net_pushed`).
    pushed: i32,
    /// `(top, bottom)`: the rows this run's shrinks left in the engine's
    /// resize undo (`ResizeReport::stashed`: the demoted and the pushed ones)
    /// that no grow has handed back yet.
    held: (u32, u32),
    /// A width change rewrapped the grid.
    reflowed: bool,
    /// The engine ordinal of the run's last resize: a reading taken before it
    /// says nothing about this run.
    last_ordinal: u64,
    /// The content sequence the screen still shows while the app has drawn
    /// nothing since this run: the last resize's, advanced past every later
    /// resize's own re-layout.
    quiet_seq: u64,
    /// The engine's counters at the last resize.
    full_clears: u64,
    screen_replaced: u64,
    /// The app drew over the run's displaced content (the content moved other
    /// than by a resize while the run was `silent`): after its last resize, or
    /// between two of its resizes, which a later resize in the run keeps.
    drew: bool,
    verdict: Verdict,
    /// When a read first saw the app draw (after the resize the draw followed,
    /// which is the run's last unless the run went on to take another), and
    /// when one first saw the heal (after the run's last resize).
    first_out_ms: Option<u64>,
    healed_ms: Option<u64>,
}

impl Run {
    fn start(id: u64, report: &ResizeReport, now_ms: u64, credited: (u32, u32)) -> Self {
        let mut run = Self {
            id,
            t_ms: now_ms,
            t_end_ms: now_ms,
            n: 1,
            alt: report.alt,
            start: report.from,
            end: report.to,
            geoms: vec![report.from, report.to],
            elided: 0,
            changed: report.from != report.to,
            displaced: if report.reflowed {
                0
            } else {
                own_shift(report, credited)
            },
            pushed: own_pushed(report, credited),
            held: stash_of(report),
            reflowed: report.reflowed,
            last_ordinal: report.ordinal,
            quiet_seq: report.content_seq,
            full_clears: report.full_clears,
            screen_replaced: report.screen_replaced,
            drew: false,
            verdict: Verdict::None,
            first_out_ms: None,
            healed_ms: None,
        };
        run.classify();
        run
    }

    fn extend(&mut self, report: &ResizeReport, now_ms: u64, credited: (u32, u32)) {
        self.n = self.n.saturating_add(1);
        self.t_end_ms = now_ms;
        self.end = report.to;
        if self.geoms.len() < GEOMS_KEPT {
            self.geoms.push(report.to);
        } else if let Some(last) = self.geoms.last_mut() {
            *last = report.to;
            self.elided = self.elided.saturating_add(1);
        }
        self.changed |= report.from != report.to;
        if !report.reflowed {
            self.displaced = self.displaced.saturating_add(own_shift(report, credited));
        }
        self.pushed = self.pushed.saturating_add(own_pushed(report, credited));
        let (top, bottom) = stash_of(report);
        self.held = (
            self.held.0.saturating_add(top),
            self.held.1.saturating_add(bottom),
        );
        self.reflowed |= report.reflowed;
        self.last_ordinal = report.ordinal;
        self.quiet_seq = report.content_seq;
        self.full_clears = report.full_clears;
        self.screen_replaced = report.screen_replaced;
        self.classify();
    }

    fn classify(&mut self) {
        if !self.verdict.joinable() {
            return;
        }
        self.verdict = if self.reflowed {
            Verdict::Reflow
        } else if self.judged() {
            Verdict::Silent
        } else {
            Verdict::None
        };
    }

    const fn judged(&self) -> bool {
        self.alt && !self.reflowed && (self.displaced != 0 || self.pushed > 0)
    }

    fn net_zero(&self) -> bool {
        self.start == self.end
    }

    fn eligible(&self) -> bool {
        self.judged() && self.net_zero()
    }

    fn is_flap(&self) -> bool {
        self.n >= 2 && self.changed && self.net_zero()
    }

    const fn healed_by(&self, full_clears: u64, screen_replaced: u64) -> bool {
        full_clears > self.full_clears || screen_replaced > self.screen_replaced
    }

    fn net(&self) -> &'static str {
        if self.net_zero() { "zero" } else { "changed" }
    }

    fn geom_chain(&self) -> String {
        let mut out = String::new();
        let last = self.geoms.len().saturating_sub(1);
        for (i, geom) in self.geoms.iter().enumerate() {
            if i > 0 {
                out.push('>');
            }
            if i == last && self.elided > 0 {
                out.push_str("...>");
            }
            out.push_str(&cxr(*geom));
        }
        out
    }
}

/// One reading of the engine the open runs are settled against.
#[derive(Clone, Copy, Debug)]
struct Reading {
    /// The resizes the engine had applied when it was taken: a run whose last
    /// resize came later is not settled by it.
    ordinal: u64,
    /// The content sequence then — `None` when it cannot be compared with the
    /// runs' (a reading taken with a resize the ledger has not booked, or
    /// before one it has: a resize's own re-layout moves it too).
    content_seq: Option<u64>,
    full_clears: u64,
    screen_replaced: u64,
}

/// A verdict transition the timeline records as `kind=render`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Transition {
    /// `desync-risk run=<r> displaced=<+-k> net=<zero|changed> at=<ms>`.
    Risk {
        run: u64,
        displaced: i32,
        net_zero: bool,
        at_ms: u64,
    },
    /// `healed run=<r> after_ms=<ms>` (after the run's last resize).
    Healed { run: u64, after_ms: u64 },
    /// `desync-risk epoch=<e> displaced=<+-k> pushed=<k> cut=<k> drew=<0|1> class=<c> at=<ms>`.
    EpochRisk {
        epoch: u64,
        shift: i32,
        pushed: i32,
        cut: u16,
        drew: bool,
        class: crate::resize_epoch::ProgramClass,
        at_ms: u64,
    },
    /// `healed epoch=<e> after_ms=<ms> by=<app|undo>`, after an [`Transition::EpochRisk`].
    EpochHealed {
        epoch: u64,
        after_ms: u64,
        by: crate::resize_epoch::EpochBy,
    },
}

impl Transition {
    /// The verdict it moved to, as the session log's `render state=` word (phase 3B).
    pub(crate) const fn state_word(&self) -> &'static str {
        match self {
            Self::Risk { .. } | Self::EpochRisk { .. } => "desync-risk",
            Self::Healed { .. } | Self::EpochHealed { .. } => "healed",
        }
    }

    /// A test's reading of the transition in its `render` row's words, which are also the pushed EVENT's tail.
    #[cfg(test)]
    pub(crate) fn payload(&self) -> String {
        match *self {
            Self::Risk {
                run,
                displaced,
                net_zero,
                at_ms,
            } => format!(
                "desync-risk run={run} displaced={displaced:+} net={} at={at_ms}",
                if net_zero { "zero" } else { "changed" }
            ),
            Self::Healed { run, after_ms } => format!("healed run={run} after_ms={after_ms}"),
            Self::EpochRisk {
                epoch,
                shift,
                pushed,
                cut,
                drew,
                class,
                at_ms,
            } => format!(
                "desync-risk epoch={epoch} displaced={shift:+} pushed={pushed} cut={cut} drew={} \
                 class={} at={at_ms}",
                u8::from(drew),
                class.wire()
            ),
            Self::EpochHealed {
                epoch,
                after_ms,
                by,
            } => format!("healed epoch={epoch} after_ms={after_ms} by={}", by.wire()),
        }
    }
}

/// The ledger: the rows ring, the runs ring and the lifetime counters.
#[derive(Clone, Debug, Default)]
pub(crate) struct ResizeLedger {
    rows: VecDeque<LedgerRow>,
    runs: VecDeque<Run>,
    /// The newest row's id, 0 before the first: also the lifetime count.
    high_row: u64,
    /// The newest run's id, likewise.
    high_run: u64,
    /// The engine lifetime the ledger has booked from (`None` before the
    /// first copy).
    lifetime: Option<u64>,
    /// The highest engine ordinal booked in that lifetime.
    last_ordinal: u64,
    flaps: u64,
    risks: u64,
    unledgered: u64,
    lost: u64,
    render: Render,
    /// The second judge: the engine's debt, as one epoch (`crate::resize_epoch`).
    epochs: EpochBook,
}

impl ResizeLedger {
    /// Books `copy`'s reports past the ledger's watermark, then evaluates against its probe.
    pub(crate) fn ingest(
        &mut self,
        copy: &JournalCopy,
        attribution: Option<Attribution>,
        now_ms: u64,
    ) -> Vec<Transition> {
        let probe = copy.probe;
        self.enter_lifetime(&probe);
        let newest = probe.ordinal;
        if newest < self.last_ordinal {
            if let Some(attribution) = attribution {
                self.attribute(newest, attribution);
            }
            return Vec::new();
        }
        let mut out = Vec::new();
        for report in &copy.reports {
            let attributed = attribution.filter(|_| report.ordinal == newest);
            if report.ordinal <= self.last_ordinal {
                if let Some(attribution) = attributed {
                    self.attribute(report.ordinal, attribution);
                }
                continue;
            }
            if report.ordinal > self.last_ordinal.saturating_add(1) {
                self.lost = self
                    .lost
                    .saturating_add(report.ordinal - self.last_ordinal - 1);
                for run in &mut self.runs {
                    run.held = (0, 0);
                }
            }
            let attribution = attributed.unwrap_or_else(|| {
                self.unledgered = self.unledgered.saturating_add(1);
                Attribution::UNLEDGERED
            });
            self.push(*report, attribution, now_ms, &mut out);
            self.last_ordinal = report.ordinal;
        }
        out.extend(self.evaluate(probe, now_ms));
        out
    }

    fn enter_lifetime(&mut self, probe: &EngineProbe) {
        if self.lifetime == Some(probe.lifetime) {
            return;
        }
        if self.lifetime.is_some() {
            self.epochs.rebase();
            for run in &mut self.runs {
                run.held = (0, 0);
            }
            for run in self.runs.iter_mut().filter(|run| run.verdict.watched()) {
                run.last_ordinal = 0;
                run.full_clears = probe.full_clears;
                run.screen_replaced = probe.screen_replaced;
                if !run.drew {
                    run.quiet_seq = probe.content_seq;
                }
            }
        }
        self.lifetime = Some(probe.lifetime);
        self.last_ordinal = 0;
    }

    fn attribute(&mut self, ordinal: u64, attribution: Attribution) {
        if let Some(row) = self
            .rows
            .iter_mut()
            .rev()
            .find(|row| row.report.ordinal == ordinal)
            && row.attribution.site == Site::Unledgered
        {
            row.attribution = attribution;
            self.unledgered = self.unledgered.saturating_sub(1);
        }
    }

    fn push(
        &mut self,
        report: ResizeReport,
        attribution: Attribution,
        now_ms: u64,
        out: &mut Vec<Transition>,
    ) {
        self.observe(
            Reading {
                ordinal: report.ordinal.saturating_sub(1),
                content_seq: Some(report.content_seq_before),
                full_clears: report.full_clears,
                screen_replaced: report.screen_replaced,
            },
            now_ms,
            out,
        );
        for run in self.runs.iter_mut().filter(|run| run.verdict.open()) {
            if !run.drew && run.quiet_seq == report.content_seq_before {
                run.quiet_seq = report.content_seq;
            }
        }
        self.high_row = self.high_row.saturating_add(1);
        let joins = match (self.rows.back(), self.runs.back()) {
            (Some(prev), Some(run)) => {
                prev.run == run.id
                    && run.verdict.joinable()
                    && joins_run(prev, &report, now_ms, run)
            }
            _ => false,
        };
        let joining = self.runs.back().filter(|_| joins).map(|run| run.id);
        let credited = self.credit(&report, joining);
        let run_id = match self.runs.back_mut().filter(|_| joins) {
            Some(run) => {
                let was_flap = run.is_flap();
                run.extend(&report, now_ms, credited);
                match (was_flap, run.is_flap()) {
                    (false, true) => self.flaps = self.flaps.saturating_add(1),
                    (true, false) => self.flaps = self.flaps.saturating_sub(1),
                    _ => {}
                }
                run.id
            }
            None => {
                self.high_run = self.high_run.saturating_add(1);
                self.runs
                    .push_back(Run::start(self.high_run, &report, now_ms, credited));
                if self.runs.len() > LEDGER_RUNS_CAP {
                    self.runs.pop_front();
                }
                self.high_run
            }
        };
        self.epochs.note_resize(&report, now_ms);
        self.rows.push_back(LedgerRow {
            id: self.high_row,
            t_ms: now_ms,
            timed: attribution.site != Site::Unledgered,
            report,
            attribution,
            run: run_id,
        });
        if self.rows.len() > LEDGER_ROWS_CAP {
            self.rows.pop_front();
        }
    }

    /// Credits rows restored from the engine's resize undo (`restored_top`,
    /// `restored_bottom`) LIFO to the runs whose shrinks stashed them, returning
    /// `(top, bottom)` credited to runs other than `joining` (which nets its own
    /// shift directly).
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "ResizeRender",
            action = "Grow",
            project = "aterm_gui::resize_ledger::tests::Rig::project"
        )
    )]
    fn credit(&mut self, report: &ResizeReport, joining: Option<u64>) -> (u32, u32) {
        let (mut top, mut bottom) = (
            u32::from(report.restored_top),
            u32::from(report.restored_bottom),
        );
        let mut credited = (0u32, 0u32);
        for run in self.runs.iter_mut().rev() {
            if top == 0 && bottom == 0 {
                break;
            }
            if run.drew || !run.verdict.joinable() {
                continue;
            }
            let (t, b) = (top.min(run.held.0), bottom.min(run.held.1));
            if (t, b) == (0, 0) {
                continue;
            }
            run.held = (run.held.0 - t, run.held.1 - b);
            (top, bottom) = (top - t, bottom - b);
            if Some(run.id) == joining {
                continue;
            }
            run.displaced = run
                .displaced
                .saturating_add(i32::try_from(t).unwrap_or(i32::MAX));
            run.pushed = run
                .pushed
                .saturating_sub(i32::try_from(b).unwrap_or(i32::MAX));
            run.classify();
            credited = (credited.0 + t, credited.1 + b);
        }
        credited
    }

    fn observe(&mut self, reading: Reading, now_ms: u64, out: &mut Vec<Transition>) {
        let mut risks = 0u64;
        for run in self.runs.iter_mut().filter(|run| run.verdict.watched()) {
            if reading.ordinal < run.last_ordinal {
                continue;
            }
            let since = now_ms.saturating_sub(run.t_end_ms);
            if run.healed_by(reading.full_clears, reading.screen_replaced) {
                if run.verdict == Verdict::DesyncRisk {
                    out.push(Transition::Healed {
                        run: run.id,
                        after_ms: since,
                    });
                }
                run.verdict = Verdict::Healed;
                run.healed_ms = Some(since);
                run.held = (0, 0);
                if since <= RISK_AFTER_MS {
                    self.epochs.learn();
                }
                continue;
            }
            if !run.drew && reading.content_seq.is_some_and(|seq| seq > run.quiet_seq) {
                run.drew = true;
                run.held = (0, 0);
                run.first_out_ms = Some(since);
            }
            if run.drew && run.verdict == Verdict::Silent && since >= RISK_AFTER_MS {
                if run.eligible() {
                    run.verdict = Verdict::DesyncRisk;
                    risks += 1;
                    out.push(Transition::Risk {
                        run: run.id,
                        displaced: run.displaced,
                        net_zero: run.net_zero(),
                        at_ms: run.t_ms,
                    });
                } else {
                    run.verdict = Verdict::Unverified;
                }
            }
        }
        self.risks = self.risks.saturating_add(risks);
    }

    /// Evaluate the open runs against `probe` (idempotent but for the time-driven verdicts) and return the transitions for the timeline.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "ResizeRender",
            action = "Evaluate",
            project = "aterm_gui::resize_ledger::tests::Rig::project"
        )
    )]
    pub(crate) fn evaluate(&mut self, probe: EngineProbe, now_ms: u64) -> Vec<Transition> {
        let mut out = Vec::new();
        if self
            .lifetime
            .is_none_or(|lifetime| lifetime == probe.lifetime)
        {
            self.observe(
                Reading {
                    ordinal: probe.ordinal,
                    content_seq: (probe.ordinal == self.last_ordinal).then_some(probe.content_seq),
                    full_clears: probe.full_clears,
                    screen_replaced: probe.screen_replaced,
                },
                now_ms,
                &mut out,
            );
        }
        if self
            .lifetime
            .is_none_or(|lifetime| lifetime == probe.lifetime)
        {
            let run_risk = self.runs.iter().any(|r| r.verdict == Verdict::DesyncRisk);
            let healed_run = out.iter().any(|t| matches!(t, Transition::Healed { .. }));
            self.epochs.judge(
                &probe,
                self.last_ordinal,
                now_ms,
                run_risk,
                healed_run,
                &mut out,
            );
        }
        let any = |verdict: Verdict| self.runs.iter().any(|r| r.verdict == verdict);
        let epoch = self.epochs.state();
        self.render = if any(Verdict::DesyncRisk) || epoch == Some(EpochVerdict::DesyncRisk) {
            Render::DesyncRisk
        } else if any(Verdict::Unverified) {
            Render::Unverified
        } else if epoch == Some(EpochVerdict::Unjudged) {
            Render::Unjudged
        } else if any(Verdict::Silent) || epoch == Some(EpochVerdict::Open) {
            Render::Displaced
        } else {
            Render::Ok
        };
        out
    }

    /// The presence classifier's reading of this session's program.
    pub(crate) fn set_agent(&mut self, agent: bool) {
        self.epochs.set_agent(agent);
    }

    /// Whether the session's program is the agent class.
    pub(crate) fn agent_class(&self) -> bool {
        self.epochs.class() == crate::resize_epoch::ProgramClass::Agent
    }

    /// Whether epoch `id` is still the open one and at risk.
    pub(crate) fn still_at_risk(&self, id: u64) -> bool {
        self.epochs.still_at_risk(id)
    }

    /// The open epoch when the repaint nudge may act on it.
    pub(crate) fn nudge_view(&self) -> Option<crate::resize_epoch::NudgeView> {
        self.epochs.nudge_view()
    }

    /// A repair was attempted for epoch `id` (tried for good).
    pub(crate) fn note_tried(&mut self, id: u64) {
        self.epochs.note_tried(id);
    }

    /// [`crate::resize_epoch::EpochBook::note_superseded`].
    pub(crate) fn note_superseded(&mut self, id: u64) {
        self.epochs.note_superseded(id);
    }

    /// [`crate::resize_epoch::EpochBook::note_unrepaired`].
    pub(crate) fn note_unrepaired(&mut self, id: u64) {
        self.epochs.note_unrepaired(id);
    }

    /// [`crate::resize_epoch::EpochBook::take_returned`].
    pub(crate) fn take_returned(&mut self) -> Option<u64> {
        self.epochs.take_returned()
    }

    /// The repaint nudge's phase word and attempt count (the `resizes` header).
    pub(crate) fn set_nudge(&mut self, phase: &'static str, nudged: u32) {
        self.epochs.set_nudge(phase, nudged);
    }

    /// `aterm.log` lines the epoch judge owes (drained).
    pub(crate) fn take_logs(&mut self) -> Vec<String> {
        self.epochs.take_logs()
    }

    /// A look found the terminal locked: ask again soon.
    pub(crate) fn defer_look(&mut self, now_ms: u64) {
        self.epochs.defer_look(now_ms);
    }

    /// The instant an epoch look is owed (timeline ms).
    pub(crate) fn look_at_ms(&self) -> Option<u64> {
        self.epochs.look_at_ms()
    }

    /// An epoch look is owed and due (timeline ms).
    pub(crate) fn look_due(&self, now_ms: u64) -> bool {
        self.epochs.look_due(now_ms)
    }

    /// The epochs, for the tests and `resizes`.
    #[cfg(test)]
    pub(crate) fn epochs(&self) -> &EpochBook {
        &self.epochs
    }

    /// The run judge's word alone, as of now: the model's `verdict`.
    #[cfg(test)]
    pub(crate) fn run_render(&self) -> Render {
        let any = |verdict: Verdict| self.runs.iter().any(|r| r.verdict == verdict);
        if any(Verdict::DesyncRisk) {
            Render::DesyncRisk
        } else if any(Verdict::Unverified) {
            Render::Unverified
        } else if any(Verdict::Silent) {
            Render::Displaced
        } else {
            Render::Ok
        }
    }

    /// The render state as of the last evaluation.
    #[cfg(test)]
    pub(crate) fn render(&self) -> Render {
        self.render
    }

    /// The newest row id (0 before the first): a subscriber's high-water mark.
    pub(crate) const fn high_row(&self) -> u64 {
        self.high_row
    }

    /// The oldest retained row id.
    pub(crate) fn low_row(&self) -> Option<u64> {
        self.rows.front().map(|row| row.id)
    }

    /// The engine `probe` read has a resize this ledger has not booked (or is another engine).
    pub(crate) fn behind(&self, probe: &EngineProbe) -> bool {
        self.lifetime
            .is_some_and(|lifetime| lifetime != probe.lifetime)
            || probe.ordinal != self.last_ordinal
    }

    /// Rows with `id > after`, oldest first (a suffix: ids only increase).
    pub(crate) fn rows_since(&self, after: u64) -> impl DoubleEndedIterator<Item = &LedgerRow> {
        let start = self.rows.partition_point(|row| row.id <= after);
        self.rows.range(start..)
    }

    /// `render=<ok|displaced|desync-risk|unverified|unjudged|-> resizes=<n> flaps=<n>`, the `status` fields.
    pub(crate) fn status_fields(
        &mut self,
        probe: Option<EngineProbe>,
        now_ms: u64,
    ) -> (String, Vec<Transition>) {
        let (render, out) = match probe {
            Some(probe) => {
                let out = self.evaluate(probe, now_ms);
                (self.render.wire(), out)
            }
            None => ("-", Vec::new()),
        };
        (
            format!(
                "render={render} resizes={} flaps={}",
                self.high_row, self.flaps
            ),
            out,
        )
    }

    /// The `resizes` reply: the header, then the selected `resize` rows oldest first, then the `run` rows they belong to — and every retained run still `silent`, `desync-risk` or `unverified`, whatever the selection.
    pub(crate) fn reply(&self, n: usize, since: Option<u64>) -> String {
        let rows: Vec<&LedgerRow> = self.rows_since(since.unwrap_or(0)).collect();
        let skip = if n == 0 {
            0
        } else {
            rows.len().saturating_sub(n)
        };
        let rows = &rows[skip..];
        let mut run_ids: Vec<u64> = rows.iter().map(|row| row.run).collect();
        run_ids.extend(
            self.runs
                .iter()
                .filter(|run| run.verdict.watched())
                .map(|run| run.id),
        );
        run_ids.sort_unstable();
        run_ids.dedup();
        let runs: Vec<&Run> = run_ids
            .iter()
            .filter_map(|id| self.runs.iter().find(|run| run.id == *id))
            .collect();
        let epochs: Vec<&crate::resize_epoch::Epoch> = self.epochs.listed().collect();
        let mut out = format!(
            "OK {} total={} runs={} flaps={} risks={} render={} unledgered={} lost={} {}\n",
            rows.len() + runs.len() + epochs.len(),
            self.high_row,
            self.high_run,
            self.flaps,
            self.risks,
            self.render.wire(),
            self.unledgered,
            self.lost,
            self.epochs.nudge_wire(),
        );
        for row in rows {
            wire_row(&mut out, row);
        }
        for run in runs {
            wire_run(&mut out, run);
        }
        for epoch in epochs {
            crate::resize_epoch::wire_epoch(&mut out, epoch);
        }
        out
    }
}

/// A resize joins `run`, which `prev` ended,
/// when it came at most [`RUN_GAP_MS`] after `prev` with nothing changed on
/// the screen between them, or with `run` `silent` and this resize taking
/// `prev` back (`to` is `prev`'s `from`: a flap's second half), whatever was
/// drawn between; or when
/// `run` is `silent` and never drawn over and nothing changed on the screen
/// since `prev`, however late.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        reason = "The 250 ms bound on a run is the environment assumption the ResizeRender \
                  model states and does not model: there every step but Evaluate (a reader a \
                  second later) lands inside the gap, and Evaluate closes a run unless it is \
                  silent and undrawn, which the Tier-1 rig reproduces."
    )
)]
fn joins_run(prev: &LedgerRow, report: &ResizeReport, now_ms: u64, run: &Run) -> bool {
    let silent = run.verdict == Verdict::Silent;
    let adjoins = report.adjoins(&prev.report);
    let in_gap = now_ms.saturating_sub(prev.t_ms) <= run_gap_ms();
    let takes_back = silent && prev.timed && report.to == prev.report.from;
    (in_gap && (adjoins || takes_back)) || (silent && !run.drew && adjoins)
}

fn own_shift(report: &ResizeReport, credited: (u32, u32)) -> i32 {
    report
        .shift()
        .saturating_sub(i32::try_from(credited.0).unwrap_or(i32::MAX))
}

fn own_pushed(report: &ResizeReport, credited: (u32, u32)) -> i32 {
    report
        .net_pushed()
        .saturating_add(i32::try_from(credited.1).unwrap_or(i32::MAX))
}

fn stash_of(report: &ResizeReport) -> (u32, u32) {
    if report.stashed == 0 {
        (0, 0)
    } else {
        (u32::from(report.demoted), u32::from(report.pushed))
    }
}

fn cxr((rows, cols): (u16, u16)) -> String {
    format!("{cols}x{rows}")
}

fn wire_site(site: Site) -> String {
    match site {
        Site::Caller { cause, at } => format!(
            "site={} at={}",
            cause.word(),
            crate::control::pct_encode(&format!("{}:{}", at.file(), at.line()))
        ),
        Site::CtlCross => "site=ctl-cross at=-".to_string(),
        Site::Unledgered => "site=- at=-".to_string(),
    }
}

fn wire_row(out: &mut String, row: &LedgerRow) {
    let r = &row.report;
    let win = row
        .attribution
        .win
        .map_or_else(|| "-".to_string(), |(cols, rows)| format!("{cols}x{rows}"));
    let chrome = row
        .attribution
        .chrome
        .map_or_else(|| "-".to_string(), |k| k.to_string());
    let _ = writeln!(
        out,
        "resize {} t={} run={} ord={} from={} to={} alt={} sync={} trimmed={} demoted={} \
         pushed={} revealed={} appended={} restored={} shift={:+} cut={} {} win={win} \
         chrome={chrome}",
        row.id,
        row.t_ms,
        row.run,
        r.ordinal,
        cxr(r.from),
        cxr(r.to),
        u8::from(r.alt),
        u8::from(r.in_sync),
        r.trimmed,
        r.demoted,
        r.pushed,
        r.revealed,
        r.appended,
        u32::from(r.restored_top) + u32::from(r.restored_bottom),
        r.shift(),
        r.cut,
        wire_site(row.attribution.site),
    );
}

fn wire_run(out: &mut String, run: &Run) {
    let opt = |v: Option<u64>| v.map_or_else(|| "-".to_string(), |v| v.to_string());
    let _ = writeln!(
        out,
        "run {} t={} n={} alt={} geom={} net={} displaced={:+} verdict={} first_out_ms={} \
         healed_ms={}",
        run.id,
        run.t_ms,
        run.n,
        u8::from(run.alt),
        run.geom_chain(),
        run.net(),
        run.displaced,
        run.verdict.wire(),
        opt(run.first_out_ms),
        opt(run.healed_ms),
    );
}

/// `resizes [<n>] [since=<id>]`: returns the target session's ledger.
pub(crate) fn cmd_resizes(
    ctx: &crate::SessionCtx,
    term: &std::sync::Mutex<Terminal>,
    rest: &str,
) -> String {
    const USAGE: &str = "ERR usage: resizes [<n>] [since=<id>]\n";
    let mut n = 0usize;
    let mut since: Option<u64> = None;
    for tok in rest.split_whitespace() {
        if let Some(v) = tok.strip_prefix("since=") {
            match (!v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
                .then(|| v.parse::<u64>().ok())
                .flatten()
            {
                Some(id) => since = Some(id),
                None => return USAGE.to_string(),
            }
        } else if !tok.is_empty()
            && tok.bytes().all(|b| b.is_ascii_digit())
            && let Ok(v) = tok.parse::<usize>()
        {
            n = v;
        } else {
            return USAGE.to_string();
        }
    }
    let copy = JournalCopy::take(&crate::term_lock(term));
    let mut tl = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
    tl.ingest_resizes(&copy, None, crate::turn_ledger::now_ms());
    tl.resizes().reply(n, since)
}

/// `ResizeRender` actions no shipping function here performs, each waived where its
/// Tier-1 (`tests`) binds it.
#[cfg(any(test, debug_assertions))]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "ResizeRender",
    action = "Demote",
    reason = "The engine's alt-screen shrink with a full tail: aterm-core's grid resize, \
              journalled as a ResizeReport. The Tier-1 rig drives the real Terminal \
              through it and books it with the real ResizeLedger::ingest."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "ResizeRender",
    action = "Trim",
    reason = "The engine's alt-screen shrink with a blank tail: aterm-core's grid resize, \
              driven for real by the Tier-1 rig."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "ResizeRender",
    action = "Output",
    reason = "Application output that draws nothing (a mode set): an environment step the \
              Tier-1 rig's stand-in performs on the real Terminal, which drops the engine's \
              resize undo."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "ResizeRender",
    action = "Step",
    reason = "The application's SIGWINCH handler and its next frame: an environment step \
              the Tier-1 rig's stand-in performs on the real Terminal."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "ResizeRender",
    action = "Draw",
    reason = "An application diff frame: an environment step the Tier-1 rig's stand-in \
              performs on the real Terminal."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "ResizeRender",
    action = "Classify",
    reason = "The presence classifier's reading of the session's program (Claude Code), or \
              the session showing it clears its screen when resized: an environment fact the \
              Tier-1 rig sets with `ResizeLedger::set_agent`."
)]
#[allow(
    dead_code,
    reason = "carrier for the `spec_unmodeled` waivers above; nothing calls it. An allow, \
              not an expect: rustc does not lint a fn the declarative anchors rebuild"
)]
fn resize_render_scope_waivers() {}

#[cfg(test)]
#[path = "resize_ledger_tests.rs"]
mod tests;
