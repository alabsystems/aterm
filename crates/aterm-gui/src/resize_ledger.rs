// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE RESIZE LEDGER: every grid resize a session took, who asked for it, the
//! runs they form, and whether the application has drawn over content a
//! resize displaced (`resizes`, `status render=`, `EVENT <local> resize`,
//! `EVENT <local> render desync-risk|healed`).
//!
//! WHY IT EXISTS (measured 2026-09-28 against a live Claude Code session). The
//! window re-gridded 64→63→64 rows within a millisecond about once a minute.
//! On the alternate screen the shrink DEMOTES the top row (the alt grid keeps
//! no history, so it is dropped) and the grow appends a blank row, so a
//! net-zero flap leaves every row one higher than the app painted it. Claude
//! Code repaints (ED 2) only when its SIGWINCH handler reads a CHANGED size;
//! once in 52 flaps both resizes landed before the handler ran, it read
//! 64 == 64, skipped the repaint, and its diff frames landed on shifted
//! content for good. The geometry is unchanged across such a flap and a
//! per-read watermark misses it, so no surface could say it happened. The
//! engine now journals what each resize did to the rows
//! (`aterm_core::terminal::ResizeReport`); this ledger reads that journal.
//! The engine also UNDOES such a flap when no output landed between its
//! halves (the grid's resize undo hands the demoted row back, counted in
//! `revealed` and said as `restored=`), so a quiet flap nets to zero here and
//! reads `none`; what the judge below still sees is a flap that output split
//! (a mode set, a query, a spinner frame), which dropped the undo but not the
//! run.
//!
//! RUNS. A resize joins the newest run (one not yet settled: `none`,
//! `reflow` or `silent`) on the same screen, with no full clear and no screen
//! switch since the previous resize, when
//!
//! * it came at most [`RUN_GAP_MS`] after the previous one and nothing
//!   changed the screen between them (`ResizeReport::adjoins`): the proxy for
//!   "the app's SIGWINCH handler could only see the final size";
//! * it came at most [`RUN_GAP_MS`] after the previous one, the run is
//!   `silent` (it displaced rows) and this resize takes the previous one back
//!   (its `to` is the previous `from`: a flap's second half), whatever the
//!   app drew between them: a draw is no evidence that the handler read the
//!   intermediate size (a spinner tick is drawn by a frame laid out for the
//!   size the app last read), and a handler that did read it and repaints
//!   from scratch shows as the clear that splits the run. A draw closes the
//!   run to any other resize — to one that moved nothing (the app drew on an
//!   undisplaced screen, a fresh baseline), and to one going on to a third
//!   size, so a drag the app follows frame by frame is not one long run; or
//! * the run is `silent` and never drawn over, and nothing changed the screen
//!   since the previous resize, however late: the displacement is still the
//!   run's, and a grow the engine's resize undo answers (`restored=`) puts it
//!   back, so the pair nets to zero rather than leaving two `silent` halves.
//!
//! The gap is measured from the previous row's booking time only when the
//! path that ran it booked it: a backfilled row (`site=-`) is stamped when a
//! reader found it, so after one only the first and last cases join.
//!
//! A run is NET-ZERO when its geometry ends where it began; `flaps=` counts
//! the net-zero runs of two or more resizes. Rows a grow hands back from the
//! engine's resize undo (`restored=`) go back to the run whose shrink stashed
//! them, newest first as the undo pops them ([`ResizeLedger::credit`]): the
//! run the grow joins nets them in its own shift, and a run the grow did NOT
//! join (a newer run opened between, or a run the grouping above closed while
//! the undo still held its rows) is credited with them and the grow's own run
//! does not count them, so a quiet flap moves nothing here however its halves
//! were grouped.
//!
//! THE JUDGE. A run is judged only on the alternate screen, where apps address
//! rows absolutely and nothing can move a demoted row back; the primary
//! screen's content is history-backed and moves with the cursor (a rewrap
//! there is terminal-owned: `reflow`). A judged run is one that moved content
//! (`displaced`) or pushed rows off the bottom (`pushed`). It is:
//!
//! * `silent` while the app has not drawn since, or drew less than
//!   [`RISK_AFTER_MS`] ago (its changed-size repaint may still be arriving);
//! * `desync-risk`, for a NET-ZERO run only, once the app has drawn (after
//!   the grow back, or between the halves: a frame on displaced content) and
//!   [`RISK_AFTER_MS`] passed with no heal: its handler, whenever it ran, read
//!   an unchanged size, and it drew over the displaced content without
//!   clearing it — t=2649, and the presence row's attention flap whose halves
//!   a spinner tick split (2026-09-28: `appended=1 restored=0`, the top row
//!   lost, which the ledger then split into two `net=changed` runs);
//! * `unverified`, for a run whose geometry stayed CHANGED, once the app has
//!   drawn and [`RISK_AFTER_MS`] passed with no heal: its handler read the
//!   change and most apps repaint with a clear (which heals it), but a repaint
//!   that does not clear is invisible to every counter here, and so is a
//!   spinner tick drawn on the moved rows before the handler ran. The ledger
//!   does not vouch either way; `cast drift` replays the recording and
//!   decides: a size that later came back, however long after, it weighs as
//!   a flap the app may have missed (a RETURN), and one that stayed changed
//!   is the app's to repaint. (Until 2026-09-28 this was `repainted`, the
//!   draw TAKEN as the repaint: a single net-changed resize the app never
//!   repainted read as fine. No engine counter can prove a non-clearing
//!   repaint — a row written since the resize may be one cell of it — so the
//!   verdict says what is known.) A later heal still moves it to `healed`;
//! * `healed` once the engine's full-clear or screen-replacement count moved
//!   past the run's (ED 2, a home ED 0, an alt-screen switch, RIS);
//! * `none` for a run that moved nothing (a trim), `reflow` for a primary
//!   width change.
//!
//! THE RULE, in the journal's fields. A shrink that displaced alt-screen rows
//! (`demoted`, `pushed`) and the grow back to its starting geometry, in one
//! run, sum to a net displacement (`shift()`, `net_pushed()`: a grow the
//! resize undo answered carries it back in `restored=`, one after output
//! only appends). The pair is a risk when that sum is nonzero, whatever
//! output (`content_seq`) landed between the halves, unless the app repainted
//! the screen from scratch: a `full_clears`/`screen_replaced` advance between
//! the halves splits the run (the shrink's run is `healed`, the grow's moved
//! nothing), and one after the grow heals the joined run. Fully restored, the
//! sum is zero: `none`, and `render=ok`.
//!
//! The verdict is a RISK, not a proof. Its blind spots, stated: a flap whose
//! halves more than [`RUN_GAP_MS`] separate, with the app drawing between them,
//! is two net-changed runs, never `desync-risk` (the app's handler had that
//! long to read the intermediate size) — the half that displaced reads
//! `unverified` once drawn on, and `cast drift` audits it (a RETURN, a lossy
//! shrink through the resize that brings the size back however long after,
//! holds the halves time split); a changed-size repaint that does not clear,
//! between the halves of a flap, still reads as a draw on displaced content (a
//! false `desync-risk`) — a window dragged down and straight back on the
//! alternate screen, the app redrawing each size without ED 2, is such a flap
//! at the turn; and a `desync-risk` (or an `unverified`) stays until a full
//! clear or a screen switch, even when the app repaints every row without
//! clearing. The
//! formal statement is `aterm_spec::derive::resize_render_model`
//! (`ResizeRender`); the Tier-1 bind to this code is
//! `tests::resize_render_conformance`.
//!
//! WHO WRITES IT. The host's resize paths copy the engine's journal INSIDE the
//! `term_lock` hold the resize already takes ([`JournalCopy::take`], at most
//! `RESIZE_JOURNAL_CAP` reports) and ingest it after releasing that lock, under
//! the session timeline's own leaf lock, with their attribution: the window
//! pass (`App::resize_panes_scoped_with_active_plan`, `site=` the entry point
//! it runs under, [`SiteScope`]) and the cross-session `resize`
//! (`site=ctl-cross`). Readers ([`EngineProbe`], five integers sampled under a
//! hold they already take) evaluate. Two threads copy and book in either
//! order, so a copy or a probe can arrive LATE: every reading carries the
//! engine's lifetime token and resize ordinal, a copy older than what is
//! booked only hands over its attribution, and a probe is compared only with
//! the runs it is not older than (and with their content only when it saw
//! exactly what is booked). A resize no path recorded is found by the
//! SELF-AUDIT: an engine ordinal past the ledger's is backfilled from the
//! journal with `site=-` and counted (`unledgered=`); one that fell out of the
//! journal first is counted as `lost=`. No lock is taken under another here,
//! and every operation is bounded by the two rings.
//!
//! No clock is read here: the host stamps each row and passes `now_ms` (the
//! timeline clock, [`crate::turn_ledger::now_ms`]).

use std::cell::Cell;
use std::collections::VecDeque;
use std::fmt::Write as _;
use std::panic::Location;

use aterm_core::terminal::{ResizeReport, Terminal};

/// How many resize rows the ledger keeps (drop-oldest; a subscriber's gap is
/// said as `dropped=<n>` on its next `EVENT <local> resize`). A window drag
/// makes about 60 rows a second, so the ring holds the last couple of seconds
/// of a drag and hours of chrome flaps.
pub(crate) const LEDGER_ROWS_CAP: usize = 128;
/// How many runs the ledger keeps (drop-oldest).
pub(crate) const LEDGER_RUNS_CAP: usize = 64;
/// The widest gap between two resizes of one run.
pub(crate) const RUN_GAP_MS: u64 = 250;
/// How long after a run's last resize a draw is judged: time for a
/// changed-size repaint that starts with a diff to finish (Claude Code's
/// healthy repaint lands 5-30 ms after the flap).
pub(crate) const RISK_AFTER_MS: u64 = 1_000;
/// Geometries a run keeps for its `geom=` chain; past it the newest replaces
/// the last and the chain says `...`.
const GEOMS_KEPT: usize = 8;

/// Which kind of entry point asked for a window pass (`site=`): the OUTERMOST
/// one on the main thread's stack when the pass ran, a stable word an operator
/// can read on any build. Where in the source it was is `at=`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Cause {
    /// The input dispatch (`App::input_to_session`): an `InputEvent::Resize`,
    /// which is how the control `resize` verb reaches a window.
    Input,
    /// The OS window changed size (`App::on_resize`).
    Window,
    /// The window's chrome rows changed — the tab strip, the message band or
    /// the presence row appeared or folded (`regrid_window_for_chrome_rows`).
    Chrome,
    /// One window's sessions re-gridded directly (`App::apply_term_resize`
    /// under no other entry point).
    Term,
    /// A pane pass under no other entry point: a split, a zoom, a tab switch,
    /// a font or scale change, a drag's settle (`App::resize_panes`).
    Pass,
    /// A tab a drag deferred, sized as it is drawn (`redraw_window`).
    Redraw,
}

impl Cause {
    /// The wire word.
    const fn word(self) -> &'static str {
        match self {
            Self::Input => "input",
            Self::Window => "window",
            Self::Chrome => "chrome",
            Self::Term => "term",
            Self::Pass => "pass",
            Self::Redraw => "redraw",
        }
    }
}

/// Who asked for a resize.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Site {
    /// A window pass: its entry point's kind and the source line that called
    /// that entry point (`site=<cause> at=<file:line>`).
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
    /// The resize entry point this thread is inside, outermost first.
    static SCOPE: Cell<Option<Site>> = const { Cell::new(None) };
}

/// Marks a resize ENTRY POINT for the ledger's `site=`: a one-line shim in
/// front of an ordinary body (`on_resize`, `input_to_session`, …) enters the
/// scope with the source line that CALLED it (`#[track_caller]` on the shim
/// alone, so no panic or `term_lock` tripwire inside the body is re-pointed at
/// the caller), and the window pass reads [`SiteScope::current`]. The
/// OUTERMOST entry point wins — a chrome re-grid that runs `on_resize` is
/// `chrome` — and dropping the guard (a panic's unwind included) leaves the
/// scope. Main-thread state only, hence a thread-local rather than a field
/// every `App` constructor would carry.
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

/// Where a resize came from: its site, and the window's grid and chrome rows
/// when a window drove it.
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

/// The engine facts a verdict needs, read under a `term_lock` hold the caller
/// already takes: five integer loads.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct EngineProbe {
    /// The engine's lifetime token (`Terminal::resize_lifetime`): ordinals and
    /// counters compare only within one.
    pub(crate) lifetime: u64,
    /// The engine's newest resize ordinal (`Terminal::resize_ordinal`).
    pub(crate) ordinal: u64,
    /// The active grid's content sequence.
    pub(crate) content_seq: u64,
    /// `Terminal::full_clear_count`.
    pub(crate) full_clears: u64,
    /// `Terminal::screen_replaced_count`.
    pub(crate) screen_replaced: u64,
}

impl EngineProbe {
    /// Sample `term` (the caller holds its lock).
    pub(crate) fn sample(term: &Terminal) -> Self {
        Self {
            lifetime: term.resize_lifetime(),
            ordinal: term.resize_ordinal(),
            content_seq: term.content_seq(),
            full_clears: term.full_clear_count(),
            screen_replaced: term.screen_replaced_count(),
        }
    }
}

/// The engine's journal as a resize path copied it, inside the same
/// `term_lock` hold as the resize: at most `RESIZE_JOURNAL_CAP` reports and
/// the probe taken with them.
#[derive(Clone, Debug, Default)]
pub(crate) struct JournalCopy {
    reports: Vec<ResizeReport>,
    probe: EngineProbe,
}

impl JournalCopy {
    /// Copy every report the journal still holds (the caller holds `term`'s
    /// lock). Bounded by the journal's ring, whatever the ledger has read: the
    /// ledger skips what it booked and counts what it never saw.
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
    /// A run whose geometry stayed changed, drawn over since with no heal:
    /// the app's changed-size repaint or a diff on moved rows, which no
    /// counter here tells apart (`cast drift` does).
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

    /// Still waiting on the app: a heal or a draw moves it.
    const fn open(self) -> bool {
        matches!(self, Self::Silent | Self::DesyncRisk)
    }

    /// Still owed a verdict the engine can give: an open run, or an
    /// unverified one a later heal (a full clear, a screen switch) settles.
    /// Such a run is listed by every `resizes` reply and keeps `render=`.
    const fn watched(self) -> bool {
        matches!(self, Self::Silent | Self::DesyncRisk | Self::Unverified)
    }

    /// A run with this verdict may still take resizes.
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
    /// The app drew over content a net-changed run displaced, and nothing
    /// cleared it: repainted or not, no counter here can say (`cast drift`).
    Unverified,
}

impl Render {
    /// The wire token.
    pub(crate) const fn wire(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Displaced => "displaced",
            Self::DesyncRisk => "desync-risk",
            Self::Unverified => "unverified",
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
    /// `t_ms` is when the resize ran: the path that ran it booked it. A row
    /// backfilled from the journal (`site=-`) is stamped when a reader found
    /// it, possibly much later, so its `t_ms` is only an upper bound, and the
    /// run gap is not measured from it.
    pub(crate) timed: bool,
    /// What the engine did.
    pub(crate) report: ResizeReport,
    /// Who asked.
    pub(crate) attribution: Attribution,
    /// The run it belongs to.
    pub(crate) run: u64,
}

/// A maximal sequence of adjoining resizes (see the module note).
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
    /// that no grow has handed back yet. A grow that hands rows back credits
    /// them here ([`ResizeLedger::credit`]). Zeroed once the app draws, a heal
    /// lands or the engine is replaced (any of those dropped the undo), and
    /// when the journal dropped a report before the ledger read it (`lost=`):
    /// a stash it never saw breaks the newest-first pairing. Output that draws
    /// nothing (a mode set) also drops the undo but moves no counter here, so
    /// the count can outlive the rows; it is never credited then, because the
    /// engine hands back only what a newer, booked shrink stashed, and that
    /// run is credited first.
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
    /// when one first saw the heal (after the run's last resize). Upper
    /// bounds, at the readers' cadence.
    first_out_ms: Option<u64>,
    healed_ms: Option<u64>,
}

impl Run {
    /// A run of one resize. `credited` is `(top, bottom)`: the rows `report`
    /// handed back to OTHER runs ([`ResizeLedger::credit`]), which are theirs,
    /// not this run's shift.
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

    /// Fold one more adjoining resize in (`credited` as for [`Run::start`]).
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

    /// The verdict before any read: only a run that is still taking resizes
    /// is re-classified (an outcome, once read, stands).
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

    /// The alternate screen, no rewrap, and content moved or pushed off —
    /// moved EITHER way: a drawn net-zero run whose content moved down is a
    /// `desync-risk` as one moved up is (`ResizeRender`'s `lr` reads both; its
    /// `led` sums only the up-moved ones, which keeps its space bounded).
    fn judged(&self) -> bool {
        self.alt && !self.reflowed && (self.displaced != 0 || self.pushed > 0)
    }

    fn net_zero(&self) -> bool {
        self.start == self.end
    }

    /// A judged run that can turn `desync-risk`: its geometry ended where it
    /// began, so the app's handler read no change whenever it ran. A run whose
    /// geometry stayed changed is the app's to repaint.
    fn eligible(&self) -> bool {
        self.judged() && self.net_zero()
    }

    /// A geometry change and back, in one run.
    fn is_flap(&self) -> bool {
        self.n >= 2 && self.changed && self.net_zero()
    }

    /// Either heal counter advanced PAST the run's (both only grow, so an
    /// older reading never passes for a heal).
    fn healed_by(&self, full_clears: u64, screen_replaced: u64) -> bool {
        full_clears > self.full_clears || screen_replaced > self.screen_replaced
    }

    fn net(&self) -> &'static str {
        if self.net_zero() { "zero" } else { "changed" }
    }

    /// `geom=<CxR>><CxR>>...`, `...` standing for the elided middle.
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

/// A verdict transition the timeline records as `kind=render` (and the
/// `events` digest pushes as `EVENT <local> render …`). Only entry into and
/// exit from `desync-risk` are recorded, so healthy flaps never reach the
/// lifecycle ring.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Transition {
    /// `desync-risk run=<r> displaced=<+-k> net=<zero|changed> at=<ms>` (`at=`
    /// is the run's first resize, the `t=` its `resizes` row carries; only a
    /// net-zero run is judged, so `net=zero` today).
    Risk {
        run: u64,
        displaced: i32,
        net_zero: bool,
        at_ms: u64,
    },
    /// `healed run=<r> after_ms=<ms>` (after the run's last resize).
    Healed { run: u64, after_ms: u64 },
}

impl Transition {
    /// The timeline payload, which is also the pushed EVENT's tail.
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
        }
    }
}

/// The ledger: the rows ring, the runs ring and the lifetime counters. Lives on
/// [`crate::session_timeline::SessionTimeline`] (one per session, behind that
/// leaf lock).
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
}

impl ResizeLedger {
    /// Book `copy`'s reports past the ledger's watermark, then evaluate against
    /// its probe. `attribution` belongs to the resize the caller just ran: the
    /// journal's newest report. Every other new report is backfilled as
    /// `site=-` (`unledgered=`); a report already booked by a backfill takes
    /// `attribution` over, so the path that ran it keeps the credit.
    ///
    /// A copy OLDER than what is booked (same engine, lower ordinal: another
    /// thread copied later and booked first) only hands over its attribution —
    /// its reports are booked and its probe predates them. A copy of ANOTHER
    /// engine lifetime (a restored engine, ordinals restarting at 0) rebases
    /// the ledger onto it.
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
                // A lost report may have stashed rows no run holds, and the
                // undo pops newest first: the next grow's hand-back could then
                // be credited to an older run whose own rows are long gone.
                // Give every held count up. The cost is a run the undo did
                // make whole reading `unverified` once drawn on; never `none`
                // over rows really dropped.
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

    /// Follow `probe`'s engine. The same lifetime changes nothing; another one
    /// (a restored engine: ordinals, content and counters restart) rebases the
    /// open runs onto its readings, so the displacement a checkpoint carried
    /// over is still judged, against counters it can be compared with.
    fn enter_lifetime(&mut self, probe: &EngineProbe) {
        if self.lifetime == Some(probe.lifetime) {
            return;
        }
        if self.lifetime.is_some() {
            // A restored engine starts with no resize undo: nothing is held.
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

    /// Give a backfilled row the attribution its own path brought late.
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
        // What the screen showed just before this resize settles the open runs
        // first, then the resize's own re-layout is not the app drawing.
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

    /// Return the rows `report` handed back from the engine's resize undo
    /// (`restored_top`, `restored_bottom`) to the runs whose shrinks stashed
    /// them, newest first, as the undo pops them (LIFO), and say how many went
    /// to runs OTHER than `joining` (the run `report` is about to join, whose
    /// own shift already nets them). A run holding rows has drawn nothing
    /// since (any output drops the undo), so it is still `silent` or `none`;
    /// credited back to zero it moved nothing and reads `none`. This is what
    /// keeps a quiet flap an identity here when more than [`RUN_GAP_MS`]
    /// split its halves: time closed the shrink's run, the undo still held the
    /// row, and the grow's own run would otherwise count it as a shift down.
    /// The pairing needs every stash booked: after a `lost=` report nothing is
    /// held (see [`Run::held`]), and a hand-back then credits no run.
    #[cfg_attr(
        test,
        aterm_spec::refines(
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

    /// Settle the watched runs against one reading of the engine: a heal
    /// first (the screen was repainted from scratch), then a draw, then — once
    /// [`RISK_AFTER_MS`] passed — the risk (a net-zero run) or `unverified` (a
    /// run whose geometry stayed changed; only a later heal moves it). A run
    /// whose last resize came after the reading is left alone.
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

    /// Evaluate the open runs against `probe` (idempotent but for the
    /// time-driven verdicts) and return the transitions for the timeline.
    /// O(retained runs); nothing to do while no run is open. A probe of
    /// another engine lifetime settles nothing (the next copy rebases); one
    /// taken with a different resize count than is booked still sees a heal of
    /// the runs it is not older than, but not a draw.
    #[cfg_attr(
        test,
        aterm_spec::refines(
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
        let any = |verdict: Verdict| self.runs.iter().any(|r| r.verdict == verdict);
        self.render = if any(Verdict::DesyncRisk) {
            Render::DesyncRisk
        } else if any(Verdict::Unverified) {
            Render::Unverified
        } else if any(Verdict::Silent) {
            Render::Displaced
        } else {
            Render::Ok
        };
        out
    }

    /// The render state as of the last evaluation.
    #[cfg(test)]
    pub(crate) fn render(&self) -> Render {
        self.render
    }

    /// The newest row id (0 before the first): a subscriber's high-water mark.
    pub(crate) fn high_row(&self) -> u64 {
        self.high_row
    }

    /// The oldest retained row id.
    pub(crate) fn low_row(&self) -> Option<u64> {
        self.rows.front().map(|row| row.id)
    }

    /// The engine `probe` read has a resize this ledger has not booked (or is
    /// another engine): a reader that can take the terminal lock should copy
    /// the journal and backfill.
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

    /// `render=<ok|displaced|desync-risk|unverified|-> resizes=<n> flaps=<n>`, the `status`
    /// fields; `probe` is `None` when the terminal lock was contended, and the
    /// render then reads `-`.
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

    /// The `resizes` reply: the header, then the selected `resize` rows oldest
    /// first, then the `run` rows they belong to — and every retained run still
    /// `silent`, `desync-risk` or `unverified`, whatever the selection, so a
    /// narrow read never hides the run behind `render=` — oldest first. `n` keeps the newest `n`
    /// rows (0 = all retained); `since` keeps rows with a larger id.
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
        let mut out = format!(
            "OK {} total={} runs={} flaps={} risks={} render={} unledgered={} lost={}\n",
            rows.len() + runs.len(),
            self.high_row,
            self.high_run,
            self.flaps,
            self.risks,
            self.render.wire(),
            self.unledgered,
            self.lost,
        );
        for row in rows {
            wire_row(&mut out, row);
        }
        for run in runs {
            wire_run(&mut out, run);
        }
        out
    }
}

/// A resize joins `run`, which `prev` ended (see the module note, RUNS),
/// when it came at most [`RUN_GAP_MS`] after `prev` with nothing changed on
/// the screen between them, or with `run` `silent` and this resize taking
/// `prev` back (`to` is `prev`'s `from`: a flap's second half), whatever was
/// drawn between; or when
/// `run` is `silent` and never drawn over and nothing changed on the screen
/// since `prev`, however late. The gap is measured only from a `timed` row.
///
/// No heal is tested here: `push` settles the open runs against the reading
/// just before `report` first, and a full clear or a screen switch (every
/// alternate-screen switch counts one) since `prev` has already turned
/// `run` `healed`, which nothing joins.
#[cfg_attr(
    test,
    aterm_spec::spec_unmodeled(
        reason = "The 250 ms bound on a run is the environment assumption the ResizeRender \
                  model states and does not model: there every step but Evaluate (a reader a \
                  second later) lands inside the gap, and Evaluate closes a run unless it is \
                  silent and undrawn, which the Tier-1 rig reproduces."
    )
)]
fn joins_run(prev: &LedgerRow, report: &ResizeReport, now_ms: u64, run: &Run) -> bool {
    let silent = run.verdict == Verdict::Silent;
    let adjoins = report.adjoins(&prev.report);
    let in_gap = now_ms.saturating_sub(prev.t_ms) <= RUN_GAP_MS;
    let takes_back = silent && prev.timed && report.to == prev.report.from;
    (in_gap && (adjoins || takes_back)) || (silent && !run.drew && adjoins)
}

/// `report`'s row shift less the top rows it handed back to other runs
/// (`credited.0`), which move THEIR content back, not this run's.
fn own_shift(report: &ResizeReport, credited: (u32, u32)) -> i32 {
    report
        .shift()
        .saturating_sub(i32::try_from(credited.0).unwrap_or(i32::MAX))
}

/// `report`'s net push less the bottom rows it put back for other runs
/// (`credited.1`): those were pushed off by another run's shrink.
fn own_pushed(report: &ResizeReport, credited: (u32, u32)) -> i32 {
    report
        .net_pushed()
        .saturating_add(i32::try_from(credited.1).unwrap_or(i32::MAX))
}

/// `(top, bottom)`: the demoted and pushed rows `report` left in the engine's
/// resize undo (all of them or none: `ResizeReport::stashed`).
fn stash_of(report: &ResizeReport) -> (u32, u32) {
    if report.stashed == 0 {
        (0, 0)
    } else {
        (u32::from(report.demoted), u32::from(report.pushed))
    }
}

/// `(rows, cols)` as the cast's `<cols>x<rows>`.
fn cxr((rows, cols): (u16, u16)) -> String {
    format!("{cols}x{rows}")
}

/// `site=<word> at=<pct file:line|->`.
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

/// `resize <id> t=<ms> run=<r> ord=<n> from=<CxR> to=<CxR> alt=<0|1>
/// sync=<0|1> trimmed= demoted= pushed= revealed= appended= restored= shift=<+-k>
/// site=<word> at=<file:line|-> win= chrome=`
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
         pushed={} revealed={} appended={} restored={} shift={:+} {} win={win} chrome={chrome}",
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
        wire_site(row.attribution.site),
    );
}

/// `run <r> t=<ms> n=<k> alt=<0|1> geom=<chain> net=<zero|changed>
/// displaced=<+-k> verdict=<v> first_out_ms=<ms|-> healed_ms=<ms|->`
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

/// `resizes [<n>] [since=<id>]`: the TARGET session's ledger. Cross-session
/// correct like `timeline` (both locks are the target's). The journal is
/// copied under a brief `term_lock`, released, then the timeline's leaf lock
/// backfills anything no path booked and evaluates: never nested. A copy that
/// arrives after a resize path booked a newer one is stale and books nothing.
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

    use aterm_spec::derive::resize_render_model;
    use aterm_spec::interp;

    use super::*;

    /// The engine lifetime the pure tests book from.
    const LIFE: u64 = 7;

    /// A report as the engine writes one, for the pure ledger tests.
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

    /// The incident's flap on the alternate screen: a demote then a grow, the
    /// second adjoining the first.
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
        // 0.7 ms apart (the same ms here) and nothing drawn between: one run.
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

        // 300 ms apart, a run that moved nothing (a trim): two runs, no flap.
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

        // The app drew between the halves of a flap that displaced rows (a
        // spinner tick, 2026-09-28): still one run, and it saw the draw — a
        // draw is no evidence that the handler read the smaller size.
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

        // A full clear between the halves (the handler read the smaller size
        // and repainted from scratch): two runs, the first healed.
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

        // A draw after a resize that moved nothing (a trim) is a fresh
        // baseline: two runs.
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

        // A displaced run nothing drew over waits for its grow back however
        // late: 600 ms on, the undo's grow nets it to zero.
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

        // A drawn-on flap past the gap takes no later resize, adjoining or
        // not: 400 ms after the tick-split flap, the next shrink opens a run
        // of its own, and the flap is still judged net-zero (joined, it would
        // net CHANGED and read `unverified`).
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

        // A drag the app follows frame by frame: a draw closes a displaced
        // run to a resize that goes on to a third size, and only the one
        // that takes the previous resize back (the turn) joins across it.
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

        // Backfilled rows (`site=-`) carry the time a reader found them, not
        // when they ran: a shrink and a grow found together, a draw between
        // them, are two runs — how far apart they ran is unknown.
        let mut found = ResizeLedger::default();
        let mut shrink = report(1, 24, 23, true, (10, 11));
        shrink.demoted = 1;
        let mut grow = report(2, 23, 24, true, (15, 16));
        grow.appended = 1;
        found.ingest(&copy_of(&[shrink, grow], probe(2, 16, 0)), None, 9_000);
        assert_eq!((found.high_run, found.unledgered), (2, 2));
        assert!(found.rows.iter().all(|row| !row.timed));
        // The newest booked by its own path, the older one backfilled: the
        // gap is still measured from the backfilled row, so still two runs.
        let mut half = ResizeLedger::default();
        half.ingest(
            &copy_of(&[shrink, grow], probe(2, 16, 0)),
            Some(here()),
            9_000,
        );
        assert_eq!((half.high_run, half.unledgered), (2, 1));
        assert!(half.rows[1].timed && !half.rows[0].timed);
        // Found together with nothing drawn between: adjoining, one run.
        let mut quiet = ResizeLedger::default();
        let [shrink, grow] = alt_flap(1, 10);
        quiet.ingest(&copy_of(&[shrink, grow], probe(2, 12, 0)), None, 9_000);
        assert_eq!((quiet.high_run, quiet.flaps), (1, 1));
    }

    /// Rows a grow hands back from the engine's resize undo go back to the run
    /// whose shrink stashed them, newest first (the undo pops LIFO). Two
    /// shrinks, each stashing a demoted row, then two grows handing them back,
    /// 300 ms apart: a run that displaced rows and was never drawn over waits
    /// for its next resize however late, so the four are ONE run whose own
    /// shift nets the hand-backs, it moves nothing, and a draw after it is no
    /// verdict at all; inside the gap the same. The control: the second grow
    /// finds the undo dropped (it appends), so the run is a net-zero one that
    /// lost a row, and a draw on it is a RISK. And a run the grow does NOT
    /// join (here: `credit` asked directly, as for a run a grouping closed
    /// while the undo still held its rows) is credited with the row it
    /// stashed, and the grow's own run does not count it.
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

        // The control: the undo was dropped before the last grow, which only
        // appends; the one net-zero run lost a row.
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

        // A run the grow does not join is credited, and the credit is said to
        // the grow's own run (which then does not count the row).
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
        // …and the run it joins nets the row in its own shift instead.
        let mut joining = ResizeLedger::default();
        book(&mut joining, &journal[..1], 0);
        assert_eq!(joining.credit(&back, Some(1)), (0, 0));
        assert_eq!(
            (joining.runs[0].displaced, joining.runs[0].held),
            (-1, (0, 0))
        );
    }

    /// A stash the ledger never saw breaks the newest-first pairing: the
    /// journal dropped a shrink's report (`lost=`), so the grow that hands its
    /// row back must not credit an OLDER run, whose own row output that drew
    /// nothing (a mode set) already dropped from the undo. That run keeps its
    /// shift and reads `unverified` once drawn on, never `none`. Two things
    /// hold it: the `lost=` branch gives every hold up, and, before it, the
    /// lost shrink's own re-layout moved the content past the run's, which the
    /// next booking reads as a draw (so this passes without the first too,
    /// measured 2026-09-28; the branch keeps it from resting on that).
    #[test]
    fn a_lost_report_credits_no_older_run() {
        let mut ledger = ResizeLedger::default();
        // Run 1: a shrink that stashed its demoted row.
        let mut first = report(1, 24, 23, true, (10, 11));
        (first.demoted, first.stashed) = (1, 1);
        ledger.ingest(&copy_of(&[first], probe(1, 11, 0)), Some(here()), 0);
        assert_eq!(ledger.runs[0].held, (1, 0));
        // A mode set dropped the undo (no counter moved); then a shrink the
        // journal dropped before any read stashed a row of its own, and a
        // grow handed THAT row back.
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
        // Trim-only: nothing moved.
        let mut trim = ResizeLedger::default();
        let mut shrink = report(1, 24, 23, true, (10, 11));
        shrink.trimmed = 1;
        trim.ingest(&copy_of(&[shrink], probe(1, 11, 0)), Some(here()), 0);
        assert_eq!(run_verdict(&trim, 1), Verdict::None);
        trim.evaluate(probe(1, 40, 0), 5_000);
        assert_eq!(trim.render(), Render::Ok);

        // The flap the engine UNDID (no output between the halves): the grow
        // handed the demoted row back, so the run nets to zero and moved
        // nothing, and a draw after it is no risk. The same for a bottom-push
        // flap: its pushed row came back to the bottom.
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

        // Flap, then ED 2: healed, nothing recorded.
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

        // Flap, a diff frame, a second: desync-risk; then ED 2: healed.
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

        // Flap and no output: silent, render=displaced, for as long as it lasts.
        let mut silent = ResizeLedger::default();
        silent.ingest(&copy_of(&flap, probe(2, 12, 0)), Some(here()), 0);
        silent.evaluate(probe(2, 12, 0), 60_000);
        assert_eq!(run_verdict(&silent, 1), Verdict::Silent);
        assert_eq!(silent.render(), Render::Displaced);

        // A shrink whose geometry STAYS changed, then a draw: the app's handler
        // read the change, and the draw may be its repaint or a diff on the
        // moved rows — never a risk, and not vouched for: unverified, until a
        // heal says more.
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
        // A later full clear is evidence: healed, and render=ok. No transition
        // (only desync-risk enters or leaves the timeline).
        assert_eq!(changed.evaluate(probe(1, 31, 1), 7_000), vec![]);
        assert_eq!(run_verdict(&changed, 1), Verdict::Healed);
        assert_eq!(changed.render(), Render::Ok);

        // A primary-screen width change: the terminal's own rewrap.
        let mut reflow = ResizeLedger::default();
        let mut wide = report(1, 24, 24, false, (10, 12));
        wide.to = (24, 100);
        wide.reflowed = true;
        wide.demoted = 3;
        reflow.ingest(&copy_of(&[wide], probe(1, 12, 0)), Some(here()), 0);
        assert_eq!(run_verdict(&reflow, 1), Verdict::Reflow);
        reflow.evaluate(probe(1, 20, 0), 5_000);
        assert_eq!(reflow.render(), Render::Ok);

        // The same flap on the primary screen is not judged.
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
        // A displaced run, then 300 ms later an unrelated resize: its own
        // re-layout bumps the content sequence, which must not read as a draw.
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

    /// Forty booked resizes, then the interleaving two threads produce: one
    /// copies the journal at ordinal 40, another resizes, copies at 41 and
    /// books first, then the first books its OLDER copy. The late copy is stale
    /// — it books nothing, counts nothing lost or unledgered, and evaluates
    /// nothing (its probe predates run 41) — and a late attribution it carries
    /// still lands on the row it names.
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
        // The newer resize: an alt-screen flap half that demotes, booked by a
        // reader with no attribution of its own (a backfill).
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

        // The stale copy lands: nothing is re-booked.
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

        // The path that made 41 books its own copy late: it takes the credit.
        let site = here();
        ledger.ingest(&fresh, Some(site), 41_002);
        assert_eq!((ledger.high_row, ledger.unledgered), (41, 0));
        assert_eq!(ledger.rows.back().expect("row").attribution, site);
    }

    /// A probe sampled BEFORE a run was booked (another thread's read, settled
    /// after the resize) says nothing about that run: its older content
    /// sequence is not a draw, and its lower counters are not a heal.
    #[test]
    fn a_stale_probe_neither_draws_nor_heals() {
        // Fake draw: an idle alt screen, the flap, then a probe from before it.
        let mut ledger = ResizeLedger::default();
        let flap = alt_flap(1, 10);
        let stale = probe(0, 10, 0);
        ledger.ingest(&copy_of(&flap, probe(2, 12, 0)), Some(here()), 100);
        assert_eq!(ledger.evaluate(stale, 101), vec![]);
        assert!(!ledger.runs[0].drew);
        assert_eq!(ledger.evaluate(probe(2, 12, 0), 1_600), vec![]);
        assert_eq!(run_verdict(&ledger, 1), Verdict::Silent, "never drawn");
        assert_eq!(ledger.render(), Render::Displaced);

        // Fake heal: an ED 2 counted BEFORE the flap (full_clears 3 at both
        // halves); a probe from before that clear reads 2, which is not past 3.
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
        // A probe AHEAD of the ledger (an unbooked resize re-laid the grid)
        // still sees a real heal, but not a draw.
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

    /// A restored engine (another lifetime token, ordinals from 0) rebases the
    /// ledger: its first resizes are booked, the old engine's open run is
    /// judged against the new engine's counters, and a probe of the OLD engine
    /// settles nothing.
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
        // Nothing lost across the restore; the flap's first half was booked
        // from the journal (only its second carried an attribution).
        assert_eq!((ledger.high_row, ledger.lost, ledger.unledgered), (3, 0, 1));
        assert_eq!(run_verdict(&ledger, 1), Verdict::Silent, "carried over");
        // The old engine's probe (content far ahead) is not a draw.
        ledger.evaluate(probe(2, 500, 9), 2_000);
        assert!(!ledger.runs[0].drew);
        // The new engine's clear heals it.
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

        // A path that did not record: the engine is two past the ledger.
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
        // Its own path arrives late with the newest one's attribution.
        ledger.ingest(
            &copy_of(&journal[start..], probe(202, 204, 0)),
            Some(here()),
            300_001,
        );
        assert_eq!((ledger.high_row, ledger.unledgered), (202, 1));
        let last = ledger.rows.back().expect("row");
        assert!(matches!(last.attribution.site, Site::Caller { .. }));

        // Twenty more the journal overran before anyone read it: 16 kept.
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
                "OK 3 total=2 runs=1 flaps=1 risks=0 render=displaced unledgered=0 lost=0\n\
                 resize 1 t=7 run=1 ord=1 from=80x24 to=80x23 alt=1 sync=1 trimmed=0 demoted=1 \
                 pushed=0 revealed=0 appended=0 restored=0 shift=-1 site=chrome at={at} win=135x63 chrome=2\n\
                 resize 2 t=7 run=1 ord=2 from=80x23 to=80x24 alt=1 sync=0 trimmed=0 demoted=0 \
                 pushed=0 revealed=0 appended=1 restored=0 shift=+0 site=ctl-cross at=- win=- chrome=-\n\
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
        // No row past id 2, but run 1 is still open (silent): it is listed
        // whatever the selection, so `render=displaced` always names its run.
        assert_eq!(
            ledger.reply(0, Some(2)),
            "OK 1 total=2 runs=1 flaps=1 risks=0 render=displaced unledgered=0 lost=0\n\
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

        // A long drag's chain keeps its ends.
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

    /// The OUTERMOST entry point names a pass, from the line that called it;
    /// leaving the scope — a panic's unwind included — clears it.
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
            Some("OK 3 total=2 runs=1 flaps=1 risks=0 render=ok unledgered=2 lost=0"),
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
        // Nothing past row 2, and the undone run is not silent, so it is not
        // listed whatever the selection.
        assert!(cmd_resizes(ctx, term, "since=2").starts_with("OK 0 "));
    }

    /// The window pass books each resize it makes, with its entry point and
    /// the line that called it (`site=term at=<this file>:<line>`), the
    /// window's grid and its chrome rows; a pass that changes nothing books
    /// nothing.
    #[test]
    fn the_window_pass_books_its_caller_the_grid_and_the_chrome() {
        let mut app = crate::App::headless_for_test(); // window 0 = 24x80, session 0
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
        assert!(reply.contains(" unledgered=0 lost=0\n"), "{reply}");
    }

    /// The reviewer's false alarm, on the real engine: a real 8 -> 6 shrink
    /// (the geometry stays changed), after which the app repaints every row
    /// with CUP + text + EL and no ED 2. The screen is exactly the app's frame
    /// and no counter moved: the ledger calls the run `unverified` — not a
    /// risk, and not vouched for either, since a spinner tick on the moved rows
    /// moves the same counters; `cast drift` is the authority there.
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

    // ---- Tier-1: ResizeRender ----------------------------------------------

    /// The Claude-shaped frame's full height; one row fewer is `geom = 1`.
    const ROWS: u16 = 8;
    const COLS: u16 = 20;
    /// The model's displacement box.
    const CAP: i64 = 2;

    /// `L<r>` on each row but the last, `F<tick>` (the footer) on the last,
    /// with absolute cursor moves only, and the cursor parked on the composer
    /// three rows from the bottom, as Claude Code parks it.
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

    /// The application stand-in: the rows it laid its frame out for and what
    /// it believes each row shows.
    struct App {
        rows: u16,
        believed: Vec<String>,
        tick: u32,
    }

    impl App {
        /// A full repaint at `rows` (its SIGWINCH handler read a changed size):
        /// ED 2, then every row.
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

        /// A diff frame: the footer's spinner advances, drawn at the row the
        /// app believes is its last, with absolute moves and EL, no clear.
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

    /// The real terminal, the stand-in, the real ledger and what the model
    /// cannot read off them: whether the app drew onto displaced content
    /// (ground truth, from the screen at the moment it drew) and the last
    /// verdict.
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
            }
        }

        /// How many rows up the screen's content sits from where the app
        /// painted it: the witness is row 0, which no diff frame touches.
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

        /// The ledger's OPEN run: the newest one, when the next resize would
        /// join it (`push`'s own test, [`joins_run`], read off the engine as
        /// it stands: the same run, still joinable, no heal since its last
        /// resize — `push`'s settling of it first — and within the gap with
        /// nothing drawn since or the run `silent` — the next resize here, with
        /// two geometries, always takes the last one back — or `silent`,
        /// undrawn and nothing drawn since). Every row here is booked by its
        /// own path (`timed`).
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

        /// The projection onto `ResizeRender`'s variables: the screen, the
        /// engine's resize undo (`st`), the stand-in and the ledger's runs
        /// (`ro`/`rs`/`rd`/`rw` the open one; `led` and `lr` the closed,
        /// unhealed, judged net-zero ones; `lb` and `br` the closed, unhealed,
        /// judged net-changed ones that displaced rows, and whether the app drew
        /// on one; `blind` is ground truth).
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
            // `lr` reads a net-zero run moved EITHER way, as the ledger judges
            // it (`Run::judged`); `led` sums only the up-moved ones, as the
            // model books them.
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
            // `rw` is the run's own `drew` whatever its verdict: a run a
            // grow or a shrink brought back to `displaced == 0` is no longer
            // judged (`Verdict::None`), yet keeps the draw it saw, and the
            // model's `rw` keeps it too (a depth-9 schedule, pinned below as
            // `a_run_brought_back_to_zero_keeps_the_draw_it_saw`).
            let (ro, rs, rd, rw) = open.map_or((0, 0, 0, false), |run| {
                (
                    1,
                    i64::from(run.start.0 != ROWS),
                    -i64::from(run.displaced),
                    run.drew
                        || (run.verdict == Verdict::Silent && probe.content_seq > run.quiet_seq),
                )
            });
            BTreeMap::from([
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

        /// Resize the real engine to `rows` and ingest its journal the way the
        /// window pass does (copied under the resize's hold, booked after).
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

        /// A diff frame; ground truth marks it drawn onto displaced content
        /// when the screen, at that moment, is displaced. Output inside the
        /// open run (`out`).
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

        /// Drive one step and name the model action it is. A shrink is a
        /// `Demote` or a `Trim` by what the engine journalled it did.
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
                    // The displacement the reader saw is gone: a later one
                    // is a new episode, which that reader never saw.
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
                    // The first bytes of a SIGWINCH handler: mode sets, no
                    // content. Any output drops the engine's resize undo.
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
                Step::Evaluate => {
                    if self.displacement() >= 1 && self.term.rows() != ROWS {
                        self.blind = true;
                    }
                    self.now_ms += RISK_AFTER_MS;
                    let probe = EngineProbe::sample(&self.term);
                    self.ledger.evaluate(probe, self.now_ms);
                    self.evaluated = true;
                    self.verdict = match self.ledger.render() {
                        Render::DesyncRisk => 1,
                        Render::Unverified => 2,
                        Render::Ok | Render::Displaced => 0,
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
        Evaluate,
    }

    const STEPS: [Step; 6] = [
        Step::Shrink,
        Step::Grow,
        Step::Handler,
        Step::Draw,
        Step::Output,
        Step::Evaluate,
    ];

    // The machine's other actions are the environment the ledger reads: the
    // engine's resize (journalled; the ledger's booking of it is `ingest`,
    // which every schedule below drives) and the app's own frames. `Grow` is
    // not waived here: its ledger half, the credit of the rows the grow hands
    // back to the run that stashed them (the model's `rd - st` in the run the
    // grow joins, a late one included), is
    // `ResizeLedger::credit`, which carries its `refines` anchor (spec-link
    // refuses an action both anchored and waived). Its engine half, the grid's
    // alt-screen grow that hands back what the resize undo kept, is driven for
    // real by the Tier-1 rig below, as the shrinks are.
    #[aterm_spec::spec_unmodeled(
        machine = "ResizeRender",
        action = "Demote",
        reason = "The engine's alt-screen shrink with a full tail: aterm-core's grid resize, \
                  journalled as a ResizeReport. The Tier-1 rig drives the real Terminal \
                  through it and books it with the real ResizeLedger::ingest."
    )]
    #[aterm_spec::spec_unmodeled(
        machine = "ResizeRender",
        action = "Trim",
        reason = "The engine's alt-screen shrink with a blank tail: aterm-core's grid resize, \
                  driven for real by the Tier-1 rig."
    )]
    #[aterm_spec::spec_unmodeled(
        machine = "ResizeRender",
        action = "Output",
        reason = "Application output that draws nothing (a mode set): an environment step the \
                  Tier-1 rig's stand-in performs on the real Terminal, which drops the engine's \
                  resize undo."
    )]
    #[aterm_spec::spec_unmodeled(
        machine = "ResizeRender",
        action = "Step",
        reason = "The application's SIGWINCH handler and its next frame: an environment step \
                  the Tier-1 rig's stand-in performs on the real Terminal."
    )]
    #[aterm_spec::spec_unmodeled(
        machine = "ResizeRender",
        action = "Draw",
        reason = "An application diff frame: an environment step the Tier-1 rig's stand-in \
                  performs on the real Terminal."
    )]
    #[expect(
        dead_code,
        reason = "carrier for the `spec_unmodeled` waivers above; nothing calls it"
    )]
    fn resize_render_scope_waivers() {}

    /// Replay `schedule` on a fresh rig, checking every real transition
    /// against the model: the action is enabled in the projected state, the
    /// projected successor is the model's, and every invariant holds.
    fn conform(
        model: &aterm_spec::derive::Model,
        schedule: &[Step],
        seen: &mut BTreeMap<&'static str, usize>,
    ) -> Option<BTreeMap<&'static str, i64>> {
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
            let action = rig.drive(step);
            assert!(
                model.action_enabled(action, &state),
                "{schedule:?}: real `{action}` from a state the model disables: {state:?}"
            );
            if action == "Grow" && state["late"] == 1 {
                *seen.entry("Grow joins a late run").or_default() += 1;
            }
            let mut expected = state.clone();
            assert!(model.fire(action, &mut expected));
            let real = rig.project();
            assert_eq!(real, expected, "{schedule:?}: `{action}` from {state:?}");
            for invariant in [
                "RiskIsFlagged",
                "NoSilentPass",
                "NoFalseVerdict",
                "DisplacedIsShown",
                "NoFalseDisplaced",
                "QuietFlapIsIdentity",
                "Bounded",
            ] {
                assert!(
                    model.check_invariant(invariant, &real),
                    "{invariant}: {real:?}"
                );
            }
            *seen.entry(action).or_default() += 1;
            state = real;
        }
        Some(state)
    }

    /// Every schedule of rig steps up to depth 6 (a step never repeated back
    /// to back, which adds no state), each replayed on a real alternate-screen
    /// Terminal and the real ledger.
    #[test]
    fn resize_render_conformance() {
        let model = resize_render_model();
        let mut seen = BTreeMap::new();
        let mut verdicts = [0usize; 3];
        let mut shown = [0usize; 2];
        let mut blind = 0usize;
        let mut late = 0usize;
        let mut max_disp = 0;
        let mut stack: Vec<Vec<Step>> = vec![Vec::new()];
        let mut schedules = 0usize;
        while let Some(prefix) = stack.pop() {
            if prefix.len() == 6 {
                continue;
            }
            for step in STEPS {
                if prefix.last() == Some(&step) {
                    continue;
                }
                let mut schedule = prefix.clone();
                schedule.push(step);
                if !valid_geometry(&schedule) {
                    continue;
                }
                schedules += 1;
                if let Some(end) = conform(&model, &schedule, &mut seen) {
                    if end["evaluated"] == 1 {
                        let v = usize::try_from(end["verdict"]).expect("0..=2");
                        verdicts[v] += 1;
                        shown[usize::from(end["shown"] == 1)] += 1;
                    }
                    blind += usize::from(end["blind"] == 1);
                    late += usize::from(end["late"] == 1);
                    max_disp = max_disp.max(end["disp"]);
                    stack.push(schedule);
                }
            }
        }
        for action in [
            "Demote",
            "Trim",
            "Grow",
            "Step",
            "Draw",
            "Output",
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
        assert!(blind > 0, "the blind spot is reached, and named");
        assert!(late > 0, "a silent run outlives the run gap");
        assert_eq!(max_disp, CAP, "the displacement box is reached");
        assert!(schedules > 1_000, "{schedules} schedules");
    }

    /// A RUN BROUGHT BACK TO ZERO KEEPS THE DRAW IT SAW (review of the
    /// integration, 2026-09-29): past the exhaustive depth, a time-closed
    /// run's rows still in the undo open a run the next grow hands them back
    /// to (`rd = -1`), a draw lands inside the gap (`rw = 1`), and the shrink
    /// that follows joins and brings `rd` back to 0 — no longer judged, so the
    /// ledger's verdict is `None`, but its `drew` stands, as the model's `rw`
    /// does. The projection read the draw only while `Silent` and answered
    /// `rw = 0` (8 schedules of 673,093 at depth 9, all this class). No
    /// verdict moved (`br` already forced `unverified`); the successor did.
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

    /// Shrinks and grows alternate from the full height.
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

    /// THE SPLIT FLAP, on the real engine (2026-09-28, live on main: an
    /// attention set/unset 2-15 ms apart with the app ticking). A diff frame
    /// between the halves drops the engine's undo, so the grow appends and the
    /// top row is lost; the ledger keeps the frame inside the run and flags
    /// the net-zero run it is. The negative control: the real journal shows
    /// the frame between the halves (`adjoins` fails: a judge that closes a
    /// run on any draw split the pair there into two net-changed runs), and
    /// the model at `Buggy = 5` (that judge) answers `unverified` on the same
    /// schedule, breaking `RiskIsFlagged`.
    ///
    /// And the flap the undo RESTORED with its halves further apart than the
    /// run gap (the attention held, nothing drawn): one net-zero run that
    /// moved nothing, `render=ok`, where a judge that closes a run on time
    /// alone left two `silent` halves and `render=displaced` for good
    /// (`Buggy = 3` breaks `NoFalseDisplaced`).
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

        // The restored flap a reader saw short, then the tick-split one: the
        // blind spot excused the first displacement only, and the grow that
        // put it back ended it, so the second is judged — on the real engine
        // and ledger (flagged) and in the model, where the draw-closing judge
        // answers `unverified` and breaks `RiskIsFlagged`.
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

    /// THE INCIDENT'S SCHEDULE, on the fixed engine and on the historical one.
    ///
    /// Fixed: the grow hands the demoted row back (the resize undo), the app's
    /// diff frame lands on what it painted, and the ledger reads `none`.
    ///
    /// THE NEGATIVE CONTROL: the same schedule on the historical engine (the
    /// undo dropped before the grow, so it can only append). The real ledger
    /// calls it a desync risk; the size-difference judge (the historical bug:
    /// flag a render only while the grid's size differs from the app's) answers
    /// ok; and the model at `Buggy = 1` on the same schedule breaks
    /// `QuietFlapIsIdentity` and `RiskIsFlagged`.
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

        // A mode set between the halves (output that draws nothing) drops the
        // undo but not the run: the real engine now displaces, and the real
        // ledger judges it, conforming to the model at `Buggy = 0`.
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

    /// THE SECOND NEGATIVE CONTROL: a spinner tick on the moved rows of a
    /// shrink whose size has not come back, on the real engine and ledger.
    /// The screen is one row up and the app drew on it; the real ledger says
    /// `unverified`. The assumed-repaint judge the ledger first landed with
    /// (`repainted`, never in a release; the model's `Buggy = 2`) answers ok
    /// on the same schedule and breaks `NoSilentPass`. (Before the
    /// integration with the split-flap grouping this control was the flap a
    /// tick splits; that one is now a single net-zero run and a RISK, which
    /// `a_flap_split_by_a_frame_or_by_time_is_judged_on_the_real_engine`
    /// pins.)
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

    /// THE LATE RUN AND THE CREDIT, on the real engine and ledger: a quiet
    /// flap whose halves only TIME split (nothing was drawn) is still an
    /// identity on the screen, since the undo kept the demoted row. The
    /// shrink's run, `silent` and undrawn, waits for the grow however late,
    /// and the grow that hands the row back credits it there: one run that
    /// moved nothing, and a spinner tick after it reads ok. Once a mode set
    /// drops the undo the grow can only append, and the same run is a
    /// net-zero one that lost a row: the tick on it is a RISK. The model that
    /// closes a run on time alone (`Buggy = 3`, neither the late run nor the
    /// credit) reads the first schedule `unverified` over a screen that is
    /// exactly the app's and breaks `NoFalseVerdict`.
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
}
