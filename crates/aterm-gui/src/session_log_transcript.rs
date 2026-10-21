// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! TRANSCRIPT STEPS REACH THE SESSION LOG (phase 3B, plan §2; addendum A13,
//! A20; rulings S1, S2, S9, S12, S16, S23, S27): the glue between the footer
//! resolver's step tap (`aterm_agent::harness::steps`, folded on the
//! resolver thread from the transcript lines it already reads) and the agent
//! run's turn tracker, whose [`aterm_slog::TurnTracker::transcript`] is the
//! one writer of every transcript record (S2). A child module of
//! `session_log`, so it reaches the agent run as the carry module does.
//!
//! - ARMING ([`SessionLog::transcript_arm`]). The resolver arms the tap only
//!   while the log holds an open agent run that reads a transcript (Claude
//!   Code: an exhaustive match on the program, so a new agent answers here),
//!   handing it the run's watermark and floor. A run with no watermark binds
//!   at the file's length (S1): what the file held is history, said once by
//!   the tracker's `source … skipped=` mark, never replayed.
//! - APPLYING ([`apply_batch`]). At most [`CHUNK`] events a timeline hold,
//!   the lock released between chunks (S16): the batch's leading binds
//!   (and a latch's `off`) first, so its counts land on the file they are
//!   of, then the overflow count (the oldest steps after the bind were the
//!   ones dropped), then rows lost, then the rest of the steps in order,
//!   then the screen's open wall judged against the chain's end, and last
//!   the batch's end ([`TxEvent::Flush`]: a loss counted and not yet said
//!   is said once, S23, S27). A batch that finds no run to go to, or a run
//!   that bound no file, disarms the tap, so the next read arms a fresh
//!   one, bound at once.
//! - THE HOLD, MEASURED (T31). Each chunk's hold is timed from the lock
//!   taken to its release, a full chunk's apart from the last's (which
//!   also judges, flushes and takes the wake), and read by `metrics
//!   percentiles` (`tx_chunk_*`, `tx_last_*`).
//! - THE WAKE (S12). The last hold takes the log's owed wake, which rings the
//!   session's watchers under the timeline lock
//!   ([`crate::session_timeline::SessionTimeline::slog_take_wake`]); the
//!   resolver tells the loop only when it owes a settle tick or the clause
//!   switch is on (`claude_footer::LogWake`).
//!
//! Nothing here holds text: a step is ids, a tool's name, class words and
//! numbers (`aterm_agent::harness::steps::Step`), mapped field for field.

use std::sync::Mutex;

use aterm_agent::harness::steps::{
    End, Judge, Step, StepBatch, StepEvent, StepMark, StepState, judge_wall,
};
use aterm_slog::{
    ScreenStep, ShortId, Stamp, TxEvent, TxPos, TxState, TxView, WallJudge, WallKind,
};

use super::SessionLog;
use crate::session_timeline::SessionTimeline;

/// The most transcript events one timeline hold applies (ruling S16): a
/// catch-up read of a long resumed conversation never makes the footer
/// thread a long holder of the lock the composer and the sweep take.
pub(crate) const CHUNK: usize = 32;

/// What arming the tap takes from the log: the floor (wall ms: the
/// adoption, for a carried run) and the watermark the run's join holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TapArm {
    /// Rows older than this are another process's (wall ms).
    pub(crate) floor_ms: u64,
    /// Rows at or below it were applied.
    pub(crate) given: Option<StepMark>,
}

/// What applying a batch did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Applied {
    /// The log's readers are owed a wake (taken in the last hold).
    pub(crate) owed: bool,
    /// The tracker's settle wake (monotonic ms) when the batch moved it.
    pub(crate) settle: Option<u64>,
    /// The tap is to be disarmed: no run takes its steps.
    pub(crate) disarm: bool,
    /// The events applied, and the holds they took.
    pub(crate) events: usize,
    pub(crate) holds: usize,
}

/// Whether `who`'s run reads a transcript the tap folds: Claude Code alone
/// (plan §2.1; Codex rollouts are Phase 5, owner default 4).
const fn reads_transcript(who: aterm_phase::Program) -> bool {
    match who {
        aterm_phase::Program::Claude => true,
        aterm_phase::Program::Codex
        | aterm_phase::Program::Agy
        | aterm_phase::Program::Muse
        | aterm_phase::Program::Gemini
        | aterm_phase::Program::Generic => false,
    }
}

/// A step's state as the tracker takes it.
const fn tx_state(s: StepState) -> TxState {
    match s {
        StepState::On => TxState::On,
        StepState::Partial => TxState::Partial,
    }
}

/// The log's wall kind for the reader's.
fn slog_wall(k: aterm_phase::WallKind) -> Option<WallKind> {
    WallKind::from_name(k.wire())
}

/// The judge's answer for the screen's wall of the log's `kind`: the reader's
/// kind of the same class. An API error's status is not in the log's word,
/// and a 429 is a limit to the harness's classes: the judge confirms when
/// either reading confirms, refutes only when both refute, else waits.
fn judge_screen(kind: WallKind, end: &End, opened_ms: u64) -> Judge {
    use aterm_phase::{ApiCause, WallKind as W};
    let one = |k: W| judge_wall(k, end, opened_ms);
    match kind {
        WallKind::UsageSession => one(W::UsageSession),
        WallKind::UsageWeekly => one(W::UsageWeekly),
        WallKind::ModelBucket => one(W::ModelBucket { consent: false }),
        WallKind::Spend => one(W::Spend),
        WallKind::Context => one(W::Context),
        WallKind::Auth => one(W::Auth),
        WallKind::Overloaded => one(W::Overloaded),
        WallKind::Memory => one(W::Memory),
        WallKind::ApiError => {
            let api = |code| W::ApiError {
                code,
                retryable: true,
                cause: ApiCause::Server,
            };
            match (one(api(None)), one(api(Some(429)))) {
                (Judge::Confirm, _) | (_, Judge::Confirm) => Judge::Confirm,
                (Judge::Refute, Judge::Refute) => Judge::Refute,
                _ => Judge::Wait,
            }
        }
    }
}

/// Where a step was read, as the tracker takes it.
const fn tx_pos(step: &Step) -> TxPos {
    TxPos {
        fp: step.pos.fp,
        id: step.pos.id,
        off: step.pos.off,
        wall_ms: step.pos.at_ms,
    }
}

/// How the log reads a batch's run (one hold's look).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ready {
    /// No open run that reads a transcript.
    NoRun,
    /// A run whose join bound no file yet.
    Unbound,
    /// A run bound to a file.
    Bound,
}

impl SessionLog {
    /// THE TAP'S ARMING: the open agent run that reads a transcript, its
    /// join's floor and watermark; `None` while the log holds none (the tap
    /// stays unarmed, and nothing is folded for it).
    pub(crate) fn transcript_arm(&self) -> Option<TapArm> {
        let a = self
            .agent
            .as_ref()
            .filter(|a| self.log.is_open(a.node) && reads_transcript(a.who))?;
        let view = a.tracker.tx_view();
        Some(TapArm {
            floor_ms: view.floor_ms,
            given: view.through.map(|m| StepMark {
                fp: m.fp,
                id: m.id,
                off: m.off,
            }),
        })
    }

    /// What the transcript join of the run in front holds (tests, `status`).
    pub(crate) fn transcript_view(&self) -> Option<TxView> {
        self.agent
            .as_ref()
            .filter(|a| self.log.is_open(a.node) && reads_transcript(a.who))
            .map(|a| a.tracker.tx_view())
    }

    fn transcript_ready(&mut self) -> Ready {
        self.resync();
        match self.transcript_view() {
            None => Ready::NoRun,
            Some(v) if v.through.is_none() => Ready::Unbound,
            Some(_) => Ready::Bound,
        }
    }

    /// One transcript fact for the run in front, applied at `now`: the
    /// tracker's own entry point, typed field for field.
    fn transcript_event(&mut self, ev: TxEvent<'_>, pos: TxPos, state: TxState, now: Stamp) {
        self.reconcile_person(now);
        let Self { log, agent, .. } = self;
        if let Some(a) = agent
            .as_mut()
            .filter(|a| log.is_open(a.node) && reads_transcript(a.who))
        {
            let _ = a.tracker.transcript(log, ev, pos, state, now);
        }
    }

    /// One step of the tap.
    fn transcript_step(&mut self, step: &Step, now: Stamp) {
        let pos = tx_pos(step);
        let state = tx_state(step.state);
        let id = |s: &aterm_agent::harness::steps::StepId| ShortId::new(s.as_str());
        match step.ev {
            StepEvent::Bind { len } => self.transcript_event(
                TxEvent::Bind {
                    len,
                    floor_ms: step.pos.at_ms,
                },
                TxPos { off: len, ..pos },
                state,
                now,
            ),
            StepEvent::Prompt { vref } => {
                if let Some(vref) = id(&vref) {
                    self.transcript_event(TxEvent::Prompt { vref }, pos, state, now);
                }
            }
            StepEvent::Interrupt => self.transcript_event(TxEvent::Interrupt, pos, state, now),
            StepEvent::ToolUse {
                id: call,
                tool,
                cls,
                subagent,
            } => {
                if let Some(call) = id(&call) {
                    let ev = TxEvent::ToolUse {
                        id: call,
                        tool: tool.as_str(),
                        cls,
                        subagent,
                    };
                    self.transcript_event(ev, pos, state, now);
                }
            }
            StepEvent::ToolResult { id: call, error } => {
                if let Some(call) = id(&call) {
                    self.transcript_event(TxEvent::ToolResult { id: call, error }, pos, state, now);
                }
            }
            StepEvent::TurnEnd {
                vdur_ms,
                tok_in,
                tok_out,
            } => self.transcript_event(
                TxEvent::TurnEnd {
                    vdur_ms,
                    tok_in,
                    tok_out,
                },
                pos,
                state,
                now,
            ),
            StepEvent::EndWall { kind } => {
                if let Some(kind) = slog_wall(kind) {
                    self.transcript_event(TxEvent::EndWall { kind }, pos, state, now);
                }
            }
            StepEvent::Compact => self.transcript_event(TxEvent::Compact, pos, state, now),
            StepEvent::Model { model, effort, ver } => {
                let ev = TxEvent::Model {
                    model: Some(model.as_str()),
                    effort: effort.as_ref().map(|e| e.as_str()),
                    ver: ver.as_ref().map(|v| v.as_str()),
                };
                self.transcript_event(ev, pos, state, now);
            }
            // The fold latched off for the path: the source says `off`
            // once, and nothing more applies from it.
            StepEvent::Off { .. } => {
                self.transcript_event(TxEvent::Lost { n: 0 }, pos, TxState::Off, now);
            }
        }
    }

    /// A count of the glue's (`Lost`, `Overflow`, the batch's end) or a
    /// judge's answer: no row of the file, so dated at `now` at the join's
    /// own watermark.
    fn transcript_count(&mut self, ev: TxEvent<'_>, now: Stamp) {
        let Some(view) = self.transcript_view() else {
            return;
        };
        let (Some(mark), Some(state)) = (view.through, view.state) else {
            return;
        };
        let pos = TxPos {
            fp: mark.fp,
            id: mark.id,
            off: mark.off,
            wall_ms: now.wall_ms,
        };
        self.transcript_event(ev, pos, state, now);
    }

    /// THE WALL JUDGE for the screen's open wall step, against the chain's
    /// `end` (plan §2.6; the harness's own rule): its turn's open, or, for
    /// a wall the screen left on the run after its turn closed, that turn's.
    fn transcript_judge(&mut self, end: &End, now: Stamp) {
        let Some(a) = self
            .agent
            .as_ref()
            .filter(|a| self.log.is_open(a.node) && reads_transcript(a.who))
        else {
            return;
        };
        let view = a.tracker.view(now.mono_ms);
        let Some((node, ScreenStep::Wall(kind), under_turn)) = view.step else {
            return;
        };
        let step_t = self.log.open_entry(node).map_or(now.wall_ms, |e| e.t);
        let opened_ms = if under_turn {
            view.turn
                .and_then(|t| self.log.open_entry(t))
                .map_or(step_t, |e| e.t)
        } else {
            self.log.state().last[1]
                .as_ref()
                .and_then(|c| c.opened_t)
                .filter(|&t| t <= step_t)
                .unwrap_or(step_t)
        };
        let judge = match judge_screen(kind, end, opened_ms) {
            Judge::Confirm => WallJudge::Confirm,
            Judge::Refute => WallJudge::Refute,
            Judge::Wait => return,
        };
        self.transcript_count(TxEvent::Wall { kind, judge }, now);
    }
}

/// One thing a batch applies, in order ([`apply_batch`]).
#[derive(Clone, Copy, Debug)]
enum Item<'a> {
    /// A step of the tap's.
    Step(&'a Step),
    /// A count of the glue's.
    Count(TxEvent<'static>),
}

/// The order a batch applies in (module docs): its leading binds and
/// latch, then its counts — the steps an overflow dropped were the oldest
/// after the bind (a bind is never dropped), so the join is desynced
/// before the newest apply, on the file they are of — then its other
/// steps.
fn ordered(batch: &StepBatch) -> impl Iterator<Item = Item<'_>> {
    let lead = batch
        .steps
        .iter()
        .take_while(|s| matches!(s.ev, StepEvent::Bind { .. } | StepEvent::Off { .. }))
        .count();
    let (pinned, rest) = batch.steps.split_at(lead);
    let counts = [
        (batch.overflow > 0).then_some(TxEvent::Overflow { n: batch.overflow }),
        (batch.lost > 0).then_some(TxEvent::Lost { n: batch.lost }),
    ];
    pinned
        .iter()
        .map(Item::Step)
        .chain(counts.into_iter().flatten().map(Item::Count))
        .chain(rest.iter().map(Item::Step))
}

/// Which hold of [`apply_batch`] a hold-time sample is of (ruling T31;
/// `metrics percentiles`' `tx_chunk_*` and `tx_last_*`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Hold {
    /// A full chunk: [`CHUNK`] events, with more of the batch to follow.
    Full,
    /// The batch's last hold: what was left of it (at most [`CHUNK`]
    /// events; none for a read that found no new rows, whose one hold is
    /// still taken), then the wall's judge, the flush and the owed wake.
    Last,
}

/// APPLY `batch` to `timeline`'s session log (module docs): at most
/// [`CHUNK`] events a hold, the lock released between chunks, the wall
/// judged, the batch's unsaid loss said and the owed wake taken in the
/// last. The caller notifies once every hold is released
/// (`claude_footer::LogWake::after`). Each hold that applied a chunk is
/// timed, from the lock taken to its release, into `metrics percentiles`
/// (T31: the hold the composer and the sweep can wait behind).
pub(crate) fn apply_batch(
    timeline: &Mutex<SessionTimeline>,
    batch: &StepBatch,
    bound: bool,
) -> Applied {
    apply_batch_timed(
        timeline,
        batch,
        bound,
        &mut crate::metrics::now_ns,
        &mut crate::metrics::note_tx_hold,
    )
}

/// [`apply_batch`] on the clock `now_ns` (monotonic nanoseconds), each
/// chunk's hold handed to `book` with its kind ([`Hold`]). A batch no run
/// takes applies no chunk and books nothing.
pub(crate) fn apply_batch_timed(
    timeline: &Mutex<SessionTimeline>,
    batch: &StepBatch,
    bound: bool,
    now_ns: &mut dyn FnMut() -> u64,
    book: &mut dyn FnMut(Hold, u64),
) -> Applied {
    let mut applied = Applied::default();
    let mut items = ordered(batch).peekable();
    let mut before = None;
    loop {
        let mut g = timeline.lock().unwrap_or_else(|p| p.into_inner());
        let taken = now_ns();
        let slog = g.slog_mut();
        let now = slog.now();
        if applied.holds == 0 {
            before = slog.wake();
            let ready = slog.transcript_ready();
            let binds = batch
                .steps
                .iter()
                .any(|s| matches!(s.ev, StepEvent::Bind { .. }));
            // No run takes the steps, or a run that bound no file meets a
            // tap bound for an earlier one: the tap goes, and the next arm
            // binds afresh (S1). A tap with no file yet waits for one.
            if ready == Ready::NoRun || (ready == Ready::Unbound && !binds) {
                applied.holds = 1;
                applied.disarm = ready == Ready::NoRun || bound;
                applied.owed = g.slog_take_wake();
                return applied;
            }
        }
        applied.holds += 1;
        for item in items.by_ref().take(CHUNK) {
            match item {
                Item::Step(step) => slog.transcript_step(step, now),
                Item::Count(ev) => slog.transcript_count(ev, now),
            }
            applied.events += 1;
        }
        if items.peek().is_some() {
            book(Hold::Full, now_ns().saturating_sub(taken));
            // Released between chunks (S16).
            continue;
        }
        slog.transcript_judge(&batch.end, now);
        slog.transcript_count(TxEvent::Flush, now);
        let after = slog.wake();
        applied.settle = after.filter(|_| after != before);
        applied.owed = g.slog_take_wake();
        book(Hold::Last, now_ns().saturating_sub(taken));
        return applied;
    }
}

#[cfg(test)]
#[path = "session_log_transcript_tests.rs"]
mod tests;
