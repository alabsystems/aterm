// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! TRANSCRIPT STEPS REACH THE SESSION LOG (phase 3B, plan §2; addendum A13, A20): the glue between the footer resolver's step tap (`aterm_agent::harness::steps`, folded on the resolver thread from the transcript lines it already reads) and the agent run's turn tracker, whose [`aterm_slog::TurnTracker::transcript`] is the one writer of every transcript record (S2).

use std::sync::Mutex;

use aterm_agent::harness::codex_tap::Refusal;
use aterm_agent::harness::steps::{
    End, Judge, Step, StepBatch, StepEvent, StepMark, StepState, judge_wall,
};
use aterm_slog::{
    ATTRIBUTION_MS, Key, Kind, Log, Node, Op, ScreenStep, ShortId, Stamp, TxEvent, TxPos, TxState,
    TxView, Val, WallJudge, WallKind,
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
    /// The rollout's process (a proven claimant, or a released one read only) leads the
    /// foreground run it reads.
    pub(crate) fn led_by(self, pid: u32) -> bool {
        i64::from(self.pgid) == i64::from(pid)
    }
}

/// What applying a batch did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Applied {
    /// The log's readers are owed a wake (taken in the last hold).
    pub(crate) owed: bool,
    /// The log's wake ([`SessionLog::wake`], monotonic ms: the tracker's settle or a harness
    /// turn's pair wake) when the batch moved it.
    pub(crate) settle: Option<u64>,
    /// The batch moved the read wait of its run's harness pairing ([`SessionLog::harness_wait`]):
    /// the loop hands it to the rollout tap's ask ([`crate::codex_rollout_tap::pair_wait`]).
    pub(crate) waits: bool,
    /// The tap is to be disarmed: no run takes its steps.
    pub(crate) disarm: bool,
    /// The events applied, and the holds they took.
    pub(crate) events: usize,
    pub(crate) holds: usize,
}

/// Declares [`RolloutWord`] and [`RolloutWord::words`] from one variant list, so every word
/// `status rollout=` can print is among those its help is held to.
macro_rules! rollout_words {
    ($($(#[$doc:meta])* $variant:ident => $word:literal,)*) => {
        /// The word `status rollout=` prints for the last thing the session's rollout tap
        /// said ([`TapAnswers`]): one of these, or a [`Refusal`]'s.
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub(crate) enum RolloutWord {
            $($(#[$doc])* $variant,)*
            /// The tap bound nothing, and says why.
            Refused(Refusal),
        }

        impl RolloutWord {
            /// The word `status rollout=` prints.
            pub(crate) const fn word(self) -> &'static str {
                match self {
                    $(RolloutWord::$variant => $word,)*
                    RolloutWord::Refused(refusal) => refusal.word(),
                }
            }

            /// Every word a [`RolloutWord`] can print, from the same lists as the enums.
            #[cfg(test)]
            pub(crate) fn words() -> impl Iterator<Item = &'static str> {
                [$($word,)*]
                    .into_iter()
                    .chain(Refusal::ALL.iter().map(|refusal| refusal.word()))
            }
        }
    };
}

rollout_words! {
    /// The tap is bound: the rollout's rows reach the log, read `state=on` (its build and its
    /// writer's both measured).
    Bound => "bound",
    /// The tap is bound to a rollout a build nobody measured writes: its rows reach the log read
    /// `state=partial` (strong: they add, open a turn none runs and end only their own, fix E1),
    /// as an unmeasured Claude Code's transcript does.
    Partial => "partial",
    /// Nothing bound yet, and no refusal stands.
    Unbound => "unbound",
    /// The tap cannot be made for the target's thread, or a read-only target's record names none.
    NoThread => "no-thread",
    /// The queued claim, target, run or sampled process image no longer matches.
    Stale => "stale",
    /// The sweep dropped a tap that answered ([`SessionLog::rollout_stopped`]).
    Stopped => "stopped",
}

/// `status rollout=` while the tap was never asked for the session ([`SessionLog::rollout_fields`]).
pub(crate) const ROLLOUT_NEVER_ASKED: &str = "-";

/// WHAT THE ROLLOUT TAP ANSWERED (`status rollout= rollout_asks=`; session log 6D-3b's tap, its fence B5c): how many asks the session's rollout tap answered, each counted in a hold taken AFTER that ask's batch was applied (`crate::codex_rollout_tap`), and the last answer's word: `bound` (read `state=on`) or `partial` (a build nobody measured, read `state=partial`), or why it bound nothing (a `codex_tap::Refusal`'s word, `unknown-meta` among them; `stale` when the captured claim or read-only copy, record, run or sampled process image moved; `no-thread` if the tap cannot be made or a read-only copy's record names no thread), or `stopped` once the sweep dropped the tap (the claimant, or a released claim's process read only, gone from the front, the sweep off) until it asks again.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct TapAnswers {
    asks: u64,
    word: Option<RolloutWord>,
    /// The log's last seq when the count last moved: at or past every
    /// record the counted ask wrote, which the tap's tests read
    /// ([`SessionLog::rollout_counted_at`]).
    #[cfg(test)]
    counted_at: u64,
}

impl SessionLog {
    /// One ask the rollout tap answered, with its word ([`TapAnswers`]); one that reads no
    /// rollout now hands the run's turns back to its claims ([`Self::rollout_unread`]).
    pub(crate) fn rollout_answered(&mut self, word: RolloutWord) {
        self.rollout.asks = self.rollout.asks.saturating_add(1);
        self.rollout.word = Some(word);
        match word {
            RolloutWord::Bound | RolloutWord::Partial => {}
            RolloutWord::Unbound
            | RolloutWord::NoThread
            | RolloutWord::Stale
            | RolloutWord::Stopped
            | RolloutWord::Refused(_) => self.rollout_unread(),
        }
        #[cfg(test)]
        {
            self.rollout.counted_at = self.log.last_seq();
        }
    }

    /// The sweep dropped the session's tap ([`TapAnswers`]): a session the tap answered says `stopped`, its count kept; one never asked stays `-`. Its run's turns go back to its claims ([`Self::rollout_unread`]).
    pub(crate) fn rollout_stopped(&mut self) {
        if self.rollout.word.is_some() {
            self.rollout.word = Some(RolloutWord::Stopped);
        }
        self.rollout_unread();
    }

    /// THE ROLLOUT READS NOTHING NOW (the tap dropped, refused, stale or not bound): the run it
    /// reads takes its turns from its claims again (`aterm_slog::TurnTracker::tx_stopped`), as
    /// where no rollout ever bound, until the tap's next fact says it reads again.
    fn rollout_unread(&mut self) {
        if let Some(a) = self
            .agent
            .as_mut()
            .filter(|a| reads(a.who, Source::Rollout))
        {
            a.tracker.tx_stopped();
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
            self.rollout
                .word
                .map_or(ROLLOUT_NEVER_ASKED, RolloutWord::word),
            self.rollout.asks
        )
    }
}

/// Which tap's steps a batch carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Source {
    /// Claude Code's transcript, by the footer resolver's tap (plan §2.1).
    Transcript,
    /// A codex-alab's rollout, by the rollout tap.
    Rollout,
}

/// The tap that reads `who`'s run, if any: Claude Code's transcript and
/// Codex's rollout (only a codex-alab's is ever bound: the tap follows a
/// proven self-claim); no other agent's.
const fn reader_of(who: aterm_phase::Program) -> Option<Source> {
    match who {
        aterm_phase::Program::Claude => Some(Source::Transcript),
        aterm_phase::Program::Codex => Some(Source::Rollout),
        aterm_phase::Program::Agy
        | aterm_phase::Program::Muse
        | aterm_phase::Program::Grok
        | aterm_phase::Program::Kimi
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

/// Whether a proven self-claim's act can be the one that typed a codex Autopilot turn: its rule
/// types a turn (`acts::self_claim_types_turn`), except the plan box's yes (`RULE_PLAN`). That
/// yes starts its implementing turn through codex's TUI, which submits it as a person's turn
/// (`turn_trigger=user`), never `alab_autopilot`; an approval's or a question's answer types no
/// turn at all.
fn types_autopilot_turn(act: &str) -> bool {
    act != aterm_agent::supervise::policy::approval::RULE_PLAN
        && aterm_agent::harness::acts::self_claim_types_turn(act)
}

/// The latest a codex-alab rollout row reaches its file after the time it is stamped with, as the
/// pairing takes it: the tap's settle ([`crate::codex_rollout_tap::SETTLE`]), the allowance it
/// makes for codex-alab's writers. A read that began at `w` and reached the file's end read every
/// row stamped at or before `w - WRITE_LAG_MS`.
const WRITE_LAG_MS: u64 = crate::codex_rollout_tap::SETTLE
    .as_secs()
    .saturating_mul(1000);

/// What a codex run's harness pairing keeps between reads (gap 25, [`SessionLog::harness_pair`]):
/// held by its agent run, so a run's end drops it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct HarnessPairs {
    /// The newest watermark a rollout read to the file's end gave (wall ms): the read's start
    /// less [`WRITE_LAG_MS`], every row stamped before it read.
    pub(super) read_ms: u64,
    /// When the clock alone next settles a harness turn still waiting for its act (the log's
    /// monotonic ms, [`SessionLog::wake`]): [`Causes::clock`].
    pub(super) wake: Option<u64>,
    /// The read a harness turn waiting on the read side needs ([`SessionLog::harness_wait`]).
    pub(super) wait: Option<ReadWait>,
}

/// A harness turn of the run in front waits on the read side of its pairing (gap 25,
/// [`Causes::read`]): the rollout read that settles it, as the rollout tap's ask takes it
/// ([`crate::codex_rollout_tap::pair_wait`]), which owes one read for each wait it has not taken.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ReadWait {
    /// The soonest a rollout read to the file's end can begin and settle it (the log's wall ms).
    pub(crate) since: u64,
    /// The run's watermark from reads to the file's end ([`HarnessPairs::read_ms`]) as the wait
    /// was taken. A read to the end that leaves the turn waiting (one begun before `since`) raises
    /// it, so the ask takes the wait as new and asks for one more read; a read that fails or stops
    /// short of the end leaves it as it was.
    pub(crate) read_ms: u64,
}

/// What one pairing of a run's harness turns settled and what it leaves waiting
/// ([`harness_causes`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Causes {
    /// Each turn that takes its act, with that act mark's seq.
    paired: Vec<(Node, u64)>,
    /// The soonest wall time, still to come, at which the clock alone settles a waiting turn: one
    /// whose read side holds, at its open plus `d`; one with no act yet, at its open plus
    /// [`ATTRIBUTION_MS`].
    clock: Option<u64>,
    /// The soonest start of a rollout read to the file's end that settles the read side of a
    /// waiting turn: its act's time plus `d` plus [`WRITE_LAG_MS`]; it can lie in the past.
    read: Option<u64>,
}

/// THE ACTS THAT BEGAN HARNESS TURNS (gap 25), as far as the log settles them: for each turn of
/// `run` amended `turn_by=harness` whose records name no cause yet, the act mark it takes (its
/// seq), and what the turns left waiting wait for ([`Causes`]). The candidates are the act
/// marks (`mark … act holder=<h> harness_act=<rule>`, `SessionLog::self_claim_act`) of the run's
/// own holder (`h` is `codex-alab@<pid>`, `pid` the run's process group `pgid`), logged at or
/// after the run's open, whose rule types an Autopilot turn ([`types_autopilot_turn`]), within
/// [`ATTRIBUTION_MS`] of the turn's open, named by no turn's `cause=`, and with no other turn in
/// the log opened nearer them; the turn takes the nearest, `d` from its open. The window is
/// symmetric: the holder records its act once its turn started, and a goal's resume or a
/// compaction can record it before its turn begins. A pair is returned only once no nearer turn
/// and no nearer act can still reach the log: `read` passed the act's time plus `d` (every
/// rollout row stamped before `read` was read, so a turn that opened nearer the act is in the
/// log, as is one the screen opened, stamped by the log's clock, which passed `read`) and the
/// log's clock `now_ms` passed the turn's open plus `d` (act marks carry that clock, so an act
/// nearer the open was logged). So the act a turn takes is the same whichever of the
/// act mark and the turn's rows is logged first. A turn left waiting while `read` has not passed
/// its act's time plus `d` waits on the read side: a read to the file's end begun at that time
/// plus [`WRITE_LAG_MS`] or later passes it ([`Causes::read`]). One whose read side holds waits on
/// the clock alone, until its open plus `d`; one with no act yet, until its open plus
/// [`ATTRIBUTION_MS`], once every act of its window is logged, and never after that
/// ([`Causes::clock`]). Each kind's soonest is its own, so a read side long due hides no turn's
/// clock.
fn harness_causes(log: &Log, run: Node, pgid: i32, read: u64, now_ms: u64) -> Causes {
    let Some(run_t) = log.open_entry(run).map(|e| e.t) else {
        return Causes::default();
    };
    let ours = |r: &aterm_slog::Rec| {
        r.f.get_str(Key::Holder)
            .and_then(crate::self_claim::holder_pid)
            .is_some_and(|pid| i64::from(pid) == i64::from(pgid))
    };
    let (mut opens, mut harness, mut caused, mut named, mut acts) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for r in log.window() {
        match (r.op, r.k) {
            (Op::Open | Op::Amend, Kind::Turn) => {
                if r.op == Op::Open {
                    opens.push((r.node, r.t, r.par == Some(run)));
                } else if r.f.get(Key::TurnBy).is_some() {
                    harness.push(r.node);
                }
                if let Some(cause) = r.f.get(Key::Cause) {
                    caused.push(r.node);
                    if let Val::Id(id) = cause
                        && let Ok(seq) = id.as_str().parse::<u64>()
                    {
                        named.push(seq);
                    }
                }
            }
            (Op::Mark, Kind::Act)
                if r.t >= run_t
                    && ours(r)
                    && r.f
                        .get_str(Key::HarnessAct)
                        .is_some_and(types_autopilot_turn) =>
            {
                acts.push((r.seq, r.t));
            }
            _ => {}
        }
    }
    let soonest = |at: &mut Option<u64>, due: u64| *at = Some(at.map_or(due, |at| at.min(due)));
    let mut causes = Causes::default();
    for &(turn, o, _) in opens
        .iter()
        .filter(|(n, _, of_run)| *of_run && harness.contains(n) && !caused.contains(n))
    {
        let nearest = acts
            .iter()
            .filter(|&&(seq, t)| {
                t.abs_diff(o) <= ATTRIBUTION_MS
                    && !named.contains(&seq)
                    && !opens
                        .iter()
                        .any(|&(n, ot, _)| n != turn && ot.abs_diff(t) < o.abs_diff(t))
            })
            .min_by_key(|&&(seq, t)| (t.abs_diff(o), seq));
        match nearest {
            Some(&(seq, t)) => {
                let d = t.abs_diff(o);
                let (read_by, logged_by) = (t.saturating_add(d), o.saturating_add(d));
                if read < read_by {
                    soonest(&mut causes.read, read_by.saturating_add(WRITE_LAG_MS));
                } else if now_ms < logged_by {
                    soonest(&mut causes.clock, logged_by);
                } else {
                    named.push(seq);
                    causes.paired.push((turn, seq));
                }
            }
            None if now_ms < o.saturating_add(ATTRIBUTION_MS) => {
                soonest(&mut causes.clock, o.saturating_add(ATTRIBUTION_MS));
            }
            None => {}
        }
    }
    causes
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
            StepEvent::HarnessTurn { vref } => {
                if let Some(vref) = id(&vref) {
                    self.transcript_event(source, TxEvent::HarnessTurn { vref }, pos, state, now);
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

    /// THE HARNESS TURNS' PAIRING (gap 25, [`harness_causes`]): every harness turn of the run in
    /// front whose act is settled takes it, on the tracker's own cause amend
    /// ([`aterm_slog::TurnTracker::harness_cause`]), at the end of each batch of the rollout tap's,
    /// at the run's pair wake ([`SessionLog::tick`]) and just before the run ends
    /// ([`Self::harness_last`]). `eof` is the wall time a batch's read began, when it reached the
    /// rollout's end, which raises the run's watermark to it less [`WRITE_LAG_MS`]; the pairing
    /// reads the higher of that and the join's newest applied row ([`TxView::read_ms`]), so a turn
    /// that no later row follows still settles once a read to the end passes it. Each pass
    /// replaces the run's two waits with what the turns it leaves waiting wait for: its pair wake
    /// at the soonest the clock alone settles one ([`Causes::clock`], always still to come, so the
    /// tick at it never finds it passed), and its read wait ([`Causes::read`],
    /// [`SessionLog::harness_wait`]), which the loop hands to the rollout tap's ask. A read that
    /// fails or stops short of the end leaves [`HarnessPairs::read_ms`] as it was; its pass moves
    /// the read wait only by the rows it applied, the acts logged since and the clock, and a wait
    /// the pass leaves as it was, which the ask took already, arms nothing again.
    pub(super) fn harness_pair(&mut self, eof: Option<u64>, now: Stamp) {
        let Self { log, agent, .. } = self;
        let Some(a) = agent
            .as_mut()
            .filter(|a| log.is_open(a.node) && reads(a.who, Source::Rollout))
        else {
            return;
        };
        if let Some(began) = eof {
            a.pairs.read_ms = a.pairs.read_ms.max(began.saturating_sub(WRITE_LAG_MS));
        }
        let read = a.tracker.tx_view().read_ms.max(a.pairs.read_ms);
        let causes = harness_causes(log, a.node, a.pgid, read, now.wall_ms);
        for (turn, seq) in causes.paired {
            let _ = a.tracker.harness_cause(log, turn, seq, now);
        }
        a.pairs.wake = causes
            .clock
            .map(|due| now.mono_ms.saturating_add(due.saturating_sub(now.wall_ms)));
        a.pairs.wait = causes.read.map(|since| ReadWait {
            since,
            read_ms: a.pairs.read_ms,
        });
    }

    /// The read wait of the run in front ([`HarnessPairs::wait`]), as its rollout read pairing
    /// left it.
    fn pair_wait(&self) -> Option<ReadWait> {
        self.agent
            .as_ref()
            .filter(|a| self.log.is_open(a.node))
            .and_then(|a| a.pairs.wait)
    }

    /// THE PAIRING'S READ WAIT (gap 25): the read a harness turn of the run in front waits for
    /// ([`ReadWait`]), and the log's monotonic ms a read to the rollout's end begun from then on
    /// settles it (now, once that passed).
    pub(crate) fn harness_wait(&self) -> Option<(ReadWait, u64)> {
        let wait = self.pair_wait()?;
        let now = self.now();
        Some((
            wait,
            now.mono_ms
                .saturating_add(wait.since.saturating_sub(now.wall_ms)),
        ))
    }

    /// THE RUN'S LAST PAIRING (gap 25): just before `closing` closes, or the agent run ends
    /// (`closing` its own node), with the run at or under it, every harness turn of the run whose
    /// act the watermark and the clock settle takes it ([`Self::harness_pair`], neither side
    /// waived). A turn they leave waiting keeps no cause: a turn among the rows the tap has not
    /// read yet could have opened nearer its act, and an act still to be logged could be nearer
    /// its open.
    pub(super) fn harness_last(&mut self, closing: Node, at: Stamp) {
        let ends = self.agent.as_ref().is_some_and(|a| {
            closing == Node::SESSION || a.node == closing || self.descends(a.node, closing)
        });
        if ends {
            self.harness_pair(None, at);
        }
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

/// The order a batch applies in: its leading binds and
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
    /// The batch's last hold: what was left of it (at most [`CHUNK`] events; none for a read that found no new rows, whose one hold is still taken), then the wall's judge (a rollout's: the harness causes it settled), the flush and the owed wake.
    Last,
}

/// APPLY `source`'s `batch` to `timeline`'s session log: at most [`CHUNK`] events a hold, the lock
/// released between chunks, the wall judged (a rollout's harness causes paired), the batch's
/// unsaid loss said and the owed wake taken in the last.
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
        (bound, None),
        &mut crate::metrics::now_ns,
        &mut crate::metrics::note_tx_hold,
    )
}

/// [`apply_batch`] on the clock `now_ns` (monotonic nanoseconds), each chunk's hold handed to `book` with its kind ([`Hold`]); `read` is whether the tap is bound and, for a rollout read that reached its file's end, the wall time it began ([`SessionLog::harness_pair`]).
pub(crate) fn apply_batch_timed(
    timeline: &Mutex<SessionTimeline>,
    source: Source,
    batch: &StepBatch,
    read: (bool, Option<u64>),
    now_ns: &mut dyn FnMut() -> u64,
    book: &mut dyn FnMut(Hold, u64),
) -> Applied {
    apply_batch_guarded(timeline, source, batch, read, None, now_ns, book)
}

/// A rollout batch remains owned by the queued claim or read-only copy, the thread it names, and the run at EVERY chunk, including the first hold after its off-lock file read. `eof` is the wall time the read began, when it reached the rollout's end.
pub(crate) fn apply_rollout_batch(
    ctx: &crate::SessionCtx,
    target: &crate::self_claim::Tapped,
    run: TapRun,
    batch: &StepBatch,
    (bound, eof): (bool, Option<u64>),
) -> Applied {
    apply_batch_guarded(
        &ctx.timeline,
        Source::Rollout,
        batch,
        (bound, eof),
        Some((ctx, target, run)),
        &mut crate::metrics::now_ns,
        &mut crate::metrics::note_tx_hold,
    )
}

fn apply_batch_guarded(
    timeline: &Mutex<SessionTimeline>,
    source: Source,
    batch: &StepBatch,
    (bound, eof): (bool, Option<u64>),
    gate: Option<(&crate::SessionCtx, &crate::self_claim::Tapped, TapRun)>,
    now_ns: &mut dyn FnMut() -> u64,
    book: &mut dyn FnMut(Hold, u64),
) -> Applied {
    let mut applied = Applied::default();
    let mut items = ordered(batch).peekable();
    let (mut before, mut waited) = (None, None);
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
                applied.waits = slog.pair_wait() != waited;
                applied.owed = g.slog_take_wake();
            }
            return applied;
        }
        let now = slog.now();
        if applied.holds == 0 {
            before = slog.wake();
            waited = slog.pair_wait();
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
        } else {
            slog.harness_pair(eof, now);
        }
        slog.transcript_count(source, TxEvent::Flush, now);
        let after = slog.wake();
        applied.settle = after.filter(|_| after != before);
        applied.waits = slog.pair_wait() != waited;
        applied.owed = g.slog_take_wake();
        book(Hold::Last, now_ns().saturating_sub(taken));
        return applied;
    }
}

#[cfg(test)]
#[path = "session_log_transcript_tests.rs"]
mod tests;
