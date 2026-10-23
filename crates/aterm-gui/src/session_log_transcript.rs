// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! TRANSCRIPT STEPS REACH THE SESSION LOG (phase 3B, plan §2; addendum A13, A20): the glue between the footer resolver's step tap (`aterm_agent::harness::steps`, folded on the resolver thread from the transcript lines it already reads) and the agent run's turn tracker, whose [`aterm_slog::TurnTracker::transcript`] is the one writer of every transcript record (S2).

use std::sync::Mutex;

use aterm_agent::harness::steps::{
    End, Judge, Step, StepBatch, StepEvent, StepMark, StepState, judge_wall,
};
use aterm_slog::{
    ScreenStep, ShortId, Stamp, TxEvent, TxPos, TxState, TxView, WallJudge, WallKind,
};

use super::SessionLog;
use crate::session_timeline::SessionTimeline;

/// The most transcript events one timeline hold applies: a catch-up read of a long resumed conversation never makes the footer thread a long holder of the lock the composer and the sweep take.
pub(crate) const CHUNK: usize = 32;

/// What arming the tap takes from the log: the floor (wall ms: the latest of the join's floor, the adoption for a carried run, and the run's open in this log, so the tap's fold takes a row as live exactly when the tracker admits it) and the watermark the run's join holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TapArm {
    /// The exact run this arm belongs to, including its log epoch.
    pub(crate) run: TapRun,
    /// Rows older than this are another process's (wall ms).
    pub(crate) floor_ms: u64,
    /// Rows at or below it were applied.
    pub(crate) given: Option<StepMark>,
}

/// An open agent run's identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TapRun {
    epoch: u32,
    node: aterm_slog::Node,
    pgid: i32,
}

impl TapRun {
    /// A proven claimant leads the foreground run it reads.
    pub(crate) fn claimed_by(self, pid: u32) -> bool {
        i64::from(self.pgid) == i64::from(pid)
    }
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

/// WHAT THE ROLLOUT TAP ANSWERED (`status rollout= rollout_asks=`; session log 6D-3b's tap, its fence B5c): how many asks the session's rollout tap answered, each counted in a hold taken AFTER that ask's batch was applied (`crate::codex_rollout_tap`), and the last answer's word: `bound`, or why it bound nothing (a `codex_tap::Refusal`'s word, `unmeasured` among them; `stale` when the captured claim, record, run or sampled process image moved; `no-thread` if the tap cannot be made), or `stopped` once the sweep dropped the tap (the claimant gone from the front, the sweep off) until it asks again.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct TapAnswers {
    asks: u64,
    word: Option<&'static str>,
    /// The log's last seq when the count last moved: at or past every
    /// record the counted ask wrote, which the tap's tests read
    /// ([`SessionLog::rollout_counted_at`]).
    #[cfg(test)]
    counted_at: u64,
}

impl SessionLog {
    /// One ask the rollout tap answered, with its word ([`TapAnswers`]).
    pub(crate) fn rollout_answered(&mut self, word: &'static str) {
        self.rollout.asks = self.rollout.asks.saturating_add(1);
        self.rollout.word = Some(word);
        #[cfg(test)]
        {
            self.rollout.counted_at = self.log.last_seq();
        }
    }

    /// The sweep dropped the session's tap ([`TapAnswers`]): a session the tap answered says `stopped`, its count kept; one never asked stays `-`.
    pub(crate) fn rollout_stopped(&mut self) {
        if self.rollout.word.is_some() {
            self.rollout.word = Some("stopped");
        }
    }

    /// The log's last seq when the tap's count last moved (0 before it ever moved): the order of an ask's records and its count, as the tap's thread runs them.
    #[cfg(test)]
    pub(crate) fn rollout_counted_at(&self) -> u64 {
        self.rollout.counted_at
    }

    /// `status`'s `rollout=<word|-> rollout_asks=<n>` ([`TapAnswers`]): `-` and `0` while the tap was never asked for this session.
    pub(crate) fn rollout_fields(&self) -> String {
        format!(
            "rollout={} rollout_asks={}",
            self.rollout.word.unwrap_or("-"),
            self.rollout.asks
        )
    }
}

/// Which tap's steps a batch carries (module docs).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Source {
    /// Claude Code's transcript, by the footer resolver's tap (plan §2.1).
    Transcript,
    /// A codex-alab's rollout, by the rollout tap.
    Rollout,
}

/// The tap that reads `who`'s run, if any: Claude Code's transcript and
/// Codex's rollout (only a codex-alab's is ever bound: the rollout tap's
/// gate); no other agent's.
const fn reader_of(who: aterm_phase::Program) -> Option<Source> {
    match who {
        aterm_phase::Program::Claude => Some(Source::Transcript),
        aterm_phase::Program::Codex => Some(Source::Rollout),
        aterm_phase::Program::Agy
        | aterm_phase::Program::Muse
        | aterm_phase::Program::Gemini
        | aterm_phase::Program::Generic => None,
    }
}

/// Whether `source`'s tap reads `who`'s run.
fn reads(who: aterm_phase::Program, source: Source) -> bool {
    reader_of(who) == Some(source)
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
/// kind of the same class.
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
    /// THE TAP'S ARMING: the open agent run `source`'s tap reads, its join's floor and watermark; `None` while the log holds none (the tap stays unarmed, and nothing is folded for it).
    pub(crate) fn transcript_arm(&self, source: Source) -> Option<TapArm> {
        let a = self
            .agent
            .as_ref()
            .filter(|a| self.log.is_open(a.node) && reads(a.who, source))?;
        let view = a.tracker.tx_view();
        let opened = self.log.open_entry(a.node).map_or(0, |e| e.t);
        Some(TapArm {
            run: TapRun {
                epoch: self.log.ep(),
                node: a.node,
                pgid: a.pgid,
            },
            floor_ms: view.floor_ms.max(opened),
            given: view.through.map(|m| StepMark {
                fp: m.fp,
                id: m.id,
                off: m.off,
            }),
        })
    }

    /// What the transcript join of the run in front holds, when `source`'s tap reads it.
    pub(crate) fn transcript_view(&self, source: Source) -> Option<TxView> {
        self.agent
            .as_ref()
            .filter(|a| self.log.is_open(a.node) && reads(a.who, source))
            .map(|a| a.tracker.tx_view())
    }

    fn transcript_ready(&mut self, source: Source) -> Ready {
        self.resync();
        match self.transcript_view(source) {
            None => Ready::NoRun,
            Some(v) if v.through.is_none() => Ready::Unbound,
            Some(_) => Ready::Bound,
        }
    }

    /// One transcript fact of `source`'s for the run in front, applied at
    /// `now`: the tracker's own entry point, typed field for field.
    fn transcript_event(
        &mut self,
        source: Source,
        ev: TxEvent<'_>,
        pos: TxPos,
        state: TxState,
        now: Stamp,
    ) {
        self.reconcile_person(now);
        let Self { log, agent, .. } = self;
        if let Some(a) = agent
            .as_mut()
            .filter(|a| log.is_open(a.node) && reads(a.who, source))
        {
            let _ = a.tracker.transcript(log, ev, pos, state, now);
        }
    }

    /// One step of `source`'s tap.
    fn transcript_step(&mut self, source: Source, step: &Step, now: Stamp) {
        let pos = tx_pos(step);
        let state = tx_state(step.state);
        let id = |s: &aterm_agent::harness::steps::StepId| ShortId::new(s.as_str());
        match step.ev {
            StepEvent::Bind { len } => self.transcript_event(
                source,
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
                    self.transcript_event(source, TxEvent::Prompt { vref }, pos, state, now);
                }
            }
            StepEvent::Interrupt => {
                self.transcript_event(source, TxEvent::Interrupt, pos, state, now)
            }
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
                    self.transcript_event(source, ev, pos, state, now);
                }
            }
            StepEvent::ToolResult { id: call, error } => {
                if let Some(call) = id(&call) {
                    self.transcript_event(
                        source,
                        TxEvent::ToolResult { id: call, error },
                        pos,
                        state,
                        now,
                    );
                }
            }
            StepEvent::TurnEnd {
                vdur_ms,
                tok_in,
                tok_out,
            } => self.transcript_event(
                source,
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
                    self.transcript_event(source, TxEvent::EndWall { kind }, pos, state, now);
                }
            }
            StepEvent::Compact => self.transcript_event(source, TxEvent::Compact, pos, state, now),
            StepEvent::Model { model, effort, ver } => {
                let ev = TxEvent::Model {
                    model: Some(model.as_str()),
                    effort: effort.as_ref().map(|e| e.as_str()),
                    ver: ver.as_ref().map(|v| v.as_str()),
                };
                self.transcript_event(source, ev, pos, state, now);
            }
            StepEvent::Off { .. } => {
                self.transcript_event(source, TxEvent::Lost { n: 0 }, pos, TxState::Off, now);
            }
        }
    }

    /// A count of the glue's (`Lost`, `Overflow`, the batch's end) or a
    /// judge's answer: no row of the file, so dated at `now` at the join's
    /// own watermark.
    fn transcript_count(&mut self, source: Source, ev: TxEvent<'_>, now: Stamp) {
        let Some(view) = self.transcript_view(source) else {
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
        self.transcript_event(source, ev, pos, state, now);
    }

    /// THE WALL JUDGE for the screen's open wall step, against the chain's
    /// `end` (plan §2.6; the harness's own rule): its turn's open, or, for
    /// a wall the screen left on the run after its turn closed, that turn's.
    fn transcript_judge(&mut self, end: &End, now: Stamp) {
        let Some(a) = self
            .agent
            .as_ref()
            .filter(|a| self.log.is_open(a.node) && reads(a.who, Source::Transcript))
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
        self.transcript_count(Source::Transcript, TxEvent::Wall { kind, judge }, now);
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

/// Which hold of [`apply_batch`] a hold-time sample is of ( `metrics percentiles`' `tx_chunk_*` and `tx_last_*`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Hold {
    /// A full chunk: [`CHUNK`] events, with more of the batch to follow.
    Full,
    /// The batch's last hold: what was left of it (at most [`CHUNK`] events; none for a read that found no new rows, whose one hold is still taken), then the wall's judge, the flush and the owed wake.
    Last,
}

/// APPLY `source`'s `batch` to `timeline`'s session log (module docs): at most [`CHUNK`] events a hold, the lock released between chunks, the wall judged, the batch's unsaid loss said and the owed wake taken in the last.
pub(crate) fn apply_batch(
    timeline: &Mutex<SessionTimeline>,
    source: Source,
    batch: &StepBatch,
    bound: bool,
) -> Applied {
    apply_batch_timed(
        timeline,
        source,
        batch,
        bound,
        &mut crate::metrics::now_ns,
        &mut crate::metrics::note_tx_hold,
    )
}

/// [`apply_batch`] on the clock `now_ns` (monotonic nanoseconds), each chunk's hold handed to `book` with its kind ([`Hold`]).
pub(crate) fn apply_batch_timed(
    timeline: &Mutex<SessionTimeline>,
    source: Source,
    batch: &StepBatch,
    bound: bool,
    now_ns: &mut dyn FnMut() -> u64,
    book: &mut dyn FnMut(Hold, u64),
) -> Applied {
    apply_batch_guarded(timeline, source, batch, bound, None, now_ns, book)
}

/// A rollout batch remains owned by the queued claim, record and run at EVERY chunk, including the first hold after its off-lock file read.
pub(crate) fn apply_rollout_batch(
    ctx: &crate::SessionCtx,
    target: &crate::self_claim::RolloutTarget,
    run: TapRun,
    batch: &StepBatch,
    bound: bool,
) -> Applied {
    apply_batch_guarded(
        &ctx.timeline,
        Source::Rollout,
        batch,
        bound,
        Some((ctx, target, run)),
        &mut crate::metrics::now_ns,
        &mut crate::metrics::note_tx_hold,
    )
}

fn apply_batch_guarded(
    timeline: &Mutex<SessionTimeline>,
    source: Source,
    batch: &StepBatch,
    bound: bool,
    gate: Option<(
        &crate::SessionCtx,
        &crate::self_claim::RolloutTarget,
        TapRun,
    )>,
    now_ns: &mut dyn FnMut() -> u64,
    book: &mut dyn FnMut(Hold, u64),
) -> Applied {
    let mut applied = Applied::default();
    let mut items = ordered(batch).peekable();
    let mut before = None;
    loop {
        let _claim = gate.and_then(|(ctx, target, _)| crate::self_claim::hold_rollout(ctx, target));
        let claim_moved = gate.is_some() && _claim.is_none();
        let mut g = timeline.lock().unwrap_or_else(|p| p.into_inner());
        let taken = now_ns();
        let slog = g.slog_mut();
        if claim_moved
            || gate.is_some_and(|(_, _, run)| {
                slog.transcript_arm(source).is_none_or(|arm| arm.run != run)
            })
        {
            applied.disarm = true;
            if applied.holds > 0 {
                applied.settle = slog.wake();
                applied.owed = g.slog_take_wake();
            }
            return applied;
        }
        let now = slog.now();
        if applied.holds == 0 {
            before = slog.wake();
            let ready = slog.transcript_ready(source);
            let binds = batch
                .steps
                .iter()
                .any(|s| matches!(s.ev, StepEvent::Bind { .. }));
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
                Item::Step(step) => slog.transcript_step(source, step, now),
                Item::Count(ev) => slog.transcript_count(source, ev, now),
            }
            applied.events += 1;
        }
        if items.peek().is_some() {
            book(Hold::Full, now_ns().saturating_sub(taken));
            continue;
        }
        if source == Source::Transcript {
            slog.transcript_judge(&batch.end, now);
        }
        slog.transcript_count(source, TxEvent::Flush, now);
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
