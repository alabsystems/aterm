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
            .and_then(|v| v.to_str()?.parse().ok())
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
    /// The window's chrome rows changed — the tab strip or the message band appeared or folded.
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

#[cfg(test)]
thread_local! {
    static CHROME_REGRID_SITES: std::cell::RefCell<Vec<Option<Site>>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Record the site the every-window chrome re-grid is running under.
#[cfg(test)]
pub(crate) fn note_chrome_regrid_site_for_test() {
    let site = SiteScope::current();
    CHROME_REGRID_SITES.with(|s| s.borrow_mut().push(site));
}

/// The sites recorded since the last take, oldest first.
#[cfg(test)]
pub(crate) fn take_chrome_regrid_sites_for_test() -> Vec<Option<Site>> {
    CHROME_REGRID_SITES.with(|s| std::mem::take(&mut *s.borrow_mut()))
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
    /// The window's chrome rows (strip, message band, presence row) then.
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
            match v.parse::<u64>() {
                Ok(id) => since = Some(id),
                Err(_) => return USAGE.to_string(),
            }
        } else if let Ok(v) = tok.parse::<usize>() {
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use aterm_models_render::resize_render_model;
    use aterm_spec::interp;

    use super::*;

    const LIFE: u64 = 7;

    fn report(ordinal: u64, from: u16, to: u16, alt: bool, seq: (u64, u64)) -> ResizeReport {
        ResizeReport {
            ordinal,
            from: (from, 80),
            to: (to, 80),
            alt,
            content_seq_before: seq.0,
            content_seq: seq.1,
            ..ResizeReport::default()
        }
    }

    fn copy_of(reports: &[ResizeReport], probe: EngineProbe) -> JournalCopy {
        JournalCopy {
            reports: reports.to_vec(),
            probe,
        }
    }

    #[track_caller]
    fn here() -> Attribution {
        Attribution {
            site: Site::caller(Cause::Pass, Location::caller()),
            win: Some((80, 24)),
            chrome: Some(1),
        }
    }

    fn alt_flap(first_ordinal: u64, seq: u64) -> [ResizeReport; 2] {
        let mut shrink = report(first_ordinal, 24, 23, true, (seq, seq + 1));
        shrink.demoted = 1;
        let mut grow = report(first_ordinal + 1, 23, 24, true, (seq + 1, seq + 2));
        grow.appended = 1;
        [shrink, grow]
    }

    fn probe(ordinal: u64, content_seq: u64, full_clears: u64) -> EngineProbe {
        EngineProbe {
            lifetime: LIFE,
            ordinal,
            content_seq,
            full_clears,
            screen_replaced: 0,
            debt: aterm_core::terminal::RenderDebt::default(),
            geom: (24, 80),
        }
    }

    fn run_verdict(ledger: &ResizeLedger, id: u64) -> Verdict {
        ledger
            .runs
            .iter()
            .find(|run| run.id == id)
            .expect("run")
            .verdict
    }

    #[test]
    fn grouping_joins_adjoining_resizes_within_the_gap() {
        let mut ledger = ResizeLedger::default();
        let [shrink, grow] = alt_flap(1, 10);
        ledger.ingest(&copy_of(&[shrink], probe(1, 11, 0)), Some(here()), 1_000);
        ledger.ingest(
            &copy_of(&[shrink, grow], probe(2, 12, 0)),
            Some(here()),
            1_000,
        );
        assert_eq!((ledger.high_row, ledger.high_run, ledger.flaps), (2, 1, 1));
        assert!(ledger.runs[0].net_zero());

        let mut apart = ResizeLedger::default();
        let mut trim = shrink;
        (trim.demoted, trim.trimmed) = (0, 1);
        apart.ingest(&copy_of(&[trim], probe(1, 11, 0)), Some(here()), 1_000);
        apart.ingest(
            &copy_of(&[trim, grow], probe(2, 12, 0)),
            Some(here()),
            1_300,
        );
        assert_eq!((apart.high_run, apart.flaps), (2, 0));

        let mut drawn = ResizeLedger::default();
        drawn.ingest(&copy_of(&[shrink], probe(1, 11, 0)), Some(here()), 1_000);
        let mut grow_after_draw = report(2, 23, 24, true, (15, 16));
        grow_after_draw.appended = 1;
        drawn.ingest(
            &copy_of(&[shrink, grow_after_draw], probe(2, 16, 0)),
            Some(here()),
            1_005,
        );
        assert_eq!((drawn.high_run, drawn.flaps), (1, 1));
        assert!(drawn.runs[0].drew, "the run saw the draw");
        assert_eq!(drawn.evaluate(probe(2, 16, 0), 2_005).len(), 1, "a risk");

        let mut cleared = ResizeLedger::default();
        cleared.ingest(&copy_of(&[shrink], probe(1, 11, 0)), Some(here()), 1_000);
        let mut grow_after_clear = grow_after_draw;
        grow_after_clear.full_clears = 1;
        cleared.ingest(
            &copy_of(&[shrink, grow_after_clear], probe(2, 16, 1)),
            Some(here()),
            1_005,
        );
        assert_eq!((cleared.high_run, cleared.flaps), (2, 0));
        assert_eq!(run_verdict(&cleared, 1), Verdict::Healed);
        assert_eq!(cleared.render, Render::Ok);

        let mut trimmed = ResizeLedger::default();
        let mut trim = report(1, 24, 23, true, (10, 11));
        trim.trimmed = 1;
        trimmed.ingest(&copy_of(&[trim], probe(1, 11, 0)), Some(here()), 1_000);
        trimmed.ingest(
            &copy_of(&[trim, report(2, 23, 24, true, (15, 16))], probe(2, 16, 0)),
            Some(here()),
            1_005,
        );
        assert_eq!((trimmed.high_run, trimmed.flaps), (2, 0));

        let mut held = ResizeLedger::default();
        let [mut shrink, mut back] = alt_flap(1, 10);
        shrink.stashed = 1;
        (back.appended, back.revealed, back.restored_top) = (0, 1, 1);
        held.ingest(&copy_of(&[shrink], probe(1, 11, 0)), Some(here()), 1_000);
        held.evaluate(probe(1, 11, 0), 1_300);
        assert_eq!(held.render, Render::Displaced, "meanwhile");
        held.ingest(
            &copy_of(&[shrink, back], probe(2, 12, 0)),
            Some(here()),
            1_600,
        );
        assert_eq!((held.high_run, held.flaps), (1, 1));
        assert_eq!(run_verdict(&held, 1), Verdict::None);
        assert_eq!(held.render, Render::Ok);

        let mut after = ResizeLedger::default();
        let mut shrink = report(1, 24, 23, true, (10, 11));
        shrink.demoted = 1;
        let mut grow = report(2, 23, 24, true, (15, 16));
        grow.appended = 1;
        let mut again = report(3, 24, 23, true, (16, 17));
        again.demoted = 1;
        after.ingest(&copy_of(&[shrink], probe(1, 11, 0)), Some(here()), 1_000);
        after.ingest(
            &copy_of(&[shrink, grow], probe(2, 16, 0)),
            Some(here()),
            1_005,
        );
        assert!(again.adjoins(&grow), "nothing drawn since the grow");
        after.ingest(
            &copy_of(&[shrink, grow, again], probe(3, 17, 0)),
            Some(here()),
            1_405,
        );
        assert_eq!((after.high_run, after.flaps), (2, 1));
        after.evaluate(probe(3, 17, 0), 2_100);
        assert_eq!(run_verdict(&after, 1), Verdict::DesyncRisk);

        let mut drag = ResizeLedger::default();
        let mut down1 = report(1, 24, 23, true, (10, 11));
        down1.demoted = 1;
        let mut down2 = report(2, 23, 22, true, (15, 16));
        down2.demoted = 1;
        let mut up = report(3, 22, 23, true, (20, 21));
        up.appended = 1;
        drag.ingest(&copy_of(&[down1], probe(1, 11, 0)), Some(here()), 1_000);
        drag.ingest(
            &copy_of(&[down1, down2], probe(2, 16, 0)),
            Some(here()),
            1_020,
        );
        assert_eq!(drag.high_run, 2, "23 -> 22 does not take 24 -> 23 back");
        drag.ingest(
            &copy_of(&[down1, down2, up], probe(3, 21, 0)),
            Some(here()),
            1_040,
        );
        assert_eq!((drag.high_run, drag.flaps), (2, 1), "the turn joins");

        let mut found = ResizeLedger::default();
        let mut shrink = report(1, 24, 23, true, (10, 11));
        shrink.demoted = 1;
        let mut grow = report(2, 23, 24, true, (15, 16));
        grow.appended = 1;
        found.ingest(&copy_of(&[shrink, grow], probe(2, 16, 0)), None, 9_000);
        assert_eq!((found.high_run, found.unledgered), (2, 2));
        assert!(found.rows.iter().all(|row| !row.timed));
        let mut half = ResizeLedger::default();
        half.ingest(
            &copy_of(&[shrink, grow], probe(2, 16, 0)),
            Some(here()),
            9_000,
        );
        assert_eq!((half.high_run, half.unledgered), (2, 1));
        assert!(half.rows[1].timed && !half.rows[0].timed);
        let mut quiet = ResizeLedger::default();
        let [shrink, grow] = alt_flap(1, 10);
        quiet.ingest(&copy_of(&[shrink, grow], probe(2, 12, 0)), None, 9_000);
        assert_eq!((quiet.high_run, quiet.flaps), (1, 1));
    }

    #[test]
    fn a_grow_hands_rows_back_to_the_run_that_stashed_them() {
        let shrink = |ordinal, from, to, seq: u64| {
            let mut r = report(ordinal, from, to, true, (seq, seq + 1));
            (r.demoted, r.stashed) = (1, 1);
            r
        };
        let grow = |ordinal, from, to, seq: u64, restored: bool| {
            let mut r = report(ordinal, from, to, true, (seq, seq + 1));
            if restored {
                (r.revealed, r.restored_top) = (1, 1);
            } else {
                r.appended = 1;
            }
            r
        };
        let book = |ledger: &mut ResizeLedger, reports: &[ResizeReport], at: u64| {
            let last = reports.last().expect("a report");
            ledger.ingest(
                &copy_of(reports, probe(last.ordinal, last.content_seq, 0)),
                Some(here()),
                at,
            );
        };
        let journal = [
            shrink(1, 24, 23, 10),
            shrink(2, 23, 22, 11),
            grow(3, 22, 23, 12, true),
            grow(4, 23, 24, 13, true),
        ];

        for (label, times) in [
            ("apart", [0, 300, 600, 900]),
            ("joined", [0, 300, 301, 302]),
        ] {
            let mut ledger = ResizeLedger::default();
            for (i, at) in times.into_iter().enumerate() {
                book(&mut ledger, &journal[..=i], at);
            }
            assert_eq!((ledger.high_run, ledger.flaps), (1, 1), "{label}");
            let run = &ledger.runs[0];
            assert_eq!(
                (run.displaced, run.held, run.verdict),
                (0, (0, 0), Verdict::None),
                "{label}"
            );
            assert_eq!(ledger.evaluate(probe(4, 40, 0), 5_000), vec![], "{label}");
            assert_eq!(ledger.render(), Render::Ok, "{label}");
        }

        let mut dropped = ResizeLedger::default();
        let mut control = journal;
        control[3] = grow(4, 23, 24, 13, false);
        for (i, at) in [0, 300, 600, 900].into_iter().enumerate() {
            book(&mut dropped, &control[..=i], at);
        }
        assert_eq!((dropped.high_run, dropped.runs[0].displaced), (1, -1));
        assert_eq!(dropped.evaluate(probe(4, 40, 0), 5_000).len(), 1, "a risk");
        assert_eq!(run_verdict(&dropped, 1), Verdict::DesyncRisk);
        assert_eq!(dropped.render(), Render::DesyncRisk);

        let mut other = ResizeLedger::default();
        book(&mut other, &journal[..1], 0);
        assert_eq!(other.runs[0].held, (1, 0));
        let back = grow(2, 23, 24, 11, true);
        assert_eq!(other.credit(&back, None), (1, 0), "credited elsewhere");
        assert_eq!(
            (
                other.runs[0].displaced,
                other.runs[0].held,
                other.runs[0].verdict
            ),
            (0, (0, 0), Verdict::None)
        );
        assert_eq!(own_shift(&back, (1, 0)), 0, "the grow's run counts nothing");
        let mut joining = ResizeLedger::default();
        book(&mut joining, &journal[..1], 0);
        assert_eq!(joining.credit(&back, Some(1)), (0, 0));
        assert_eq!(
            (joining.runs[0].displaced, joining.runs[0].held),
            (-1, (0, 0))
        );
    }

    #[test]
    fn a_lost_report_credits_no_older_run() {
        let mut ledger = ResizeLedger::default();
        let mut first = report(1, 24, 23, true, (10, 11));
        (first.demoted, first.stashed) = (1, 1);
        ledger.ingest(&copy_of(&[first], probe(1, 11, 0)), Some(here()), 0);
        assert_eq!(ledger.runs[0].held, (1, 0));
        let mut lost = report(2, 23, 22, true, (11, 12));
        (lost.demoted, lost.stashed) = (1, 1);
        let mut grow = report(3, 22, 23, true, (12, 13));
        (grow.revealed, grow.restored_top) = (1, 1);
        let journal = [first, lost, grow];
        ledger.ingest(&copy_of(&journal[2..], probe(3, 13, 0)), None, 1_000);
        assert_eq!(ledger.lost, 1);
        let run1 = ledger.runs.iter().find(|run| run.id == 1).expect("run 1");
        assert_eq!((run1.displaced, run1.held), (-1, (0, 0)), "no credit");
        assert_eq!(ledger.evaluate(probe(3, 40, 0), 5_000), vec![]);
        assert_eq!(run_verdict(&ledger, 1), Verdict::Unverified);
        assert_eq!(ledger.render(), Render::Unverified);
    }

    #[test]
    fn verdict_table() {
        let mut trim = ResizeLedger::default();
        let mut shrink = report(1, 24, 23, true, (10, 11));
        shrink.trimmed = 1;
        trim.ingest(&copy_of(&[shrink], probe(1, 11, 0)), Some(here()), 0);
        assert_eq!(run_verdict(&trim, 1), Verdict::None);
        trim.evaluate(probe(1, 40, 0), 5_000);
        assert_eq!(trim.render(), Render::Ok);

        let [mut shrink, mut grow] = alt_flap(1, 10);
        shrink.stashed = 1;
        (grow.appended, grow.revealed, grow.restored_top) = (0, 1, 1);
        let mut undone = ResizeLedger::default();
        undone.ingest(&copy_of(&[shrink, grow], probe(2, 12, 0)), Some(here()), 0);
        assert_eq!(run_verdict(&undone, 1), Verdict::None);
        assert_eq!(undone.evaluate(probe(2, 20, 0), 5_000), vec![]);
        assert_eq!(undone.render(), Render::Ok);
        let wire = undone.reply(0, None);
        assert!(
            wire.contains("revealed=1 appended=0 restored=1 shift=+1"),
            "{wire}"
        );
        assert!(
            wire.contains("net=zero displaced=+0 verdict=none"),
            "{wire}"
        );
        let mut pushed = report(1, 24, 23, true, (10, 11));
        (pushed.pushed, pushed.stashed) = (1, 1);
        let mut back = report(2, 23, 24, true, (11, 12));
        back.restored_bottom = 1;
        let mut corner = ResizeLedger::default();
        corner.ingest(&copy_of(&[pushed, back], probe(2, 12, 0)), Some(here()), 0);
        assert_eq!(run_verdict(&corner, 1), Verdict::None);

        let flap = alt_flap(1, 10);
        let mut healed = ResizeLedger::default();
        healed.ingest(&copy_of(&flap, probe(2, 12, 0)), Some(here()), 0);
        assert_eq!(healed.evaluate(probe(2, 12, 0), 10), vec![]);
        assert_eq!(
            healed.render(),
            Render::Displaced,
            "silent: displaced, not drawn"
        );
        assert_eq!(healed.evaluate(probe(2, 13, 1), 15), vec![]);
        assert_eq!(run_verdict(&healed, 1), Verdict::Healed);
        assert_eq!(healed.render(), Render::Ok);

        let mut risk = ResizeLedger::default();
        risk.ingest(&copy_of(&flap, probe(2, 12, 0)), Some(here()), 100);
        assert_eq!(
            risk.evaluate(probe(2, 13, 0), 105),
            vec![],
            "drawn, but inside the grace"
        );
        assert_eq!(risk.render(), Render::Displaced);
        assert_eq!(
            risk.evaluate(probe(2, 13, 0), 1_100),
            vec![Transition::Risk {
                run: 1,
                displaced: -1,
                net_zero: true,
                at_ms: 100
            }]
        );
        assert_eq!(risk.render(), Render::DesyncRisk);
        assert_eq!(
            risk.evaluate(probe(2, 14, 0), 2_000),
            vec![],
            "recorded once"
        );
        assert_eq!(
            risk.evaluate(probe(2, 15, 1), 2_500),
            vec![Transition::Healed {
                run: 1,
                after_ms: 2_400
            }]
        );
        assert_eq!(risk.render(), Render::Ok);
        assert_eq!(risk.risks, 1);
        let wire = risk.reply(0, None);
        assert!(
            wire.contains("verdict=healed first_out_ms=5 healed_ms=2400"),
            "{wire}"
        );

        let mut silent = ResizeLedger::default();
        silent.ingest(&copy_of(&flap, probe(2, 12, 0)), Some(here()), 0);
        silent.evaluate(probe(2, 12, 0), 60_000);
        assert_eq!(run_verdict(&silent, 1), Verdict::Silent);
        assert_eq!(silent.render(), Render::Displaced);

        let mut changed = ResizeLedger::default();
        let mut down = report(1, 24, 22, true, (10, 11));
        down.demoted = 2;
        changed.ingest(&copy_of(&[down], probe(1, 11, 0)), Some(here()), 0);
        assert_eq!(changed.evaluate(probe(1, 11, 0), 5_000), vec![]);
        assert_eq!(changed.render(), Render::Displaced, "not drawn yet");
        assert_eq!(changed.evaluate(probe(1, 30, 0), 6_000), vec![]);
        assert_eq!(run_verdict(&changed, 1), Verdict::Unverified);
        assert_eq!(changed.render(), Render::Unverified);
        assert_eq!(changed.risks, 0);
        let wire = changed.reply(0, Some(1));
        assert!(
            wire.contains("render=unverified")
                && wire.contains("net=changed displaced=-2 verdict=unverified"),
            "the run render= names is listed whatever the selection: {wire}"
        );
        assert_eq!(changed.evaluate(probe(1, 31, 1), 7_000), vec![]);
        assert_eq!(run_verdict(&changed, 1), Verdict::Healed);
        assert_eq!(changed.render(), Render::Ok);

        let mut reflow = ResizeLedger::default();
        let mut wide = report(1, 24, 24, false, (10, 12));
        wide.to = (24, 100);
        wide.reflowed = true;
        wide.demoted = 3;
        reflow.ingest(&copy_of(&[wide], probe(1, 12, 0)), Some(here()), 0);
        assert_eq!(run_verdict(&reflow, 1), Verdict::Reflow);
        reflow.evaluate(probe(1, 20, 0), 5_000);
        assert_eq!(reflow.render(), Render::Ok);

        let mut primary = ResizeLedger::default();
        let mut down = report(1, 24, 23, false, (10, 11));
        down.demoted = 1;
        primary.ingest(&copy_of(&[down], probe(1, 11, 0)), Some(here()), 0);
        primary.evaluate(probe(1, 30, 0), 5_000);
        assert_eq!(run_verdict(&primary, 1), Verdict::None);
        assert_eq!(primary.render(), Render::Ok);
    }

    #[test]
    fn a_later_resize_is_not_the_app_drawing() {
        let mut ledger = ResizeLedger::default();
        let flap = alt_flap(1, 10);
        ledger.ingest(&copy_of(&flap, probe(2, 12, 0)), Some(here()), 0);
        let mut third = report(3, 24, 23, true, (12, 13));
        third.trimmed = 1;
        ledger.ingest(
            &copy_of(&[flap[0], flap[1], third], probe(3, 13, 0)),
            Some(here()),
            300,
        );
        ledger.evaluate(probe(3, 13, 0), 5_000);
        assert!(!ledger.runs[0].drew);
        assert_eq!(run_verdict(&ledger, 1), Verdict::Silent);
    }

    #[test]
    fn a_late_stale_copy_books_nothing_and_keeps_the_counts() {
        let mut ledger = ResizeLedger::default();
        let mut journal: Vec<ResizeReport> = Vec::new();
        for ordinal in 1..=40u64 {
            let r = report(ordinal, 24, 24, true, (ordinal, ordinal + 1));
            journal.push(r);
            let start = journal.len().saturating_sub(16);
            ledger.ingest(
                &copy_of(&journal[start..], probe(ordinal, ordinal + 1, 0)),
                Some(here()),
                ordinal * 1_000,
            );
        }
        let before = ledger.reply(1, None);
        let stale = copy_of(&journal[journal.len() - 16..], probe(40, 41, 0));
        let mut r41 = report(41, 24, 23, true, (41, 42));
        r41.demoted = 1;
        journal.push(r41);
        let fresh = copy_of(&journal[journal.len() - 16..], probe(41, 42, 0));
        ledger.ingest(&fresh, None, 41_000);
        assert_eq!(
            (ledger.high_row, ledger.unledgered, ledger.lost),
            (41, 1, 0)
        );
        let at_41 = (ledger.high_row, ledger.high_run, ledger.flaps, ledger.risks);

        assert_eq!(ledger.ingest(&stale, Some(here()), 41_001), vec![]);
        assert_eq!(
            (ledger.high_row, ledger.high_run, ledger.flaps, ledger.risks),
            at_41
        );
        assert_eq!((ledger.unledgered, ledger.lost), (1, 0));
        assert_eq!(ledger.last_ordinal, 41);
        assert!(!ledger.runs.back().expect("run 41").drew, "no fake draw");
        let after = ledger.reply(1, None);
        assert!(after.starts_with("OK 2 total=41 "), "{before}\n{after}");

        let site = here();
        ledger.ingest(&fresh, Some(site), 41_002);
        assert_eq!((ledger.high_row, ledger.unledgered), (41, 0));
        assert_eq!(ledger.rows.back().expect("row").attribution, site);
    }

    #[test]
    fn a_stale_probe_neither_draws_nor_heals() {
        let mut ledger = ResizeLedger::default();
        let flap = alt_flap(1, 10);
        let stale = probe(0, 10, 0);
        ledger.ingest(&copy_of(&flap, probe(2, 12, 0)), Some(here()), 100);
        assert_eq!(ledger.evaluate(stale, 101), vec![]);
        assert!(!ledger.runs[0].drew);
        assert_eq!(ledger.evaluate(probe(2, 12, 0), 1_600), vec![]);
        assert_eq!(run_verdict(&ledger, 1), Verdict::Silent, "never drawn");
        assert_eq!(ledger.render(), Render::Displaced);

        let mut ledger = ResizeLedger::default();
        let mut flap = alt_flap(5, 20);
        for r in &mut flap {
            r.full_clears = 3;
        }
        ledger.ingest(&copy_of(&flap, probe(6, 22, 3)), Some(here()), 100);
        let before_clear = EngineProbe {
            full_clears: 2,
            ..probe(4, 18, 2)
        };
        ledger.evaluate(before_clear, 102);
        assert_eq!(run_verdict(&ledger, 1), Verdict::Silent, "not healed");
        let ahead = EngineProbe {
            full_clears: 3,
            ..probe(7, 30, 3)
        };
        ledger.evaluate(ahead, 1_500);
        assert!(!ledger.runs[0].drew, "an unbooked resize is not a draw");
        let ahead_healed = EngineProbe {
            full_clears: 4,
            ..ahead
        };
        ledger.evaluate(ahead_healed, 1_600);
        assert_eq!(run_verdict(&ledger, 1), Verdict::Healed);
    }

    #[test]
    fn a_restored_engine_rebases_the_open_runs() {
        let mut ledger = ResizeLedger::default();
        let mut flap = alt_flap(1, 100);
        for r in &mut flap {
            r.full_clears = 9;
        }
        ledger.ingest(&copy_of(&flap, probe(2, 102, 9)), Some(here()), 100);
        let reborn = |ordinal, content_seq, full_clears| EngineProbe {
            lifetime: LIFE + 1,
            ..probe(ordinal, content_seq, full_clears)
        };
        let fresh = report(1, 24, 24, true, (4, 5));
        ledger.ingest(&copy_of(&[fresh], reborn(1, 5, 0)), Some(here()), 200);
        assert_eq!((ledger.lifetime, ledger.last_ordinal), (Some(LIFE + 1), 1));
        assert_eq!((ledger.high_row, ledger.lost, ledger.unledgered), (3, 0, 1));
        assert_eq!(run_verdict(&ledger, 1), Verdict::Silent, "carried over");
        ledger.evaluate(probe(2, 500, 9), 2_000);
        assert!(!ledger.runs[0].drew);
        ledger.evaluate(reborn(1, 6, 1), 2_100);
        assert_eq!(run_verdict(&ledger, 1), Verdict::Healed);
        assert!(!ledger.behind(&reborn(1, 6, 1)));
        assert!(ledger.behind(&probe(2, 102, 9)), "another engine");
    }

    #[test]
    fn ring_bounds_backfill_and_loss() {
        let mut ledger = ResizeLedger::default();
        let mut journal: Vec<ResizeReport> = Vec::new();
        for ordinal in 1..=200u64 {
            let r = report(ordinal, 24, 24, true, (ordinal, ordinal + 1));
            journal.push(r);
            let start = journal.len().saturating_sub(16);
            ledger.ingest(
                &copy_of(&journal[start..], probe(ordinal, ordinal + 1, 0)),
                Some(here()),
                ordinal * 1_000,
            );
        }
        assert_eq!(ledger.rows.len(), LEDGER_ROWS_CAP);
        assert_eq!(ledger.high_row, 200);
        assert_eq!(ledger.low_row(), Some(73), "72 evicted");
        assert!(ledger.runs.len() <= LEDGER_RUNS_CAP);
        assert_eq!((ledger.unledgered, ledger.lost), (0, 0));

        let r201 = report(201, 24, 23, true, (202, 203));
        let r202 = report(202, 23, 24, true, (203, 204));
        journal.extend([r201, r202]);
        let start = journal.len() - 16;
        assert!(ledger.behind(&probe(202, 204, 0)));
        ledger.ingest(
            &copy_of(&journal[start..], probe(202, 204, 0)),
            None,
            300_000,
        );
        assert_eq!(
            (ledger.high_row, ledger.unledgered, ledger.lost),
            (202, 2, 0)
        );
        assert!(!ledger.behind(&probe(202, 204, 0)));
        ledger.ingest(
            &copy_of(&journal[start..], probe(202, 204, 0)),
            Some(here()),
            300_001,
        );
        assert_eq!((ledger.high_row, ledger.unledgered), (202, 1));
        let last = ledger.rows.back().expect("row");
        assert!(matches!(last.attribution.site, Site::Caller { .. }));

        for ordinal in 203..=222u64 {
            journal.push(report(ordinal, 24, 24, true, (ordinal + 2, ordinal + 3)));
        }
        let start = journal.len() - 16;
        ledger.ingest(
            &copy_of(&journal[start..], probe(222, 225, 0)),
            None,
            400_000,
        );
        assert_eq!(ledger.lost, 4);
        assert_eq!(ledger.last_ordinal, 222);
    }

    #[test]
    fn wire_goldens() {
        let mut ledger = ResizeLedger::default();
        let [mut shrink, grow] = alt_flap(1, 10);
        shrink.in_sync = true;
        let site: &'static Location<'static> = Location::caller();
        let attribution = Attribution {
            site: Site::caller(Cause::Chrome, site),
            win: Some((135, 63)),
            chrome: Some(2),
        };
        ledger.ingest(&copy_of(&[shrink], probe(1, 11, 0)), Some(attribution), 7);
        ledger.ingest(
            &copy_of(&[shrink, grow], probe(2, 12, 0)),
            Some(Attribution::CTL_CROSS),
            7,
        );
        let at = format!("{}:{}", site.file(), site.line());
        assert_eq!(
            ledger.reply(0, None),
            format!(
                "OK 3 total=2 runs=1 flaps=1 risks=0 render=displaced unledgered=0 lost=0 nudge=idle nudged=0\n\
                 resize 1 t=7 run=1 ord=1 from=80x24 to=80x23 alt=1 sync=1 trimmed=0 demoted=1 \
                 pushed=0 revealed=0 appended=0 restored=0 shift=-1 cut=0 site=chrome at={at} win=135x63 chrome=2\n\
                 resize 2 t=7 run=1 ord=2 from=80x23 to=80x24 alt=1 sync=0 trimmed=0 demoted=0 \
                 pushed=0 revealed=0 appended=1 restored=0 shift=+0 cut=0 site=ctl-cross at=- win=- chrome=-\n\
                 run 1 t=7 n=2 alt=1 geom=80x24>80x23>80x24 net=zero displaced=-1 \
                 verdict=silent first_out_ms=- healed_ms=-\n"
            )
        );
        assert_eq!(
            ledger
                .reply(1, None)
                .lines()
                .nth(1)
                .map(|l| l.starts_with("resize 2 ")),
            Some(true)
        );
        assert_eq!(
            ledger.reply(0, Some(2)),
            "OK 1 total=2 runs=1 flaps=1 risks=0 render=displaced unledgered=0 lost=0 nudge=idle nudged=0\n\
             run 1 t=7 n=2 alt=1 geom=80x24>80x23>80x24 net=zero displaced=-1 \
             verdict=silent first_out_ms=- healed_ms=-\n"
        );
        assert!(!at.contains(' '), "a site is one token");
        assert_eq!(
            Transition::Risk {
                run: 3,
                displaced: -1,
                net_zero: true,
                at_ms: 40
            }
            .payload(),
            "desync-risk run=3 displaced=-1 net=zero at=40"
        );
        assert_eq!(
            Transition::Healed {
                run: 3,
                after_ms: 900
            }
            .payload(),
            "healed run=3 after_ms=900"
        );

        let mut run = Run::start(1, &report(1, 30, 29, true, (1, 2)), 0, (0, 0));
        for (i, rows) in (20..29u16).rev().enumerate() {
            run.extend(
                &report(2 + i as u64, rows + 1, rows, true, (0, 0)),
                0,
                (0, 0),
            );
        }
        assert_eq!(
            run.geom_chain(),
            "80x30>80x29>80x28>80x27>80x26>80x25>80x24>...>80x20"
        );
    }

    #[test]
    fn the_outermost_entry_point_names_the_site() {
        assert_eq!(SiteScope::current(), None);
        let (outer, outer_line) = (SiteScope::enter(Cause::Chrome), line!());
        {
            let _inner = SiteScope::enter(Cause::Window);
            match SiteScope::current() {
                Some(Site::Caller { cause, at }) => {
                    assert_eq!((cause, at.line()), (Cause::Chrome, outer_line));
                }
                other => panic!("{other:?}"),
            }
        }
        assert!(SiteScope::current().is_some(), "the inner exit keeps it");
        drop(outer);
        assert_eq!(SiteScope::current(), None);
        let unwound = std::panic::catch_unwind(|| {
            let _scope = SiteScope::enter(Cause::Input);
            panic!("inside a resize entry point");
        });
        assert!(unwound.is_err());
        assert_eq!(SiteScope::current(), None, "an unwind leaves the scope");
    }

    #[test]
    fn the_bands_and_the_strips_regrid_is_booked_as_chrome_from_the_line_that_asked() {
        let mut app = crate::App::headless_for_test();
        let _ = take_chrome_regrid_sites_for_test();
        let line = line!() + 1;
        app.regrid_for_chrome_rows();
        match take_chrome_regrid_sites_for_test().as_slice() {
            [
                Some(Site::Caller {
                    cause: Cause::Chrome,
                    at,
                }),
            ] => assert_eq!((at.file(), at.line()), (file!(), line)),
            other => panic!("{other:?}"),
        }
        assert_eq!(SiteScope::current(), None, "left on return");
        let now = std::time::Instant::now();
        let msg = aterm_messages::Message::new(
            aterm_messages::tags::CONFIG,
            aterm_messages::Severity::Warn,
            "Disk nearly full",
        )
        .now(aterm_messages::need::Ask::flash(
            aterm_messages::need::Why::OnlyYou(aterm_messages::need::Reason::new(
                "the disk is nearly full",
            )),
            aterm_messages::need::Then::step("Free some space"),
            aterm_messages::need::Span::Ten,
        ));
        let stamp = aterm_messages::WallStamp {
            unix_ms: 1_790_000_000_000,
        };
        let _ = app.messages.post(msg, stamp, now);
        let _ = app.settle_messages(now);
        assert_eq!(app.message_band_rows, 1, "the row reached the glass");
        match take_chrome_regrid_sites_for_test().as_slice() {
            [
                Some(Site::Caller {
                    cause: Cause::Chrome,
                    at,
                }),
            ] => assert!(at.file().ends_with("messages_host.rs"), "{at}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn cmd_resizes_reads_the_real_engine_and_refuses_bad_args() {
        let session = crate::stub_session(1);
        let (ctx, term) = (&session.ctx, &session.term);
        {
            let mut t = crate::term_lock(term);
            *t = claude_shaped_alt(8);
            t.resize(7, 20);
            t.resize(8, 20);
        }
        let reply = cmd_resizes(ctx, term, "");
        let mut lines = reply.lines();
        assert_eq!(
            lines.next(),
            Some(
                "OK 3 total=2 runs=1 flaps=1 risks=0 render=ok unledgered=2 lost=0 nudge=idle nudged=0"
            ),
            "{reply}"
        );
        assert!(lines.next().is_some_and(|l| l.contains("demoted=1")
            && l.contains(" ord=1 ")
            && l.contains("site=- at=-")));
        assert!(
            lines
                .next()
                .is_some_and(|l| l.contains("appended=0 restored=1")),
            "the engine undid the quiet flap: {reply}"
        );
        assert!(
            lines
                .next()
                .is_some_and(|l| l.contains("net=zero displaced=+0 verdict=none"))
        );
        assert_eq!(
            cmd_resizes(ctx, term, "since=x"),
            "ERR usage: resizes [<n>] [since=<id>]\n"
        );
        assert_eq!(
            cmd_resizes(ctx, term, "bogus"),
            "ERR usage: resizes [<n>] [since=<id>]\n"
        );
        assert!(cmd_resizes(ctx, term, "since=2").starts_with("OK 0 "));
    }

    #[test]
    fn the_window_pass_books_its_caller_the_grid_and_the_chrome() {
        let mut app = crate::App::headless_for_test();
        let wid = crate::WindowId(0);
        let chrome = app.chrome_rows(wid);
        let (shrunk, shrink_line) = (app.apply_term_resize(wid, 23, 80), line!());
        let (grown, grow_line) = (app.apply_term_resize(wid, 24, 80), line!());
        assert!(shrunk && grown);
        assert!(!app.apply_term_resize(wid, 24, 80), "unchanged: no pass");
        app.resize_panes(wid);
        assert_eq!(SiteScope::current(), None, "every entry point left");
        let session = app.pool.get(0).expect("session 0");
        assert_eq!(crate::term_lock(&session.term).resize_ordinal(), 2);
        let reply = session
            .ctx
            .timeline
            .lock()
            .unwrap()
            .resizes()
            .reply(0, None);
        let rows: Vec<&str> = reply.lines().filter(|l| l.starts_with("resize ")).collect();
        assert_eq!(rows.len(), 2, "{reply}");
        let site = |line: u32| format!(" site=term at={}:{line} ", file!());
        assert!(
            rows[0].contains(" from=80x24 to=80x23 ")
                && rows[0].contains(&site(shrink_line))
                && rows[0].ends_with(&format!(" win=80x23 chrome={chrome}")),
            "{reply}"
        );
        assert!(
            rows[1].contains(" from=80x23 to=80x24 ")
                && rows[1].contains(&site(grow_line))
                && rows[1].ends_with(&format!(" win=80x24 chrome={chrome}")),
            "{reply}"
        );
        assert!(
            reply.contains(" unledgered=0 lost=0 nudge=idle nudged=0\n"),
            "{reply}"
        );
    }

    #[test]
    fn the_band_regrid_books_site_chrome() {
        let sites = |app: &crate::App| -> Vec<String> {
            let reply = app
                .pool
                .get(0)
                .expect("session 0")
                .ctx
                .timeline
                .lock()
                .unwrap()
                .resizes()
                .reply(0, None);
            reply
                .lines()
                .filter(|l| l.starts_with("resize "))
                .filter_map(|l| l.split(' ').find(|w| w.starts_with("site=")))
                .map(str::to_owned)
                .collect()
        };
        let wid = crate::WindowId(0);
        let mut app = crate::App::headless_for_test();
        app.on_resize(wid, aterm_window::dpi::PhysicalSize::new(1600, 1000));
        assert_eq!(sites(&app), ["site=window"], "the plain window pass");
        {
            let _site = SiteScope::enter(Cause::Chrome);
            app.on_resize(wid, aterm_window::dpi::PhysicalSize::new(1600, 900));
        }
        assert_eq!(SiteScope::current(), None, "the scope left");
        assert_eq!(
            sites(&app),
            ["site=window", "site=chrome"],
            "the chrome pass"
        );
    }

    #[test]
    fn a_changed_size_repaint_without_a_clear_is_unverified_not_a_risk() {
        let mut term = claude_shaped_alt(8);
        let mut ledger = ResizeLedger::default();
        term.resize(6, COLS);
        ledger.ingest(&JournalCopy::take(&term), Some(here()), 1_000);
        let run = ledger.runs.back().expect("run");
        assert_eq!((run.net_zero(), run.displaced), (false, -2));
        let mut repaint = String::from("\x1b[?2026h");
        let frame = frame(6, 1);
        for (r, text) in frame.iter().enumerate() {
            let _ = write!(repaint, "\x1b[{};1H{text}\x1b[K", r + 1);
        }
        repaint.push_str("\x1b[4;3H\x1b[?2026l");
        term.process(repaint.as_bytes());
        let shown: Vec<String> = (0..6)
            .map(|r| term.row_text(r).unwrap_or_default().trim_end().to_string())
            .collect();
        assert_eq!(shown, frame, "the screen is the app's frame");
        let risen = ledger.evaluate(EngineProbe::sample(&term), 2_500);
        assert_eq!(risen, vec![], "no risk recorded");
        assert_eq!(run_verdict(&ledger, 1), Verdict::Unverified);
        assert_eq!(ledger.render(), Render::Unverified);
    }

    const ROWS: u16 = 8;
    const COLS: u16 = 20;
    const CAP: i64 = 2;

    fn frame(rows: u16, tick: u32) -> Vec<String> {
        (0..rows)
            .map(|r| {
                if r + 1 == rows {
                    format!("F{tick}")
                } else {
                    format!("L{r}")
                }
            })
            .collect()
    }

    fn claude_shaped_alt(rows: u16) -> Terminal {
        let mut t = Terminal::new(rows, COLS);
        t.process(b"\x1b[?1049h");
        let mut app = App {
            rows,
            believed: Vec::new(),
            tick: 0,
        };
        app.repaint(&mut t, rows);
        t
    }

    struct App {
        rows: u16,
        believed: Vec<String>,
        tick: u32,
    }

    impl App {
        fn repaint(&mut self, t: &mut Terminal, rows: u16) {
            self.rows = rows;
            self.believed = frame(rows, self.tick);
            let mut bytes = String::from("\x1b[?2026h\x1b[2J");
            for (r, text) in self.believed.iter().enumerate() {
                let _ = write!(bytes, "\x1b[{};1H{text}", r + 1);
            }
            let _ = write!(bytes, "\x1b[{};3H\x1b[?2026l", rows - 2);
            t.process(bytes.as_bytes());
        }

        fn diff(&mut self, t: &mut Terminal) {
            self.tick += 1;
            let last = usize::from(self.rows) - 1;
            let text = format!("F{}", self.tick);
            t.process(
                format!(
                    "\x1b[?2026h\x1b[{};1H{text}\x1b[K\x1b[{};3H\x1b[?2026l",
                    last + 1,
                    self.rows - 2
                )
                .as_bytes(),
            );
            self.believed[last] = text;
        }
    }

    struct Rig {
        term: Terminal,
        app: App,
        ledger: ResizeLedger,
        now_ms: u64,
        drew: bool,
        /// Ground truth: a reader came by past the run gap while the grid
        /// was still one row short and its content displaced, and that
        /// displacement is still there (`blind`).
        blind: bool,
        evaluated: bool,
        /// `render=` at the last Evaluate: 0 ok or displaced, 1 desync-risk,
        /// 2 unverified.
        verdict: i64,
        /// The last evaluation left a run `silent` (`shown`).
        shown: bool,
        /// The displacement when the ledger's open run opened (`d0`).
        d0: i64,
        /// Output that drew nothing landed inside the open run (`out`).
        out: bool,
        /// What the last grow left on the bottom row until the app drew
        /// (`bot`): 1 the appended blank, 2 the row the undo handed back.
        bot: i64,
        /// Stand in for the HISTORICAL engine: drop the resize undo before
        /// every grow, so the grow can only append (the negative control).
        historical: bool,
        /// The presence classifier reads Claude Code (`judged`).
        judged: bool,
        /// The epoch judge's word at the last Evaluate: 0 ok, 1 `desync-risk`,
        /// 2 `unjudged` (`ev`).
        ev: i64,
    }

    impl Rig {
        fn new() -> Self {
            let mut term = Terminal::new(ROWS, COLS);
            term.process(b"\x1b[?1049h");
            let mut app = App {
                rows: ROWS,
                believed: Vec::new(),
                tick: 0,
            };
            app.repaint(&mut term, ROWS);
            Self {
                term,
                app,
                ledger: ResizeLedger::default(),
                now_ms: 1_000,
                drew: false,
                blind: false,
                evaluated: false,
                verdict: 0,
                shown: false,
                d0: 0,
                out: false,
                bot: 0,
                historical: false,
                judged: false,
                ev: 0,
            }
        }

        fn displacement(&self) -> i64 {
            let top = self.term.row_text(0).unwrap_or_default();
            let top = top.trim_end();
            (0..=CAP)
                .find(|k| {
                    usize::try_from(*k)
                        .ok()
                        .and_then(|k| self.app.believed.get(k))
                        .is_some_and(|row| row == top)
                })
                .unwrap_or_else(|| panic!("row 0 {top:?} is no shift of {:?}", self.app.believed))
        }

        fn open_run(&self, probe: &EngineProbe) -> Option<&Run> {
            let (prev, run) = (self.ledger.rows.back()?, self.ledger.runs.back()?);
            let unhealed = probe.full_clears == prev.report.full_clears
                && probe.screen_replaced == prev.report.screen_replaced;
            let adjoins = prev.report.content_seq == probe.content_seq;
            let silent = run.verdict == Verdict::Silent;
            let in_gap = self.now_ms.saturating_sub(prev.t_ms) <= RUN_GAP_MS;
            (prev.run == run.id
                && run.verdict.joinable()
                && unhealed
                && ((in_gap && (adjoins || silent)) || (silent && !run.drew && adjoins)))
                .then_some(run)
        }

        fn project(&self) -> BTreeMap<&'static str, i64> {
            let probe = EngineProbe::sample(&self.term);
            let open = self.open_run(&probe);
            let open_id = open.map(|run| run.id);
            let closed: Vec<&Run> = self
                .ledger
                .runs
                .iter()
                .filter(|run| {
                    Some(run.id) != open_id
                        && run.judged()
                        && !run.healed_by(probe.full_clears, probe.screen_replaced)
                })
                .collect();
            let led: i64 = closed
                .iter()
                .filter(|run| run.net_zero() && run.displaced < 0)
                .map(|run| -i64::from(run.displaced))
                .sum();
            let lr = closed.iter().any(|run| {
                run.net_zero()
                    && run.displaced != 0
                    && (run.drew || probe.content_seq > run.quiet_seq)
            });
            let lb = closed
                .iter()
                .any(|run| !run.net_zero() && run.displaced < 0);
            let br = closed.iter().any(|run| {
                !run.net_zero()
                    && run.displaced < 0
                    && (run.drew || probe.content_seq > run.quiet_seq)
            });
            let late = open.is_some()
                && self
                    .ledger
                    .rows
                    .back()
                    .is_some_and(|prev| self.now_ms.saturating_sub(prev.t_ms) > RUN_GAP_MS);
            let (ro, rs, rd, rw) = open.map_or((0, 0, 0, false), |run| {
                (
                    1,
                    i64::from(run.start.0 != ROWS),
                    -i64::from(run.displaced),
                    run.drew
                        || (run.verdict == Verdict::Silent && probe.content_seq > run.quiet_seq),
                )
            });
            let (shift, pushed) = self.term.render_debt().rows();
            let debt = i64::from(pushed) - i64::from(shift);
            let eg = if debt == 0 {
                0
            } else {
                self.ledger
                    .epochs()
                    .current()
                    .map_or(0, |epoch| i64::from(epoch.start.0 != ROWS))
            };
            BTreeMap::from([
                ("debt", debt),
                ("eg", eg),
                ("judged", i64::from(self.judged)),
                ("ev", self.ev),
                ("geom", i64::from(self.term.rows() != ROWS)),
                ("app_geom", i64::from(self.app.rows != ROWS)),
                ("disp", self.displacement()),
                ("ro", ro),
                ("rs", rs),
                ("rd", rd),
                ("rw", i64::from(rw)),
                ("late", i64::from(late)),
                ("led", led),
                ("lr", i64::from(lr)),
                ("lb", i64::from(lb)),
                ("br", i64::from(br)),
                ("blind", i64::from(self.blind)),
                ("drew", i64::from(self.drew)),
                ("evaluated", i64::from(self.evaluated)),
                ("verdict", self.verdict),
                ("shown", i64::from(self.shown)),
                ("ls", 0),
                (
                    "st",
                    i64::try_from(self.term.grid().resize_undo_rows()).unwrap_or(i64::MAX),
                ),
                ("d0", self.d0),
                ("bot", self.bot),
                ("out", i64::from(self.out)),
            ])
        }

        fn resize(&mut self, rows: u16) -> ResizeReport {
            if self.open_run(&EngineProbe::sample(&self.term)).is_none() {
                self.d0 = self.displacement();
                self.out = false;
            }
            self.term.resize(rows, COLS);
            let copy = JournalCopy::take(&self.term);
            self.now_ms += 1;
            self.ledger
                .ingest(&copy, Some(here_attribution()), self.now_ms);
            *copy.reports.last().expect("the resize was journalled")
        }

        fn diff(&mut self) {
            if self.displacement() >= 1 {
                self.drew = true;
            }
            if self.open_run(&EngineProbe::sample(&self.term)).is_some() {
                self.out = true;
            }
            self.app.diff(&mut self.term);
            self.bot = 0;
            self.now_ms += 1;
        }

        fn drive(&mut self, step: Step) -> &'static str {
            self.evaluated = false;
            match step {
                Step::Shrink => {
                    let report = self.resize(ROWS - 1);
                    match (report.demoted, report.trimmed, report.pushed) {
                        (1, 0, 0) => "Demote",
                        (0, 1, 0) => "Trim",
                        other => panic!("a one-row shrink did {other:?}"),
                    }
                }
                Step::Grow => {
                    if self.historical {
                        self.term.grid_mut().drop_resize_undo();
                    }
                    let report = self.resize(ROWS);
                    self.bot = match (report.revealed, report.restored_top, report.appended) {
                        (0, 0, 1) => 1,
                        (1, 1, 0) => 2,
                        other => panic!("a one-row grow did {other:?}"),
                    };
                    if self.displacement() == 0 {
                        self.blind = false;
                    }
                    "Grow"
                }
                Step::Handler => {
                    let rows = self.term.rows();
                    if rows == self.app.rows {
                        self.diff();
                    } else {
                        self.app.repaint(&mut self.term, rows);
                        self.drew = false;
                        self.blind = false;
                        self.bot = 0;
                        self.now_ms += 1;
                    }
                    "Step"
                }
                Step::Output => {
                    self.term.process(b"\x1b[?1000h\x1b[?1000l");
                    if self.open_run(&EngineProbe::sample(&self.term)).is_some() {
                        self.out = true;
                    }
                    self.now_ms += 1;
                    "Output"
                }
                Step::Draw => {
                    self.diff();
                    "Draw"
                }
                Step::Classify => {
                    self.judged = true;
                    self.ledger.set_agent(true);
                    "Classify"
                }
                Step::Evaluate => {
                    if self.displacement() >= 1 && self.term.rows() != ROWS {
                        self.blind = true;
                    }
                    self.now_ms += RISK_AFTER_MS;
                    let probe = EngineProbe::sample(&self.term);
                    self.ledger.evaluate(probe, self.now_ms);
                    self.evaluated = true;
                    self.verdict = match self.ledger.run_render() {
                        Render::DesyncRisk => 1,
                        Render::Unverified => 2,
                        Render::Ok | Render::Displaced => 0,
                        Render::Unjudged => panic!("the run judge never says unjudged"),
                    };
                    self.ev = match self.ledger.epochs().current().map(|e| e.verdict) {
                        Some(EpochVerdict::DesyncRisk) => 1,
                        Some(EpochVerdict::Unjudged) => 2,
                        _ => 0,
                    };
                    self.shown = self
                        .ledger
                        .runs
                        .iter()
                        .any(|run| run.verdict == Verdict::Silent);
                    "Evaluate"
                }
            }
        }
    }

    #[track_caller]
    fn here_attribution() -> Attribution {
        Attribution {
            site: Site::caller(Cause::Term, Location::caller()),
            win: Some((COLS, ROWS)),
            chrome: Some(0),
        }
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Step {
        Shrink,
        Grow,
        Handler,
        Draw,
        Output,
        Classify,
        Evaluate,
    }

    const STEPS: [Step; 7] = [
        Step::Shrink,
        Step::Grow,
        Step::Handler,
        Step::Draw,
        Step::Output,
        Step::Classify,
        Step::Evaluate,
    ];

    fn conform(
        model: &aterm_spec::derive::Model,
        schedule: &[Step],
        seen: &mut BTreeMap<&'static str, usize>,
    ) -> Option<BTreeMap<&'static str, i64>> {
        conform_answered(&mut Answers::new(model), schedule, seen)
    }

    struct Answers<'m> {
        model: &'m aterm_spec::derive::Model,
        fired: BTreeMap<Projected, BTreeMap<&'static str, Projected>>,
        holds: std::collections::BTreeSet<Projected>,
    }

    type Projected = BTreeMap<&'static str, i64>;

    impl<'m> Answers<'m> {
        fn new(model: &'m aterm_spec::derive::Model) -> Self {
            Self {
                model,
                fired: BTreeMap::new(),
                holds: std::collections::BTreeSet::new(),
            }
        }
    }

    fn conform_answered(
        answers: &mut Answers<'_>,
        schedule: &[Step],
        seen: &mut BTreeMap<&'static str, usize>,
    ) -> Option<BTreeMap<&'static str, i64>> {
        let model = answers.model;
        let mut rig = Rig::new();
        let mut state = rig.project();
        assert_eq!(
            state,
            model.init_state(),
            "the rig starts where the model does"
        );
        for &step in schedule {
            // The model bounds the displacement at CAP; a shrink past it is
            // outside the box, so the schedule stops there.
            if step == Step::Shrink && state["disp"] >= CAP {
                return None;
            }
            if step == Step::Classify && state["judged"] == 1 {
                return None;
            }
            let action = rig.drive(step);
            let known = answers
                .fired
                .get(&state)
                .and_then(|by_action| by_action.get(action))
                .cloned();
            if known.is_none() {
                assert!(
                    model.action_enabled(action, &state),
                    "{schedule:?}: real `{action}` from a state the model disables: {state:?}"
                );
            }
            if action == "Grow" && state["late"] == 1 {
                *seen.entry("Grow joins a late run").or_default() += 1;
            }
            let expected = known.unwrap_or_else(|| {
                let mut expected = state.clone();
                assert!(model.fire(action, &mut expected));
                answers
                    .fired
                    .entry(state.clone())
                    .or_default()
                    .insert(action, expected.clone());
                expected
            });
            let real = rig.project();
            assert_eq!(real, expected, "{schedule:?}: `{action}` from {state:?}");
            if !answers.holds.contains(&real) {
                for invariant in [
                    "RiskIsFlagged",
                    "NoSilentPass",
                    "NoFalseVerdict",
                    "DisplacedIsShown",
                    "NoFalseDisplaced",
                    "QuietFlapIsIdentity",
                    "EpochFlagsTheResidual",
                    "EpochNoFalseAlarm",
                    "OnlyAJudgedProgramIsAccused",
                    "DebtIsTheDisplacement",
                    "Bounded",
                ] {
                    assert!(
                        model.check_invariant(invariant, &real),
                        "{invariant}: {real:?}"
                    );
                }
                answers.holds.insert(real.clone());
            }
            *seen.entry(action).or_default() += 1;
            state = real;
        }
        Some(state)
    }

    /// What the walk of [`resize_render_conformance`] saw, summed over the
    /// schedules one thread replayed.
    #[derive(Default)]
    struct Tally {
        seen: BTreeMap<&'static str, usize>,
        verdicts: [usize; 3],
        shown: [usize; 2],
        epoch_words: [usize; 3],
        blind: usize,
        late: usize,
        max_disp: i64,
        schedules: usize,
    }

    impl Tally {
        fn add(&mut self, other: Self) {
            for (action, n) in other.seen {
                *self.seen.entry(action).or_default() += n;
            }
            for (a, b) in self.verdicts.iter_mut().zip(other.verdicts) {
                *a += b;
            }
            for (a, b) in self.shown.iter_mut().zip(other.shown) {
                *a += b;
            }
            for (a, b) in self.epoch_words.iter_mut().zip(other.epoch_words) {
                *a += b;
            }
            self.blind += other.blind;
            self.late += other.late;
            self.max_disp = self.max_disp.max(other.max_disp);
            self.schedules += other.schedules;
        }
    }

    fn extend(answers: &mut Answers<'_>, prefix: &[Step], t: &mut Tally) -> Vec<Vec<Step>> {
        let mut inside = Vec::new();
        for step in STEPS {
            if prefix.last() == Some(&step) {
                continue;
            }
            let mut schedule = prefix.to_vec();
            schedule.push(step);
            if !valid_geometry(&schedule) {
                continue;
            }
            t.schedules += 1;
            if let Some(end) = conform_answered(answers, &schedule, &mut t.seen) {
                if end["evaluated"] == 1 {
                    let v = usize::try_from(end["verdict"]).expect("0..=2");
                    t.verdicts[v] += 1;
                    t.shown[usize::from(end["shown"] == 1)] += 1;
                    t.epoch_words[usize::try_from(end["ev"]).expect("0..=2")] += 1;
                }
                t.blind += usize::from(end["blind"] == 1);
                t.late += usize::from(end["late"] == 1);
                t.max_disp = t.max_disp.max(end["disp"]);
                inside.push(schedule);
            }
        }
        inside
    }

    #[test]
    fn resize_render_conformance() {
        let model = resize_render_model();
        let mut tally = Tally::default();
        let mut answers = Answers::new(&model);
        let roots: Vec<Vec<Step>> = extend(&mut answers, &[], &mut tally)
            .iter()
            .flat_map(|one| extend(&mut answers, one, &mut tally))
            .collect();
        let next = std::sync::atomic::AtomicUsize::new(0);
        let threads = std::thread::available_parallelism().map_or(1, usize::from);
        std::thread::scope(|scope| {
            let workers: Vec<_> = (0..threads)
                .map(|_| {
                    let (model, roots, next) = (&model, &roots, &next);
                    scope.spawn(move || {
                        let mut t = Tally::default();
                        let mut answers = Answers::new(model);
                        while let Some(root) =
                            roots.get(next.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
                        {
                            let mut stack = vec![root.clone()];
                            while let Some(prefix) = stack.pop() {
                                if prefix.len() < 6 {
                                    stack.extend(extend(&mut answers, &prefix, &mut t));
                                }
                            }
                        }
                        t
                    })
                })
                .collect();
            for worker in workers {
                tally.add(
                    worker
                        .join()
                        .unwrap_or_else(|panic| std::panic::resume_unwind(panic)),
                );
            }
        });
        let Tally {
            seen,
            verdicts,
            shown,
            epoch_words,
            blind,
            late,
            max_disp,
            schedules,
        } = tally;
        for action in [
            "Demote",
            "Trim",
            "Grow",
            "Step",
            "Draw",
            "Output",
            "Classify",
            "Evaluate",
            "Grow joins a late run",
        ] {
            assert!(
                seen.get(action).is_some_and(|n| *n > 0),
                "{action} never driven: {seen:?}"
            );
        }
        assert!(
            verdicts.iter().all(|n| *n > 0),
            "ok, desync-risk and unverified all reached: {verdicts:?}"
        );
        assert!(
            shown[0] > 0 && shown[1] > 0,
            "render=displaced said and not said: {shown:?}"
        );
        assert!(
            epoch_words.iter().all(|n| *n > 0),
            "the epoch judge says ok, desync-risk and unjudged: {epoch_words:?}"
        );
        assert!(blind > 0, "the blind spot is reached, and named");
        assert!(late > 0, "a silent run outlives the run gap");
        assert_eq!(max_disp, CAP, "the displacement box is reached");
        assert!(schedules > 1_000, "{schedules} schedules");
    }

    #[test]
    fn a_run_brought_back_to_zero_keeps_the_draw_it_saw() {
        let model = resize_render_model();
        let mut seen = BTreeMap::new();
        let schedule = [
            Step::Shrink,
            Step::Output,
            Step::Grow,
            Step::Draw,
            Step::Shrink,
            Step::Evaluate,
            Step::Grow,
            Step::Draw,
            Step::Shrink,
        ];
        let end = conform(&model, &schedule, &mut seen).expect("inside the box");
        assert_eq!((end["ro"], end["rd"], end["rw"]), (1, 0, 1), "{end:?}");
    }

    fn valid_geometry(schedule: &[Step]) -> bool {
        let mut shrunk = false;
        for step in schedule {
            match step {
                Step::Shrink if !shrunk => shrunk = true,
                Step::Grow if shrunk => shrunk = false,
                Step::Shrink | Step::Grow => return false,
                _ => {}
            }
        }
        true
    }

    #[test]
    fn a_flap_split_by_a_frame_or_by_time_is_judged_on_the_real_engine() {
        let model = resize_render_model();
        let mut seen = BTreeMap::new();
        let tick = [
            Step::Shrink,
            Step::Draw,
            Step::Grow,
            Step::Handler,
            Step::Evaluate,
        ];
        let end = conform(&model, &tick, &mut seen).expect("inside the box");
        assert_eq!(
            (end["disp"], end["drew"], end["blind"], end["verdict"]),
            (1, 1, 0, 1)
        );
        let mut rig = Rig::new();
        for step in tick {
            rig.drive(step);
        }
        let reply = rig.ledger.reply(0, None);
        assert!(
            reply.contains(" flaps=1 risks=1 render=desync-risk "),
            "{reply}"
        );
        assert!(
            reply.contains("appended=1 restored=0 shift=+0"),
            "the grow appended: {reply}"
        );
        assert!(
            reply.contains(
                "n=2 alt=1 geom=20x8>20x7>20x8 net=zero displaced=-1 verdict=desync-risk"
            ),
            "one net-zero run: {reply}"
        );
        let [shrink, grow] = [&rig.ledger.rows[0].report, &rig.ledger.rows[1].report];
        assert!(!grow.adjoins(shrink), "the frame landed between the halves");
        let old = interp::with_buggy(&model, 5);
        let mut missed = old.init_state();
        for action in ["Demote", "Draw", "Grow", "Step", "Evaluate"] {
            assert!(old.fire(action, &mut missed), "{action}");
        }
        assert_eq!((missed["disp"], missed["verdict"]), (1, 2));
        assert!(!old.check_invariant("RiskIsFlagged", &missed), "{missed:?}");

        let slow = [Step::Shrink, Step::Evaluate, Step::Grow, Step::Evaluate];
        let end = conform(&model, &slow, &mut seen).expect("inside the box");
        assert_eq!((end["disp"], end["shown"], end["verdict"]), (0, 0, 0));
        let mut rig = Rig::new();
        for step in slow {
            rig.drive(step);
        }
        let reply = rig.ledger.reply(0, None);
        assert!(reply.contains(" flaps=1 risks=0 render=ok "), "{reply}");
        assert!(
            reply.contains("revealed=1 appended=0 restored=1 shift=+1"),
            "{reply}"
        );
        assert!(
            reply.contains("net=zero displaced=+0 verdict=none"),
            "{reply}"
        );
        let [shrink, grow] = [&rig.ledger.rows[0], &rig.ledger.rows[1]];
        assert!(
            grow.t_ms - shrink.t_ms > RUN_GAP_MS,
            "the halves are further apart than the run gap"
        );
        let timed_out = interp::with_buggy(&model, 3);
        let mut stuck = timed_out.init_state();
        for action in ["Demote", "Evaluate", "Grow", "Evaluate"] {
            assert!(timed_out.fire(action, &mut stuck), "{action}");
        }
        assert!(
            !timed_out.check_invariant("NoFalseDisplaced", &stuck),
            "{stuck:?}"
        );

        let twice = [
            Step::Shrink,
            Step::Evaluate,
            Step::Grow,
            Step::Shrink,
            Step::Draw,
            Step::Grow,
            Step::Handler,
            Step::Evaluate,
        ];
        let end = conform(&model, &twice, &mut seen).expect("inside the box");
        assert_eq!(
            (end["disp"], end["drew"], end["blind"], end["verdict"]),
            (1, 1, 0, 1)
        );
        let mut missed = old.init_state();
        for action in [
            "Demote", "Evaluate", "Grow", "Demote", "Draw", "Grow", "Step", "Evaluate",
        ] {
            assert!(old.fire(action, &mut missed), "{action}");
        }
        assert_eq!((missed["blind"], missed["verdict"]), (0, 2));
        assert!(!old.check_invariant("RiskIsFlagged", &missed), "{missed:?}");
    }

    #[test]
    fn the_t2649_flap_is_undone_and_the_historical_engine_is_caught() {
        let model = resize_render_model();
        let schedule = [Step::Shrink, Step::Grow, Step::Handler, Step::Evaluate];
        let mut seen = BTreeMap::new();
        let end = conform(&model, &schedule, &mut seen).expect("inside the box");
        assert_eq!(
            (end["disp"], end["drew"], end["verdict"]),
            (0, 0, 0),
            "the quiet flap is an identity"
        );
        let mut rig = Rig::new();
        for step in schedule {
            rig.drive(step);
        }
        let reply = rig.ledger.reply(0, None);
        assert!(reply.contains(" render=ok "), "{reply}");
        assert!(
            reply.contains("revealed=1 appended=0 restored=1 shift=+1"),
            "{reply}"
        );
        assert!(
            reply.contains("net=zero displaced=+0 verdict=none"),
            "{reply}"
        );

        let split = [
            Step::Shrink,
            Step::Output,
            Step::Grow,
            Step::Handler,
            Step::Evaluate,
        ];
        let end = conform(&model, &split, &mut seen).expect("inside the box");
        assert_eq!((end["disp"], end["drew"], end["verdict"]), (1, 1, 1));

        let mut rig = Rig::new();
        rig.historical = true;
        for step in schedule {
            rig.drive(step);
        }
        assert_eq!(rig.verdict, 1, "the real ledger flags the historical flap");
        assert_eq!(rig.displacement(), 1, "one row up");
        let size_judge = rig.term.rows() != rig.app.rows;
        assert!(!size_judge, "the size-difference judge answers ok");
        let reply = rig.ledger.reply(0, None);
        assert!(reply.contains(" render=desync-risk "), "{reply}");
        assert!(
            reply.contains("net=zero displaced=-1 verdict=desync-risk"),
            "{reply}"
        );

        let historical = interp::with_buggy(&model, 1);
        let mut state = historical.init_state();
        for action in ["Demote", "Grow"] {
            assert!(historical.fire(action, &mut state), "{action}");
        }
        assert!(
            !historical.check_invariant("QuietFlapIsIdentity", &state),
            "{state:?}"
        );
        for action in ["Step", "Evaluate"] {
            assert!(historical.fire(action, &mut state), "{action}");
        }
        assert!(
            !historical.check_invariant("RiskIsFlagged", &state),
            "{state:?}"
        );
    }

    #[test]
    fn the_assumed_repaint_answers_ok_on_a_tick_at_a_changed_size() {
        let model = resize_render_model();
        let schedule = [Step::Shrink, Step::Draw, Step::Evaluate];
        let mut seen = BTreeMap::new();
        let end = conform(&model, &schedule, &mut seen).expect("inside the box");
        assert_eq!(
            (end["disp"], end["drew"], end["br"], end["verdict"]),
            (1, 1, 1, 2)
        );
        let mut rig = Rig::new();
        for step in schedule {
            rig.drive(step);
        }
        assert_eq!(rig.displacement(), 1, "the screen is one row up");
        let reply = rig.ledger.reply(0, None);
        assert!(reply.contains(" render=unverified "), "{reply}");
        assert!(
            reply.contains("net=changed displaced=-1 verdict=unverified"),
            "{reply}"
        );

        let assumed = interp::with_buggy(&model, 2);
        let mut state = assumed.init_state();
        for action in ["Demote", "Draw", "Evaluate"] {
            assert!(assumed.fire(action, &mut state), "{action}");
        }
        assert_eq!(state["verdict"], 0, "the assumed repaint answers ok");
        assert!(
            !assumed.check_invariant("NoSilentPass", &state),
            "{state:?}"
        );
    }

    #[test]
    fn a_quiet_flap_only_time_split_is_one_run_credited_back() {
        let model = resize_render_model();
        let schedule = [
            Step::Shrink,
            Step::Evaluate,
            Step::Grow,
            Step::Draw,
            Step::Evaluate,
        ];
        let mut seen = BTreeMap::new();
        let end = conform(&model, &schedule, &mut seen).expect("inside the box");
        assert_eq!(
            (end["disp"], end["drew"], end["blind"], end["verdict"]),
            (0, 0, 0, 0)
        );
        assert_eq!(seen.get("Grow joins a late run"), Some(&1));
        let mut rig = Rig::new();
        for step in schedule {
            rig.drive(step);
        }
        assert_eq!(rig.displacement(), 0, "the screen is the app's");
        assert_eq!(rig.ledger.runs[0].held, (0, 0), "the row was credited");
        let reply = rig.ledger.reply(0, None);
        assert!(reply.contains(" render=ok "), "{reply}");
        assert!(
            reply.contains("revealed=1 appended=0 restored=1 shift=+1"),
            "the grow's row still says what the engine did: {reply}"
        );
        assert!(
            reply.contains("run 1 ") && !reply.contains("run 2 "),
            "the late grow joined the shrink's run: {reply}"
        );
        assert!(
            reply.contains("net=zero displaced=+0 verdict=none"),
            "the row went back to the shrink's run: {reply}"
        );

        let dropped = [
            Step::Shrink,
            Step::Evaluate,
            Step::Output,
            Step::Grow,
            Step::Draw,
            Step::Evaluate,
        ];
        let end = conform(&model, &dropped, &mut seen).expect("inside the box");
        assert_eq!((end["disp"], end["drew"], end["verdict"]), (1, 1, 1));
        let mut rig = Rig::new();
        for step in dropped {
            rig.drive(step);
        }
        let reply = rig.ledger.reply(0, None);
        assert!(reply.contains(" render=desync-risk "), "{reply}");
        assert!(
            reply.contains("net=zero displaced=-1 verdict=desync-risk"),
            "{reply}"
        );

        let timed_out = interp::with_buggy(&model, 3);
        let mut state = timed_out.init_state();
        for action in ["Demote", "Evaluate", "Grow", "Draw", "Evaluate"] {
            assert!(timed_out.fire(action, &mut state), "{action}");
        }
        assert_eq!((state["disp"], state["verdict"]), (0, 2));
        assert!(
            !timed_out.check_invariant("NoFalseVerdict", &state),
            "{state:?}"
        );
    }

    fn owing(ordinal: u64, content_seq: u64, shift: i32, geom: (u16, u16)) -> EngineProbe {
        EngineProbe {
            debt: aterm_core::terminal::RenderDebt::of_rows(shift, 0),
            geom,
            ..probe(ordinal, content_seq, 0)
        }
    }

    const LATE_GROW: u64 = RUN_GAP_MS + 50;

    fn split_flap(ledger: &mut ResizeLedger, at: u64) {
        let mut shrink = report(1, 24, 23, true, (10, 11));
        (shrink.demoted, shrink.stashed) = (1, 1);
        let mut grow = report(2, 23, 24, true, (15, 16));
        grow.appended = 1;
        ledger.ingest(
            &copy_of(&[shrink], owing(1, 11, -1, (23, 80))),
            Some(here()),
            at,
        );
        ledger.ingest(
            &copy_of(&[shrink, grow], owing(2, 16, -1, (24, 80))),
            Some(here()),
            at + LATE_GROW,
        );
    }

    fn epoch_lines(ledger: &ResizeLedger) -> Vec<String> {
        ledger
            .reply(0, None)
            .lines()
            .filter(|l| l.starts_with("epoch "))
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn a_flap_an_app_frame_splits_is_judged_as_one_epoch() {
        let mut ledger = ResizeLedger::default();
        ledger.set_agent(true);
        split_flap(&mut ledger, 1_000);
        assert_eq!(
            ledger.render(),
            Render::Displaced,
            "the risk wait is not over"
        );
        let out = ledger.evaluate(
            owing(2, 16, -1, (24, 80)),
            1_000 + LATE_GROW + RISK_AFTER_MS + 5,
        );
        assert_eq!(ledger.render(), Render::DesyncRisk);
        let risks: Vec<_> = out
            .iter()
            .filter(|t| matches!(t, Transition::EpochRisk { .. }))
            .collect();
        assert_eq!(risks.len(), 1, "{out:?}");
        assert!(matches!(
            risks[0],
            Transition::EpochRisk {
                epoch: 1,
                shift: -1,
                pushed: 0,
                drew: true,
                ..
            }
        ));
        assert_eq!(
            risks[0].payload(),
            "desync-risk epoch=1 displaced=-1 pushed=0 cut=0 drew=1 class=agent at=1000"
        );
        let again = ledger.evaluate(
            owing(2, 16, -1, (24, 80)),
            1_000 + LATE_GROW + RISK_AFTER_MS + 50,
        );
        assert!(again.is_empty(), "{again:?}");
        let lines = epoch_lines(&ledger);
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert_eq!(
            lines[0],
            "epoch 1 t=1000 geom=80x24>80x24 shift=-1 pushed=0 cut=0 drew=1 class=agent \
             verdict=desync-risk by=- tried=0"
        );
        assert!(
            ledger.reply(0, None).contains(" render=desync-risk "),
            "{}",
            ledger.reply(0, None)
        );
        let logs = ledger.take_logs();
        assert_eq!(logs.len(), 1, "{logs:?}");
        assert!(logs[0].contains("desync-risk") && logs[0].contains("class=agent"));
        assert!(ledger.take_logs().is_empty());
    }

    #[test]
    fn an_undrawn_residual_is_judged_after_the_risk_wait() {
        let mut ledger = ResizeLedger::default();
        ledger.set_agent(true);
        let mut shrink = report(1, 24, 23, true, (10, 11));
        (shrink.demoted, shrink.stashed) = (1, 1);
        let mut grow = report(2, 23, 24, true, (11, 12));
        grow.appended = 1;
        ledger.ingest(
            &copy_of(&[shrink, grow], owing(2, 12, -1, (24, 80))),
            Some(here()),
            1_000,
        );
        assert_eq!(ledger.render(), Render::Displaced);
        assert_eq!(ledger.look_at_ms(), Some(1_000 + RISK_AFTER_MS));
        ledger.evaluate(owing(2, 12, -1, (24, 80)), 1_000 + RISK_AFTER_MS);
        assert_eq!(ledger.render(), Render::DesyncRisk);
        assert!(epoch_lines(&ledger)[0].contains("drew=0"));
        assert_eq!(ledger.look_at_ms(), None, "judged: nothing more is owed");
    }

    #[test]
    fn an_epoch_whose_geometry_is_not_back_is_only_displaced() {
        let mut ledger = ResizeLedger::default();
        ledger.set_agent(true);
        let mut shrink = report(1, 24, 23, true, (10, 11));
        (shrink.demoted, shrink.stashed) = (1, 1);
        ledger.ingest(
            &copy_of(&[shrink], owing(1, 11, -1, (23, 80))),
            Some(here()),
            1_000,
        );
        ledger.evaluate(owing(1, 11, -1, (23, 80)), 1_000 + 10 * RISK_AFTER_MS);
        assert_eq!(ledger.render(), Render::Displaced);
        assert_eq!(ledger.look_at_ms(), None, "the next resize re-registers it");
    }

    #[test]
    fn an_epoch_closes_when_the_engine_debt_returns_to_zero() {
        let mut ledger = ResizeLedger::default();
        ledger.set_agent(true);
        let mut shrink = report(1, 24, 23, true, (10, 11));
        (shrink.demoted, shrink.stashed) = (1, 1);
        ledger.ingest(
            &copy_of(&[shrink], owing(1, 11, -1, (23, 80))),
            Some(here()),
            1_000,
        );
        assert_eq!(ledger.epochs().current().map(|e| e.id), Some(1));
        ledger.evaluate(owing(1, 11, 0, (23, 80)), 1_005);
        assert!(ledger.epochs().current().is_none());
        let lines = epoch_lines(&ledger);
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].contains("verdict=restored by=undo"), "{lines:?}");
        assert_eq!(ledger.epochs().state(), None, "the epoch owes nothing now");

        let mut quiet = ResizeLedger::default();
        let [shrink, mut grow] = alt_flap(1, 10);
        (grow.revealed, grow.restored_top, grow.appended) = (1, 1, 0);
        quiet.ingest(
            &copy_of(&[shrink, grow], probe(2, 12, 0)),
            Some(here()),
            1_000,
        );
        assert!(epoch_lines(&quiet).is_empty(), "a quiet flap owes no epoch");
        assert_eq!(quiet.look_at_ms(), None, "an idle machine arms no deadline");
    }

    #[test]
    fn a_journal_overrun_cannot_hide_a_displacement() {
        let mut ledger = ResizeLedger::default();
        ledger.set_agent(true);
        let mut shrink = report(40, 24, 23, true, (500, 501));
        (shrink.demoted, shrink.stashed) = (1, 0);
        let mut grow = report(41, 23, 24, true, (501, 502));
        grow.appended = 1;
        ledger.ingest(
            &copy_of(&[shrink, grow], owing(41, 502, -20, (24, 80))),
            Some(here()),
            1_000,
        );
        assert!(ledger.lost > 0, "the journal overran");
        ledger.evaluate(owing(41, 502, -20, (24, 80)), 1_000 + RISK_AFTER_MS);
        assert_eq!(ledger.render(), Render::DesyncRisk);
        assert!(epoch_lines(&ledger)[0].contains("shift=-20"));
    }

    #[test]
    fn the_transition_names_who_healed_it() {
        let mut ledger = ResizeLedger::default();
        ledger.set_agent(true);
        split_flap(&mut ledger, 1_000);
        ledger.evaluate(
            owing(2, 16, -1, (24, 80)),
            1_000 + LATE_GROW + RISK_AFTER_MS + 5,
        );
        assert_eq!(ledger.render(), Render::DesyncRisk);
        let healed = EngineProbe {
            full_clears: 1,
            ..owing(2, 17, 0, (24, 80))
        };
        let out = ledger.evaluate(healed, 1_000 + LATE_GROW + RISK_AFTER_MS + 300);
        let epoch_healed: Vec<_> = out
            .iter()
            .filter(|t| matches!(t, Transition::EpochHealed { .. }))
            .collect();
        assert_eq!(epoch_healed.len(), 1, "{out:?}");
        assert_eq!(
            epoch_healed[0].payload(),
            "healed epoch=1 after_ms=1300 by=app"
        );
        assert_eq!(ledger.render(), Render::Ok);
        assert!(epoch_lines(&ledger)[0].contains("verdict=healed by=app"));
    }

    #[test]
    fn the_nudge_acts_only_on_an_untried_agent_epoch_at_desync_risk() {
        let mut ledger = ResizeLedger::default();
        ledger.set_agent(true);
        split_flap(&mut ledger, 1_000);
        assert_eq!(ledger.nudge_view(), None, "the risk wait is not over");
        ledger.evaluate(
            owing(2, 16, -1, (24, 80)),
            1_000 + LATE_GROW + RISK_AFTER_MS + 5,
        );
        let view = ledger.nudge_view().expect("agent epoch at risk");
        assert_eq!(
            (view.epoch, view.shift, view.pushed, view.drew),
            (1, -1, 0, true)
        );
        assert!(ledger.agent_class());
        ledger.note_tried(view.epoch);
        assert_eq!(ledger.nudge_view(), None);
        assert!(epoch_lines(&ledger)[0].contains("tried=1"));
        let mut other = ResizeLedger::default();
        let mut shrink = report(1, 24, 23, true, (10, 11));
        (shrink.demoted, shrink.stashed) = (1, 1);
        let mut grow = report(2, 23, 24, true, (11, 12));
        grow.appended = 1;
        other.ingest(
            &copy_of(&[shrink, grow], owing(2, 12, -1, (24, 80))),
            Some(here()),
            1_000,
        );
        other.evaluate(
            owing(2, 12, -1, (24, 80)),
            1_000 + LATE_GROW + RISK_AFTER_MS,
        );
        assert_eq!(other.render(), Render::Unjudged);
        assert_eq!(other.nudge_view(), None);
        assert!(!other.agent_class());
    }

    #[test]
    fn an_epoch_the_nudge_repainted_closes_by_nudge() {
        let mut ledger = ResizeLedger::default();
        ledger.set_agent(true);
        split_flap(&mut ledger, 1_000);
        ledger.evaluate(
            owing(2, 16, -1, (24, 80)),
            1_000 + LATE_GROW + RISK_AFTER_MS + 5,
        );
        ledger.note_tried(1);
        ledger.set_nudge("restoring", 1);
        assert!(
            ledger
                .reply(0, None)
                .contains(" nudge=restoring nudged=1\n")
        );
        let healed = EngineProbe {
            full_clears: 2,
            ..owing(2, 20, 0, (24, 80))
        };
        let out = ledger.evaluate(healed, 1_000 + LATE_GROW + RISK_AFTER_MS + 900);
        let payloads: Vec<String> = out.iter().map(Transition::payload).collect();
        assert_eq!(payloads, ["healed epoch=1 after_ms=1900 by=nudge"]);
        assert!(epoch_lines(&ledger)[0].contains("verdict=healed by=nudge tried=1"));
        assert_eq!(ledger.render(), Render::Ok);
    }

    #[test]
    fn an_unknown_program_reads_unjudged_and_pushes_no_event() {
        let mut ledger = ResizeLedger::default();
        let mut shrink = report(1, 24, 23, true, (10, 11));
        (shrink.demoted, shrink.stashed) = (1, 1);
        let mut grow = report(2, 23, 24, true, (11, 12));
        grow.appended = 1;
        ledger.ingest(
            &copy_of(&[shrink, grow], owing(2, 12, -1, (24, 80))),
            Some(here()),
            1_000,
        );
        let out = ledger.evaluate(owing(2, 12, -1, (24, 80)), 1_000 + RISK_AFTER_MS);
        assert_eq!(ledger.render(), Render::Unjudged);
        assert!(out.is_empty(), "unjudged is never pushed: {out:?}");
        assert!(epoch_lines(&ledger)[0].contains("class=unknown verdict=unjudged"));
        assert!(ledger.reply(0, None).contains(" render=unjudged "));
        let logs = ledger.take_logs();
        assert_eq!(logs.len(), 1);
        assert!(logs[0].contains("unjudged"), "{logs:?}");
    }

    #[test]
    fn a_program_that_cleared_on_resize_is_learned() {
        let mut ledger = ResizeLedger::default();
        let mut shrink = report(1, 24, 23, true, (10, 11));
        (shrink.demoted, shrink.stashed) = (1, 1);
        ledger.ingest(
            &copy_of(&[shrink], owing(1, 11, -1, (23, 80))),
            Some(here()),
            1_000,
        );
        let cleared = EngineProbe {
            full_clears: 1,
            ..owing(1, 12, 0, (23, 80))
        };
        ledger.evaluate(cleared, 1_040);
        assert_eq!(run_verdict(&ledger, 1), Verdict::Healed);
        let mut shrink = report(2, 23, 22, true, (12, 13));
        (shrink.demoted, shrink.stashed) = (1, 1);
        shrink.full_clears = 1;
        let mut grow = report(3, 22, 23, true, (13, 14));
        grow.appended = 1;
        grow.full_clears = 1;
        let probe_at = |ord, seq| EngineProbe {
            full_clears: 1,
            ..owing(ord, seq, -1, (23, 80))
        };
        ledger.ingest(
            &copy_of(&[shrink, grow], probe_at(3, 14)),
            Some(here()),
            5_000,
        );
        ledger.evaluate(probe_at(3, 14), 5_000 + RISK_AFTER_MS);
        assert_eq!(ledger.render(), Render::DesyncRisk);
        assert!(
            epoch_lines(&ledger)
                .last()
                .unwrap()
                .contains("class=learned")
        );
    }

    #[test]
    fn a_contended_terminal_is_asked_again_after_100ms() {
        let mut ledger = ResizeLedger::default();
        ledger.set_agent(true);
        let mut grow = report(2, 23, 24, true, (11, 12));
        grow.appended = 1;
        let mut shrink = report(1, 24, 23, true, (10, 11));
        (shrink.demoted, shrink.stashed) = (1, 1);
        ledger.ingest(
            &copy_of(&[shrink, grow], owing(2, 12, -1, (24, 80))),
            Some(here()),
            1_000,
        );
        assert_eq!(ledger.look_at_ms(), Some(2_000));
        assert!(!ledger.look_due(1_999));
        assert!(ledger.look_due(2_000));
        ledger.defer_look(2_000);
        assert_eq!(ledger.look_at_ms(), Some(2_100));
        for _ in 1..20 {
            ledger.defer_look(2_100);
        }
        assert_eq!(ledger.look_at_ms(), Some(2_100 + 100), "the 20th retry");
        ledger.defer_look(3_000);
        assert_eq!(
            ledger.look_at_ms(),
            Some(3_000 + RISK_AFTER_MS),
            "then at the next risk wait: no spin"
        );
    }

    #[test]
    fn a_restored_engine_seeded_with_debt_owes_an_epoch() {
        let mut ledger = ResizeLedger::default();
        ledger.set_agent(true);
        ledger.evaluate(owing(0, 5, -1, (24, 80)), 10_000);
        assert_eq!(ledger.render(), Render::Displaced);
        ledger.evaluate(owing(0, 5, -1, (24, 80)), 10_000 + RISK_AFTER_MS);
        assert_eq!(ledger.render(), Render::DesyncRisk);
    }

    #[test]
    fn a_column_flap_that_cut_cells_is_judged_until_a_repaint() {
        let mut term = Terminal::new(ROWS, COLS);
        term.process(b"\x1b[?1049h\x1b[2J");
        let rule = "\u{2500}".repeat(usize::from(COLS));
        for r in [ROWS - 3, ROWS - 1] {
            term.process(format!("\x1b[{};1H{rule}", r + 1).as_bytes());
        }
        term.process(format!("\x1b[{};1H> ", ROWS - 1).as_bytes());
        let mut ledger = ResizeLedger::default();
        ledger.set_agent(true);
        term.resize(ROWS, COLS - 1);
        ledger.ingest(&JournalCopy::take(&term), Some(here()), 1_000);
        term.process(b"x");
        term.resize(ROWS, COLS);
        ledger.ingest(&JournalCopy::take(&term), Some(here()), 1_035);
        let cells = |term: &Terminal, r: u16| {
            term.row_text(usize::from(r))
                .unwrap_or_default()
                .trim_end()
                .chars()
                .count()
        };
        assert_eq!(
            cells(&term, ROWS - 1),
            usize::from(COLS) - 1,
            "the rule lost its last cell"
        );

        ledger.evaluate(EngineProbe::sample(&term), 1_035 + RISK_AFTER_MS + 5);
        let reply = ledger.reply(0, None);
        assert_eq!(ledger.render(), Render::DesyncRisk, "{reply}");
        assert!(reply.contains(" render=desync-risk "), "{reply}");
        let lines = epoch_lines(&ledger);
        assert_eq!(lines.len(), 1, "{reply}");
        assert!(
            lines[0].contains(" shift=+0 pushed=0 cut=1 ")
                && lines[0].contains(" verdict=desync-risk "),
            "{reply}"
        );
        assert!(ledger.nudge_view().is_some(), "the nudge is owed");

        term.process(b"\x1b[2J");
        ledger.evaluate(EngineProbe::sample(&term), 1_035 + RISK_AFTER_MS + 50);
        assert_eq!(ledger.render(), Render::Ok, "{}", ledger.reply(0, None));
        assert!(ledger.nudge_view().is_none());
    }

    #[test]
    fn a_session_that_owes_nothing_has_no_epoch() {
        let mut ledger = ResizeLedger::default();
        ledger.set_agent(true);
        split_flap(&mut ledger, 1_000);
        let mut conpty = ResizeLedger::default();
        conpty.set_agent(true);
        let mut shrink = report(1, 24, 23, true, (10, 11));
        shrink.demoted = 1;
        let mut grow = report(2, 23, 24, true, (15, 16));
        grow.appended = 1;
        conpty.ingest(
            &copy_of(&[shrink, grow], probe(2, 16, 0)),
            Some(here()),
            1_000,
        );
        conpty.evaluate(probe(2, 16, 0), 1_000 + LATE_GROW + RISK_AFTER_MS);
        assert!(epoch_lines(&conpty).is_empty());
        assert_ne!(conpty.render(), Render::DesyncRisk);
    }
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
