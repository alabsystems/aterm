// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Render-debt resize epoch judge for `status render=` and `resizes`.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};

use aterm_core::terminal::ResizeReport;

use crate::resize_ledger::{EngineProbe, RISK_AFTER_MS, Transition};

/// Maximum number of closed epochs retained for `resizes`.
pub(crate) const EPOCHS_KEPT: usize = 8;
/// Delay before retrying an epoch look when the terminal lock is contended.
pub(crate) const LOOK_RETRY_MS: u64 = 100;
/// Maximum lock-contention retries before deferring to the next risk window.
pub(crate) const LOOK_RETRIES: u32 = 20;

/// Classification of the program running in a session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProgramClass {
    /// Identified as Claude Code.
    Agent,
    /// Observed clearing its screen on resize.
    Learned,
    /// Neither identified as an agent nor observed clearing on resize.
    Unknown,
}

impl ProgramClass {
    /// Returns the wire token (`class=`).
    pub(crate) const fn wire(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Learned => "learned",
            Self::Unknown => "unknown",
        }
    }

    /// Returns whether an at-risk epoch of this class reports `desync-risk`.
    pub(crate) const fn judged(self) -> bool {
        !matches!(self, Self::Unknown)
    }
}

/// Verdict state of a resize epoch (`verdict=`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EpochVerdict {
    /// Debt is owed and the program may still respond.
    Open,
    /// At risk of render desync for a judged program class.
    DesyncRisk,
    /// At risk, for a program class that has not shown it clears on resize.
    Unjudged,
    /// Repainted by a full clear or screen switch.
    Healed,
    /// Render debt returned to zero via engine undo.
    Restored,
}

impl EpochVerdict {
    /// Returns the wire token.
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

/// Actor that closed a resize epoch (`by=`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EpochBy {
    /// Still open.
    None,
    /// Closed by an application repaint.
    App,
    /// Closed by engine undo.
    Undo,
    /// Closed after a repaint nudge.
    Nudge,
}

impl EpochBy {
    /// Returns the wire token.
    pub(crate) const fn wire(self) -> &'static str {
        match self {
            Self::None => "-",
            Self::App => "app",
            Self::Undo => "undo",
            Self::Nudge => "nudge",
        }
    }
}

/// Single alternate-screen resize epoch record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Epoch {
    pub(crate) id: u64,
    /// Timestamp of the first resize in the epoch (timeline ms).
    pub(crate) t_ms: u64,
    /// Timestamp of the most recent resize in the epoch.
    pub(crate) last_resize_ms: u64,
    /// `(rows, cols)` before the first resize of the epoch.
    pub(crate) start: (u16, u16),
    /// Most recently observed `(rows, cols)`.
    pub(crate) now: (u16, u16),
    /// Most recently observed engine render debt (`shift`, `pushed`, `cut`).
    pub(crate) shift: i32,
    pub(crate) pushed: i32,
    pub(crate) cut: u16,
    /// Whether the program drew content since the resize.
    pub(crate) drew: bool,
    /// The content sequence the screen shows while the program has drawn
    /// nothing since: the last resize's.
    quiet_seq: u64,
    /// The engine's heal counters when the epoch opened: either advancing past
    /// them is a repaint from scratch.
    full_clears: u64,
    screen_replaced: u64,
    /// The engine owed something at some reading.
    had_debt: bool,
    pub(crate) class: ProgramClass,
    pub(crate) verdict: EpochVerdict,
    pub(crate) by: EpochBy,
    /// Whether a repaint nudge was attempted for this epoch.
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
    const fn open(id: u64, report: &ResizeReport, now_ms: u64) -> Self {
        Self {
            id,
            t_ms: now_ms,
            last_resize_ms: now_ms,
            start: report.from,
            now: report.to,
            shift: 0,
            pushed: 0,
            cut: 0,
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

    const fn healed_by(&self, full_clears: u64, screen_replaced: u64) -> bool {
        full_clears > self.full_clears || screen_replaced > self.screen_replaced
    }

    /// Returns whether this epoch owed render debt and should appear in `resizes`.
    pub(crate) const fn listed(&self) -> bool {
        self.had_debt
    }
}

/// The soonest instant any ledger wants an epoch look (timeline ms), `u64::MAX` for
/// none.
static NEXT_LOOK_MS: AtomicU64 = AtomicU64::new(u64::MAX);

/// Records that a ledger needs an epoch look at `at_ms`.
pub(crate) fn note_look(at_ms: u64) {
    NEXT_LOOK_MS.fetch_min(at_ms, Ordering::AcqRel);
}

/// Returns the soonest scheduled epoch look timestamp (timeline ms).
pub(crate) fn next_look_ms() -> Option<u64> {
    match NEXT_LOOK_MS.load(Ordering::Acquire) {
        u64::MAX => None,
        at => Some(at),
    }
}

/// Resets the global next-look timestamp before the event loop walks sessions.
pub(crate) fn take_looks() {
    NEXT_LOOK_MS.store(u64::MAX, Ordering::Release);
}

/// Bumped whenever an epoch of the agent class is at `desync-risk` and untried, or a
/// superseded repair's epoch is back at risk ([`returned_at_risk`]): the event loop's
/// cue to walk the sessions, arm the repaint nudge (`crate::app_nudge`) and raise
/// what is owed.
static NUDGE_DUE: AtomicU64 = AtomicU64::new(0);

/// Signals that an agent-class epoch is due for a repaint nudge check.
pub(crate) fn note_nudge_due() {
    NUDGE_DUE.fetch_add(1, Ordering::AcqRel);
}

/// Returns the current nudge-due generation counter.
pub(crate) fn nudge_due_generation() -> u64 {
    NUDGE_DUE.load(Ordering::Acquire)
}

/// Snapshot of an open at-risk epoch for the repaint nudge controller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct NudgeView {
    /// Epoch identifier.
    pub(crate) epoch: u64,
    /// Vertical row shift owed.
    pub(crate) shift: i32,
    /// Rows pushed off the bottom.
    pub(crate) pushed: i32,
    /// Drawn columns cut.
    pub(crate) cut: u16,
    /// Whether the program drew since the resize.
    pub(crate) drew: bool,
}

/// Per-session book of open and recent resize epochs.
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
    /// Updates whether the presence classifier identifies Claude Code.
    pub(crate) const fn set_agent(&mut self, agent: bool) {
        self.agent = agent;
    }

    /// Records that this session cleared its screen on resize.
    pub(crate) const fn learn(&mut self) {
        self.learned = true;
    }

    /// Returns the session's current [`ProgramClass`].
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

    /// Returns [`NudgeView`] when the open epoch is untried `desync-risk` for an agent.
    pub(crate) fn nudge_view(&self) -> Option<NudgeView> {
        let epoch = self.current.as_ref()?;
        (epoch.verdict == EpochVerdict::DesyncRisk
            && epoch.class == ProgramClass::Agent
            && !epoch.tried)
            .then_some(NudgeView {
                epoch: epoch.id,
                shift: epoch.shift,
                pushed: epoch.pushed,
                cut: epoch.cut,
                drew: epoch.drew,
            })
    }

    /// Returns whether epoch `id` is still open and potentially desynced.
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

    /// Marks epoch `id` as having had a repaint nudge attempted.
    pub(crate) fn note_tried(&mut self, id: u64) {
        if let Some(epoch) = self.current.as_mut().filter(|e| e.id == id) {
            epoch.tried = true;
        }
    }

    /// Marks epoch `id`'s repair attempt as superseded by a real resize.
    pub(crate) fn note_superseded(&mut self, id: u64) {
        if let Some(epoch) = self.current.as_mut().filter(|e| e.id == id) {
            epoch.superseded = true;
        }
    }

    /// Takes the epoch ID whose superseded repair returned to `desync-risk`.
    pub(crate) const fn take_returned(&mut self) -> Option<u64> {
        self.returned.take()
    }

    /// Updates the repaint nudge phase and attempt count for the `resizes` header.
    pub(crate) const fn set_nudge(&mut self, phase: &'static str, nudged: u32) {
        self.nudge_phase = phase;
        self.nudged = nudged;
    }

    /// Formats `nudge=<phase> nudged=<n>`.
    pub(crate) fn nudge_wire(&self) -> String {
        let phase = if self.nudge_phase.is_empty() {
            "idle"
        } else {
            self.nudge_phase
        };
        format!("nudge={phase} nudged={}", self.nudged)
    }

    /// Returns the currently open epoch, if any.
    #[cfg(test)]
    pub(crate) fn current(&self) -> Option<&Epoch> {
        self.current.as_ref()
    }

    /// Iterates over closed and open epochs that owed render debt, oldest first.
    pub(crate) fn listed(&self) -> impl Iterator<Item = &Epoch> {
        self.closed
            .iter()
            .chain(self.current.iter())
            .filter(|e| e.listed())
    }

    /// Returns when the next epoch look is due (timeline ms).
    pub(crate) const fn look_at_ms(&self) -> Option<u64> {
        self.look_at_ms
    }

    /// Drains pending `aterm.log` lines emitted by epoch judgements.
    pub(crate) fn take_logs(&mut self) -> Vec<String> {
        std::mem::take(&mut self.logs)
    }

    /// Records a resize event at `now_ms`.
    pub(crate) fn note_resize(&mut self, report: &ResizeReport, now_ms: u64) {
        if !report.alt || report.reflowed {
            return;
        }
        if let Some(epoch) = self.current.as_mut() {
            if epoch.healed_by(report.full_clears, report.screen_replaced) {
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

    /// Judges the current epoch against a terminal render-debt reading.
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
        let debt = probe.debt;
        if self.current.is_none() && debt.owed() && probe.geom.0 > 0 {
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
        if !debt.owed() {
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
        epoch.shift = debt.shift;
        epoch.pushed = debt.pushed;
        epoch.cut = debt.cut;
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
                    "render epoch {} {}: class={} geom={}x{} shift={:+} pushed={} cut={} drew={}",
                    epoch.id,
                    epoch.verdict.wire(),
                    class.wire(),
                    epoch.start.1,
                    epoch.start.0,
                    epoch.shift,
                    epoch.pushed,
                    epoch.cut,
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
                    cut: epoch.cut,
                    drew: epoch.drew,
                    class,
                    at_ms: epoch.t_ms,
                });
            }
            self.look_at_ms = None;
        } else {
            epoch.verdict = EpochVerdict::Open;
            self.look_at_ms = back.then_some(epoch.last_resize_ms.saturating_add(RISK_AFTER_MS));
        }
        if let Some(at) = self.look_at_ms {
            note_look(at.max(now_ms.saturating_add(1)));
        }
    }

    /// Schedules a bounded retry when an epoch look finds the terminal locked.
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

    /// Returns whether an epoch look is due at `now_ms`.
    pub(crate) fn look_due(&self, now_ms: u64) -> bool {
        self.look_at_ms.is_some_and(|at| at <= now_ms)
    }

    /// Returns the current [`EpochVerdict`] for `status render=`.
    pub(crate) fn state(&self) -> Option<EpochVerdict> {
        self.current
            .as_ref()
            .filter(|e| e.had_debt)
            .map(|e| e.verdict)
    }

    /// Clears the open epoch after the terminal engine is replaced.
    pub(crate) const fn rebase(&mut self) {
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
/// (`RepaintNudge`'s `Report`).
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

/// Appends the wire representation of `epoch` to `out`.
pub(crate) fn wire_epoch(out: &mut String, epoch: &Epoch) {
    use std::fmt::Write as _;
    let geom = |(rows, cols): (u16, u16)| format!("{cols}x{rows}");
    let _ = writeln!(
        out,
        "epoch {} t={} geom={}>{} shift={:+} pushed={} cut={} drew={} class={} verdict={} \
         by={} tried={}",
        epoch.id,
        epoch.t_ms,
        geom(epoch.start),
        geom(epoch.now),
        epoch.shift,
        epoch.pushed,
        epoch.cut,
        u8::from(epoch.drew),
        epoch.class.wire(),
        epoch.verdict.wire(),
        epoch.by.wire(),
        u8::from(epoch.tried),
    );
}
