// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The render epoch: the resize ledger's second judge, read from the engine's debt
//! instead of from the shape of a run.
//!
//! The ledger judges net-zero runs, and a run closes when the app draws, so a flap
//! whose halves an app frame separates is two net-changed runs and neither is judged
//! (the ledger's named blind spot: every row ends one higher and only `cast drift`
//! says so). The engine's undo makes the quiet flap an identity but cannot help once
//! anything is written, so a repair needs a trigger that does not depend on run shape.
//!
//! An epoch opens at the first alternate-screen resize after the screen was last
//! repainted from scratch and remembers the geometry before it (`start`). The
//! engine's [`Terminal::render_debt`] says what is owed: `(0, 0)` when every row sits
//! where the program painted it. The epoch is at risk when the debt is not zero, the
//! geometry is back at `start` and [`RISK_AFTER_MS`] has passed since the last
//! resize: a program that compares the size it reads with the size it drew for can
//! never read the intermediate size again, so it has no reason to repaint. While the
//! geometry differs from `start` the program's handler still reads a change and
//! repaints, so the epoch is only `displaced`. Whether the program has drawn since is
//! information (`drew=`), not a condition: an undrawn residual (a mode set between
//! the halves, an idle tab) is as displaced as a drawn one.
//!
//! Whether an epoch at risk is accused depends on the program's class:
//! [`ProgramClass::Agent`] (Claude Code, the one program whose repaint contract, an
//! ED 2 on a changed size read and nothing on an unchanged one, was measured),
//! [`ProgramClass::Learned`] (a run of this session was healed by a full clear within
//! [`RISK_AFTER_MS`] of its last resize), or [`ProgramClass::Unknown`]. An unknown
//! program that repaints with cursor addressing and no ED 2 is indistinguishable from
//! one that missed, so its at-risk epoch reads `unjudged` (on `status` and `resizes`,
//! never pushed as an event).
//!
//! The ledger books every resize at the one funnel and, when that leaves an epoch
//! open, registers its next look at `last_resize + RISK_AFTER_MS` ([`note_look`]);
//! the event loop folds that instant under `DeadlineOwner::RenderEpoch` and runs the
//! look (`App::observe_render_epochs`). An idle machine folds nothing; the status
//! sweep and `subscribe` only read the verdict. No clock is read here: the host
//! passes `now_ms`.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};

use aterm_core::terminal::ResizeReport;

use crate::resize_ledger::{EngineProbe, RISK_AFTER_MS, Transition};

/// Closed epochs kept for `resizes` (drop-oldest).
pub(crate) const EPOCHS_KEPT: usize = 8;
/// Asking again after a look found the terminal locked: after this long, at most
/// [`LOOK_RETRIES`] times, then at the next [`RISK_AFTER_MS`].
pub(crate) const LOOK_RETRY_MS: u64 = 100;
/// See [`LOOK_RETRY_MS`].
pub(crate) const LOOK_RETRIES: u32 = 20;

/// What the ledger knows of the program behind a session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProgramClass {
    /// The presence classifier identifies Claude Code.
    Agent,
    /// This session cleared its screen when resized (see the module note).
    Learned,
    /// Neither.
    Unknown,
}

impl ProgramClass {
    /// The wire word (`class=`).
    pub(crate) const fn wire(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Learned => "learned",
            Self::Unknown => "unknown",
        }
    }

    /// An epoch at risk of this class is accused (`desync-risk`), not
    /// `unjudged`.
    pub(crate) const fn judged(self) -> bool {
        !matches!(self, Self::Unknown)
    }
}

/// An epoch's state (`verdict=`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EpochVerdict {
    /// Open: rows are owed and the program may still read the change.
    Open,
    /// At risk, and the class is judged.
    DesyncRisk,
    /// At risk, and the program has not shown it clears the screen.
    Unjudged,
    /// A full clear or a screen switch repainted it.
    Healed,
    /// The debt returned to zero with no heal: an undo handed the rows back.
    Restored,
}

impl EpochVerdict {
    /// The wire token.
    pub(crate) const fn wire(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::DesyncRisk => "desync-risk",
            Self::Unjudged => "unjudged",
            Self::Healed => "healed",
            Self::Restored => "restored",
        }
    }
}

/// Who closed an epoch (`by=`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EpochBy {
    /// Still open.
    None,
    /// The program repainted (a full clear or a screen switch).
    App,
    /// The engine's undo handed the rows back.
    Undo,
    /// The repaint nudge (`crate::app_nudge`) made the program repaint.
    Nudge,
}

impl EpochBy {
    /// The wire token.
    pub(crate) const fn wire(self) -> &'static str {
        match self {
            Self::None => "-",
            Self::App => "app",
            Self::Undo => "undo",
            Self::Nudge => "nudge",
        }
    }
}

/// One epoch (see the module note).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Epoch {
    pub(crate) id: u64,
    /// The first resize's time, on the timeline clock.
    pub(crate) t_ms: u64,
    /// The last resize's time.
    pub(crate) last_resize_ms: u64,
    /// `(rows, cols)` before the first resize of the epoch.
    pub(crate) start: (u16, u16),
    /// `(rows, cols)` as last read.
    pub(crate) now: (u16, u16),
    /// The engine's debt as last read.
    pub(crate) shift: i32,
    pub(crate) pushed: i32,
    /// The program drew (the content moved other than by a resize) since.
    pub(crate) drew: bool,
    /// The content sequence the screen shows while the program has drawn
    /// nothing since: the last resize's.
    quiet_seq: u64,
    /// The engine's heal counters when the epoch opened: either advancing past
    /// them is a repaint from scratch.
    full_clears: u64,
    screen_replaced: u64,
    /// The engine owed rows at some reading.
    had_debt: bool,
    pub(crate) class: ProgramClass,
    pub(crate) verdict: EpochVerdict,
    pub(crate) by: EpochBy,
    /// A repair was attempted (the repaint nudge held the PTY for this epoch).
    pub(crate) tried: bool,
    /// That repair ended superseded by a real resize (the program was given a
    /// size of its own): its end is this judge's if the epoch comes back to
    /// `desync-risk` at its start geometry ([`returned_at_risk`]).
    superseded: bool,
    /// The verdict reached `desync-risk` or `unjudged` and was logged.
    logged: bool,
    /// `desync-risk` was pushed as an event (its `healed` follows).
    announced: bool,
}

impl Epoch {
    fn open(id: u64, report: &ResizeReport, now_ms: u64) -> Self {
        Self {
            id,
            t_ms: now_ms,
            last_resize_ms: now_ms,
            start: report.from,
            now: report.to,
            shift: 0,
            pushed: 0,
            drew: false,
            quiet_seq: report.content_seq,
            full_clears: report.full_clears,
            screen_replaced: report.screen_replaced,
            had_debt: false,
            class: ProgramClass::Unknown,
            verdict: EpochVerdict::Open,
            by: EpochBy::None,
            tried: false,
            superseded: false,
            logged: false,
            announced: false,
        }
    }

    fn healed_by(&self, full_clears: u64, screen_replaced: u64) -> bool {
        full_clears > self.full_clears || screen_replaced > self.screen_replaced
    }

    /// Worth a line in `resizes`: rows were owed at some reading.
    pub(crate) fn listed(&self) -> bool {
        self.had_debt
    }
}

/// The soonest instant any ledger wants an epoch look (timeline ms), `u64::MAX` for
/// none. A process-wide minimum: a ledger only ever lowers it ([`note_look`]); the
/// event loop's look ([`take_looks`]) resets it and every ledger re-registers what it
/// still owes. A stale early value costs one walk that finds nothing due; it can never
/// be later than a registered look.
static NEXT_LOOK_MS: AtomicU64 = AtomicU64::new(u64::MAX);

/// A ledger owes a look at `at_ms`.
pub(crate) fn note_look(at_ms: u64) {
    NEXT_LOOK_MS.fetch_min(at_ms, Ordering::AcqRel);
}

/// The soonest look owed (timeline ms), `None` when nothing is.
pub(crate) fn next_look_ms() -> Option<u64> {
    match NEXT_LOOK_MS.load(Ordering::Acquire) {
        u64::MAX => None,
        at => Some(at),
    }
}

/// Reset the minimum before the event loop walks the sessions: each due ledger
/// re-registers what it still owes as it is looked at, and a registration that
/// races the walk survives (`fetch_min`).
pub(crate) fn take_looks() {
    NEXT_LOOK_MS.store(u64::MAX, Ordering::Release);
}

/// Bumped whenever an epoch of the agent class is at `desync-risk` and untried, or a
/// superseded repair's epoch is back at risk ([`returned_at_risk`]): the event loop's
/// cue to walk the sessions, arm the repaint nudge (`crate::app_nudge`) and raise
/// what is owed. Noted by whichever path judged the epoch (the loop's own look, a
/// `status` read, a `subscribe` tick). A generation, not a flag: every `App` compares
/// it with the one it last scanned, so no reader consumes another's cue.
static NUDGE_DUE: AtomicU64 = AtomicU64::new(0);

/// An agent-class epoch at `desync-risk` is owed the loop's walk: tell it.
pub(crate) fn note_nudge_due() {
    NUDGE_DUE.fetch_add(1, Ordering::AcqRel);
}

/// The cue's generation now.
pub(crate) fn nudge_due_generation() -> u64 {
    NUDGE_DUE.load(Ordering::Acquire)
}

/// What the repaint nudge needs to know of a session's open epoch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct NudgeView {
    /// The epoch's id.
    pub(crate) epoch: u64,
    /// The engine's debt as last read.
    pub(crate) shift: i32,
    /// Rows pushed off the bottom, likewise.
    pub(crate) pushed: i32,
    /// The program drew since.
    pub(crate) drew: bool,
}

/// The ledger's epochs.
#[derive(Clone, Debug, Default)]
pub(crate) struct EpochBook {
    current: Option<Epoch>,
    closed: VecDeque<Epoch>,
    high: u64,
    /// The presence classifier reads Claude Code on this session.
    agent: bool,
    /// A run of this session was healed by a full clear inside the risk window.
    learned: bool,
    /// `aterm.log` lines a judgement owes (drained by the timeline).
    logs: Vec<String>,
    /// The next look owed on the current epoch (timeline ms).
    look_at_ms: Option<u64>,
    look_tries: u32,
    /// The repaint nudge's phase word and attempt count, for the `resizes`
    /// header (`nudge=<phase> nudged=<n>`).
    nudge_phase: &'static str,
    nudged: u32,
    /// A superseded repair's epoch this judge found back at risk where the
    /// hold found it: the give-up the window owes (drained by its walk).
    returned: Option<u64>,
}

impl EpochBook {
    /// The presence classifier's reading of this session's program.
    pub(crate) fn set_agent(&mut self, agent: bool) {
        self.agent = agent;
    }

    /// This session has shown it clears its screen when resized.
    pub(crate) fn learn(&mut self) {
        self.learned = true;
    }

    /// The class now. `ATERM_NUDGE_CLASS=agent` is a development seam (compiled
    /// only into dev-seams and debug builds) that lets the lane's stand-in be the
    /// agent class; it is honoured here and nowhere else.
    pub(crate) fn class(&self) -> ProgramClass {
        static SEAM: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        let seam = *SEAM.get_or_init(|| {
            aterm_types::dev_seam!("ATERM_NUDGE_CLASS").is_some_and(|v| v.to_str() == Some("agent"))
        });
        if self.agent || seam {
            ProgramClass::Agent
        } else if self.learned {
            ProgramClass::Learned
        } else {
            ProgramClass::Unknown
        }
    }

    /// The open epoch when it is at `desync-risk` for the agent class and no
    /// repair was tried for it: what the repaint nudge acts on. `learned` is not
    /// the agent class - only the program whose repaint contract was measured is
    /// ever actuated.
    pub(crate) fn nudge_view(&self) -> Option<NudgeView> {
        let epoch = self.current.as_ref()?;
        (epoch.verdict == EpochVerdict::DesyncRisk
            && epoch.class == ProgramClass::Agent
            && !epoch.tried)
            .then_some(NudgeView {
                epoch: epoch.id,
                shift: epoch.shift,
                pushed: epoch.pushed,
                drew: epoch.drew,
            })
    }

    /// Whether epoch `id` is still the open one and the screen may still be
    /// drawn off (`open`, `desync-risk` or `unjudged`): the question the
    /// digest's `render.<tab>.unrepaired` line ends on.
    pub(crate) fn still_at_risk(&self, id: u64) -> bool {
        self.current.as_ref().is_some_and(|epoch| {
            epoch.id == id
                && epoch.had_debt
                && matches!(
                    epoch.verdict,
                    EpochVerdict::Open | EpochVerdict::DesyncRisk | EpochVerdict::Unjudged
                )
        })
    }

    /// A repair was attempted for epoch `id`: it is tried for good (one try per
    /// epoch), and if the screen is then repainted the epoch closes `by=nudge`.
    pub(crate) fn note_tried(&mut self, id: u64) {
        if let Some(epoch) = self.current.as_mut().filter(|e| e.id == id) {
            epoch.tried = true;
        }
    }

    /// The repair tried for epoch `id` ended superseded by a real resize: if
    /// the resize does not last, its end is this judge's ([`Self::judge`]).
    pub(crate) fn note_superseded(&mut self, id: u64) {
        if let Some(epoch) = self.current.as_mut().filter(|e| e.id == id) {
            epoch.superseded = true;
        }
    }

    /// The epoch whose superseded repair this judge found back at risk where
    /// the hold found it, once: the window raises the give-up.
    pub(crate) fn take_returned(&mut self) -> Option<u64> {
        self.returned.take()
    }

    /// The nudge's phase word and attempt count for the `resizes` header.
    pub(crate) fn set_nudge(&mut self, phase: &'static str, nudged: u32) {
        self.nudge_phase = phase;
        self.nudged = nudged;
    }

    /// `nudge=<phase> nudged=<n>`.
    pub(crate) fn nudge_wire(&self) -> String {
        let phase = if self.nudge_phase.is_empty() {
            "idle"
        } else {
            self.nudge_phase
        };
        format!("nudge={phase} nudged={}", self.nudged)
    }

    /// The open epoch, when there is one.
    #[cfg(test)]
    pub(crate) fn current(&self) -> Option<&Epoch> {
        self.current.as_ref()
    }

    /// The epochs `resizes` lists, oldest first: the closed ones that owed rows,
    /// then the open one when it does.
    pub(crate) fn listed(&self) -> impl Iterator<Item = &Epoch> {
        self.closed
            .iter()
            .chain(self.current.iter())
            .filter(|e| e.listed())
    }

    /// The instant the next look is owed (timeline ms).
    pub(crate) fn look_at_ms(&self) -> Option<u64> {
        self.look_at_ms
    }

    /// Drain the log lines a judgement owes.
    pub(crate) fn take_logs(&mut self) -> Vec<String> {
        std::mem::take(&mut self.logs)
    }

    /// A resize was booked (`report`, at `now_ms`). Only the alternate screen's
    /// non-reflowed resizes are an epoch's: the primary screen is
    /// history-backed and a rewrap renumbers rows on its own.
    pub(crate) fn note_resize(&mut self, report: &ResizeReport, now_ms: u64) {
        if !report.alt || report.reflowed {
            return;
        }
        if let Some(epoch) = self.current.as_mut() {
            if epoch.healed_by(report.full_clears, report.screen_replaced) {
                // The screen was repainted from scratch before this resize and
                // no reading saw it: that epoch ended, this one is new.
                let closed = self.current.take();
                self.close(closed, EpochVerdict::Healed, EpochBy::App);
            } else {
                if report.content_seq_before > epoch.quiet_seq {
                    epoch.drew = true;
                }
                epoch.quiet_seq = report.content_seq;
                epoch.last_resize_ms = now_ms;
                epoch.now = report.to;
                self.look_tries = 0;
                return;
            }
        }
        self.high = self.high.saturating_add(1);
        self.current = Some(Epoch::open(self.high, report, now_ms));
        self.look_tries = 0;
    }

    /// Judge the epoch against one engine reading. `last_ordinal` is the
    /// newest resize the ledger has booked: a reading taken with another count
    /// says nothing about the content sequence. `run_risk` is true while a run
    /// already stands at `desync-risk` (its event is the announcement).
    /// `healed_run` is true when this same look already pushed a run's `healed`.
    pub(crate) fn judge(
        &mut self,
        probe: &EngineProbe,
        last_ordinal: u64,
        now_ms: u64,
        run_risk: bool,
        healed_run: bool,
        out: &mut Vec<Transition>,
    ) {
        let class = self.class();
        let debt = (probe.debt_shift, probe.debt_pushed);
        if self.current.is_none() && debt != (0, 0) && probe.geom.0 > 0 {
            // Rows are owed and no resize of this ledger explains it: a restored
            // engine seeded with its predecessor's debt (the handoff). The
            // geometry is taken to be the one the program painted.
            self.high = self.high.saturating_add(1);
            let synthetic = ResizeReport {
                from: probe.geom,
                to: probe.geom,
                alt: true,
                content_seq: probe.content_seq,
                full_clears: probe.full_clears,
                screen_replaced: probe.screen_replaced,
                ..ResizeReport::default()
            };
            let mut epoch = Epoch::open(self.high, &synthetic, now_ms);
            epoch.t_ms = now_ms;
            self.current = Some(epoch);
        }
        let Some(epoch) = self.current.as_mut() else {
            self.look_at_ms = None;
            return;
        };
        epoch.now = probe.geom;
        epoch.class = class;
        if debt == (0, 0) {
            // The engine owes nothing: a quiet flap (nothing to record), or a
            // heal / an undo that ended a debt.
            let owed = epoch.had_debt;
            let tried = epoch.tried;
            let healed = epoch.healed_by(probe.full_clears, probe.screen_replaced);
            let since = now_ms.saturating_sub(epoch.last_resize_ms);
            let closed = self.current.take();
            self.look_at_ms = None;
            if owed {
                let (verdict, by) = if healed && tried {
                    (EpochVerdict::Healed, EpochBy::Nudge)
                } else if healed {
                    (EpochVerdict::Healed, EpochBy::App)
                } else {
                    (EpochVerdict::Restored, EpochBy::Undo)
                };
                if healed && since <= RISK_AFTER_MS {
                    self.learned = true;
                }
                let announced = closed.as_ref().is_some_and(|e| e.announced);
                let id = closed.as_ref().map_or(0, |e| e.id);
                self.close(closed, verdict, by);
                if announced && !healed_run {
                    out.push(Transition::EpochHealed {
                        epoch: id,
                        after_ms: since,
                        by,
                    });
                }
            }
            return;
        }
        epoch.had_debt = true;
        epoch.shift = debt.0;
        epoch.pushed = debt.1;
        if probe.ordinal == last_ordinal && probe.content_seq > epoch.quiet_seq {
            epoch.drew = true;
        }
        let since = now_ms.saturating_sub(epoch.last_resize_ms);
        let back = probe.geom == epoch.start;
        if back && since >= RISK_AFTER_MS {
            epoch.verdict = if class.judged() {
                EpochVerdict::DesyncRisk
            } else {
                EpochVerdict::Unjudged
            };
            if !epoch.logged {
                epoch.logged = true;
                self.logs.push(format!(
                    "render epoch {} {}: class={} geom={}x{} shift={:+} pushed={} drew={}",
                    epoch.id,
                    epoch.verdict.wire(),
                    class.wire(),
                    epoch.start.1,
                    epoch.start.0,
                    epoch.shift,
                    epoch.pushed,
                    u8::from(epoch.drew),
                ));
            }
            if epoch.verdict == EpochVerdict::DesyncRisk
                && class == ProgramClass::Agent
                && !epoch.tried
            {
                note_nudge_due();
            }
            if let Some(id) = returned_at_risk(epoch) {
                self.returned = Some(id);
                note_nudge_due();
            }
            if epoch.verdict == EpochVerdict::DesyncRisk && !epoch.announced && !run_risk {
                epoch.announced = true;
                out.push(Transition::EpochRisk {
                    epoch: epoch.id,
                    shift: epoch.shift,
                    pushed: epoch.pushed,
                    drew: epoch.drew,
                    class,
                    at_ms: epoch.t_ms,
                });
            }
            // Judged: no further look is owed until it changes.
            self.look_at_ms = None;
        } else {
            epoch.verdict = EpochVerdict::Open;
            // The next look: when the risk wait ends, or, with the geometry not
            // back, at the next resize (which re-registers it).
            self.look_at_ms = back.then_some(epoch.last_resize_ms.saturating_add(RISK_AFTER_MS));
        }
        if let Some(at) = self.look_at_ms {
            note_look(at.max(now_ms.saturating_add(1)));
        }
    }

    /// A look found the terminal locked: ask again soon (bounded), then at the
    /// next risk wait.
    pub(crate) fn defer_look(&mut self, now_ms: u64) {
        self.look_tries = self.look_tries.saturating_add(1);
        let after = if self.look_tries <= LOOK_RETRIES {
            LOOK_RETRY_MS
        } else {
            RISK_AFTER_MS
        };
        let at = now_ms.saturating_add(after);
        self.look_at_ms = Some(at);
        note_look(at);
    }

    /// The instant a look is owed and past `now_ms`.
    pub(crate) fn look_due(&self, now_ms: u64) -> bool {
        self.look_at_ms.is_some_and(|at| at <= now_ms)
    }

    /// The state `status render=` reads from this book: `None` when nothing is
    /// owed.
    pub(crate) fn state(&self) -> Option<EpochVerdict> {
        self.current
            .as_ref()
            .filter(|e| e.had_debt)
            .map(|e| e.verdict)
    }

    /// Forget an epoch whose engine was replaced (a restored engine restarts its
    /// counters); the successor's seeded debt re-opens one.
    pub(crate) fn rebase(&mut self) {
        self.current = None;
        self.look_at_ms = None;
    }

    fn close(&mut self, epoch: Option<Epoch>, verdict: EpochVerdict, by: EpochBy) {
        let Some(mut epoch) = epoch else { return };
        if !epoch.had_debt {
            return;
        }
        epoch.verdict = verdict;
        epoch.by = by;
        self.closed.push_back(epoch);
        if self.closed.len() > EPOCHS_KEPT {
            self.closed.pop_front();
        }
    }
}

/// The end of a superseded repair, the epoch judge's half of the repaint nudge
/// (`RepaintNudge`'s `Report`). A real resize that left the engine elsewhere ended
/// the attempt `superseded` (the program was given a size of its own). If that
/// resize does not last, the epoch comes back to `desync-risk` (rows still owed, the
/// geometry back at its start, the risk wait spent) with its one try spent: the same
/// give-up as a resize that comes back inside its wait. `Some(id)` once; the window
/// raises the digest line (`App::arm_nudges`). A repaint by the program closes the
/// epoch first and nothing is said.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "RepaintNudge",
        action = "Report",
        project = "aterm_gui::nudge_conformance::Rig::header"
    )
)]
fn returned_at_risk(epoch: &mut Epoch) -> Option<u64> {
    (epoch.verdict == EpochVerdict::DesyncRisk && std::mem::take(&mut epoch.superseded))
        .then_some(epoch.id)
}

/// `epoch <e> t=<ms> geom=<start>><now> shift=<+-k> pushed=<k> drew=<0|1>
/// class=<agent|learned|unknown> verdict=<open|desync-risk|unjudged|healed|
/// restored> by=<app|undo|nudge|-> tried=<0|1>`
pub(crate) fn wire_epoch(out: &mut String, epoch: &Epoch) {
    use std::fmt::Write as _;
    let geom = |(rows, cols): (u16, u16)| format!("{cols}x{rows}");
    let _ = writeln!(
        out,
        "epoch {} t={} geom={}>{} shift={:+} pushed={} drew={} class={} verdict={} by={} \
         tried={}",
        epoch.id,
        epoch.t_ms,
        geom(epoch.start),
        geom(epoch.now),
        epoch.shift,
        epoch.pushed,
        u8::from(epoch.drew),
        epoch.class.wire(),
        epoch.verdict.wire(),
        epoch.by.wire(),
        u8::from(epoch.tried),
    );
}
