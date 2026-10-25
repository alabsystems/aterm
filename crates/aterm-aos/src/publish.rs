// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Same-schema boundary publication (plan §4, design §3.9).

use core::convert::Infallible;
use core::fmt;
use std::collections::VecDeque;

use crate::checkpoint::{Checkpoint, Provenance, RestoreRefusal};
use crate::contract::{Contract, Verdict};
use crate::effects::{EffectDivergence, EffectRecord, NoEffects, ShadowPort};
use crate::evidence::{Witness, WitnessParts};
use crate::ident::{ArtifactKind, BranchId, Digest, Revision, TaskIdentity};
use crate::log::{EventLog, IdentityMismatch, LogHeader, RangeFault};
use crate::replay::{Divergence, Run, replay};
use crate::task::{Code, PausedClass, Schema, StepRefusal, state_digest};

/// The live head.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Head {
    /// The task identity's digest ([`TaskIdentity::digest`]).
    pub identity: Digest,
    /// The live pair's revision.
    pub revision: Revision,
    /// The branch it is on.
    pub branch: BranchId,
    /// The live state's digest.
    pub state_digest: Digest,
    /// The cursor of the next input.
    pub input_cursor: u64,
    /// The cursor of the next effect.
    pub effect_cursor: u64,
}

/// The live (code, state, head) published as one value.
pub struct Pair<S: Schema, C> {
    /// The live code.
    pub code: C,
    /// The live state.
    pub state: S::State,
    /// Its head. `head.state_digest` is refreshed lazily while running.
    pub head: Head,
}

impl<S: Schema, C: Clone> Clone for Pair<S, C> {
    fn clone(&self) -> Self {
        Pair {
            code: self.code.clone(),
            state: self.state.clone(),
            head: self.head.clone(),
        }
    }
}

impl<S: Schema, C: Code<S>> fmt::Debug for Pair<S, C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Pair")
            .field("code", self.code.id())
            .field("head", &self.head)
            .finish_non_exhaustive()
    }
}

/// What [`Boundary::admit`] did with an input.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Admit {
    /// Running: the input is recorded (if attached).
    Run,
    /// Paused: the input waits in the queue.
    Queued,
    /// Paused: a skip-class input the task does not observe while paused.
    Skipped,
    /// Paused with a full queue: the update was rejected, and the `delivered_after` queued
    /// inputs and then this one were delivered, in order, to the old pair
    /// ([`Boundary::take_delivered`] holds what each returned).
    Rejected {
        /// How many queued inputs were delivered before this one.
        delivered_after: usize,
    },
}

/// A candidate ready to commit.
pub struct Prepared<S: Schema, C> {
    candidate: C,
    state: S::State,
    witness: Witness,
}

impl<S: Schema, C> Prepared<S, C> {
    /// The witness.
    #[inline]
    #[must_use]
    pub const fn witness(&self) -> &Witness {
        &self.witness
    }

    /// The candidate code.
    #[inline]
    #[must_use]
    pub const fn candidate(&self) -> &C {
        &self.candidate
    }

    /// The candidate's replayed state (what commit installs).
    #[inline]
    #[must_use]
    pub const fn state(&self) -> &S::State {
        &self.state
    }

    /// The live head this candidate was prepared against (the witness's head).
    #[inline]
    #[must_use]
    pub const fn expected(&self) -> &Head {
        self.witness.head()
    }
}

impl<S: Schema, C: Code<S>> fmt::Debug for Prepared<S, C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Prepared")
            .field("candidate", self.candidate.id())
            .field("witness", &self.witness)
            .finish_non_exhaustive()
    }
}

/// A published repair.
pub struct Commit<S: Schema> {
    /// The new pair's revision.
    pub revision: Revision,
    /// The witness it was published on.
    pub witness: Witness,
    /// What each queued input returned when delivered, once each, in order, through the new code.
    pub delivered: Vec<Result<S::Observation, StepRefusal>>,
}

impl<S: Schema> fmt::Debug for Commit<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Commit")
            .field("revision", &self.revision)
            .field("witness", &self.witness)
            .field("delivered", &self.delivered)
            .finish()
    }
}

/// What a boundary has done.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    /// Replays run by `prepare` (two per prepare that reached the candidate).
    pub replays: u64,
    /// Commits.
    pub publications: u64,
    /// Rejections (explicit, or by a full queue).
    pub rejections: u64,
    /// The longest the queue has been.
    pub queued_max: usize,
    /// Inputs handed to a step (live, queued or overflowing), refused ones included.
    pub delivered: u64,
    /// Of those, the ones the step refused.
    pub refused: u64,
    /// Skip-class inputs skipped while paused.
    pub skipped: u64,
    /// Re-anchors (a full or broken log replaced by a fresh checkpoint and log).
    pub reanchors: u64,
    /// Recording breaks.
    pub breaks: u64,
    /// Restores.
    pub restores: u64,
    /// Recordings moved into the retired ring.
    pub retired: u64,
    /// Retired recordings the full ring dropped, oldest first, before a drain took them.
    pub retired_dropped: u64,
}

/// Why a boundary operation was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PublishRefusal {
    /// No checkpoint and log.
    NotAttached,
    /// The operation needs a paused boundary.
    NotPaused,
    /// The boundary is already paused.
    AlreadyPaused,
    /// An input was admitted and neither advanced nor withdrawn.
    MidStep,
    /// The log is broken.
    TraceIncomplete(&'static str),
    /// The replay range is not inside the log.
    Range(RangeFault),
    /// The candidate (or the boundary's schema) has another schema digest than the live
    /// identity.
    SchemaChanged {
        /// The live identity's schema digest.
        live: Digest,
        /// The schema digest offered.
        candidate: Digest,
    },
    /// The candidate's artifact is not of kind code.
    NotCode {
        /// Its kind.
        kind: ArtifactKind,
    },
    /// The boundary's own contract (the one it was constructed with) is not the one the task
    /// identity names.
    ContractMismatch {
        /// The identity's contract digest.
        live: Digest,
        /// The digest of the contract the boundary holds.
        asked: Digest,
    },
    /// The candidate refuses the ancestor state.
    Entry(crate::task::EntryRefusal),
    /// The ancestor checkpoint does not restore under the live identity.
    Restore(RestoreRefusal),
    /// The live code's replay from the ancestor does not reproduce the paused head.
    LiveNotReproduced(Divergence),
    /// The live code's replay met the effect frontier.
    LiveEffects(EffectDivergence),
    /// The candidate's run fails the contract.
    CandidateFailed(Verdict),
    /// The candidate's replay refused the input at `cursor`.
    CandidateRefused {
        /// The cursor of the refused input.
        cursor: u64,
    },
    /// The candidate's replay met the effect frontier.
    CandidateEffects(EffectDivergence),
    /// The live head moved since the candidate was prepared.
    StaleHead {
        /// The head the candidate was prepared against.
        expected: Box<Head>,
        /// The live head now.
        live: Box<Head>,
    },
    /// The checkpoint, log and identity of the live pair do not descend from one another.
    LineageMismatch,
    /// The log extends another checkpoint than the boundary's ancestor: its header names
    /// another ancestor digest.
    AncestorMismatch {
        /// The digest of the boundary's ancestor checkpoint.
        checkpoint: Digest,
        /// The ancestor the log's header names.
        log: Digest,
    },
    /// The witness does not match this boundary's own ancestor, log or pair (the named part).
    WitnessMismatch(&'static str),
    /// The queue holds `cap` inputs. No operation of this boundary returns it: an input that
    /// finds the queue full rejects the update instead ([`Admit::Rejected`]), so nothing is
    /// ever dropped.
    QueueFull {
        /// The queue's capacity.
        cap: usize,
    },
    /// A counter (revision, branch, log epoch) has no next value.
    Exhausted(&'static str),
}

impl fmt::Display for PublishRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PublishRefusal::NotAttached => {
                f.write_str("the task is not attached: no checkpoint or log")
            }
            PublishRefusal::NotPaused => f.write_str("the task is not paused"),
            PublishRefusal::AlreadyPaused => f.write_str("the task is already paused"),
            PublishRefusal::MidStep => f.write_str(
                "an admitted input was neither advanced nor withdrawn: not a step boundary",
            ),
            PublishRefusal::TraceIncomplete(why) => write!(f, "the trace is incomplete: {why}"),
            PublishRefusal::Range(r) => write!(f, "the replay range is not in the log: {r}"),
            PublishRefusal::SchemaChanged { live, candidate } => write!(
                f,
                "the candidate's schema {} is not the live schema {}",
                candidate.short(),
                live.short()
            ),
            PublishRefusal::NotCode { kind } => {
                write!(f, "the candidate's artifact is of kind {kind:?}, not code")
            }
            PublishRefusal::ContractMismatch { live, asked } => write!(
                f,
                "the boundary's contract {} is not the contract {} the task identity names",
                asked.short(),
                live.short()
            ),
            PublishRefusal::Entry(e) => write!(f, "the candidate refuses the ancestor state: {e}"),
            PublishRefusal::Restore(r) => write!(f, "the ancestor does not restore: {r}"),
            PublishRefusal::LiveNotReproduced(d) => {
                write!(
                    f,
                    "the live code's replay does not reproduce the paused head: {d}"
                )
            }
            PublishRefusal::LiveEffects(d) => {
                write!(f, "the live code's replay met the effect frontier: {d}")
            }
            PublishRefusal::CandidateFailed(v) => {
                write!(f, "the candidate fails the contract: {v}")
            }
            PublishRefusal::CandidateRefused { cursor } => {
                write!(
                    f,
                    "the candidate refused the recorded input at cursor {cursor}"
                )
            }
            PublishRefusal::CandidateEffects(d) => {
                write!(f, "the candidate's replay met the effect frontier: {d}")
            }
            PublishRefusal::StaleHead { expected, live } => write!(
                f,
                "the live head moved since the candidate was prepared (revision {} cursor {} state {}, now revision {} cursor {} state {})",
                expected.revision.0,
                expected.input_cursor,
                expected.state_digest.short(),
                live.revision.0,
                live.input_cursor,
                live.state_digest.short()
            ),
            PublishRefusal::LineageMismatch => f.write_str(
                "the live pair, its checkpoint and its log do not descend from one another",
            ),
            PublishRefusal::AncestorMismatch { checkpoint, log } => write!(
                f,
                "the log extends checkpoint {}, not the ancestor checkpoint {}",
                log.short(),
                checkpoint.short()
            ),
            PublishRefusal::WitnessMismatch(part) => {
                write!(f, "the witness's {part} is not this boundary's own")
            }
            PublishRefusal::QueueFull { cap } => {
                write!(f, "the queue already holds the {cap} inputs it may hold")
            }
            PublishRefusal::Exhausted(what) => write!(f, "the {what} counter has no next value"),
        }
    }
}

impl std::error::Error for PublishRefusal {}

/// The ancestor checkpoint and the log since it.
struct Recording<S: Schema> {
    checkpoint: Checkpoint,
    log: EventLog<S>,
}

impl<S: Schema> Clone for Recording<S> {
    fn clone(&self) -> Self {
        Recording {
            checkpoint: self.checkpoint.clone(),
            log: self.log.clone(),
        }
    }
}

/// The most retired recordings a boundary holds before it drops the oldest.
pub const RETIRED_CAP: usize = 4;

/// Why a boundary retired a recording.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Retire {
    /// The log was full, and a re-anchor replaced it.
    Full,
    /// The log was broken, and a re-anchor replaced it.
    Broken,
    /// A restore replaced it.
    Restored,
    /// A commit replaced it, with the code it replaced.
    Committed,
}

impl Retire {
    /// The cause as one word.
    #[inline]
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Retire::Full => "full",
            Retire::Broken => "broken",
            Retire::Restored => "restored",
            Retire::Committed => "committed",
        }
    }
}

/// A recording the boundary replaced, with the code that ran it and the state it ended in.
pub struct Retired<S: Schema, C> {
    /// Why it was replaced.
    pub cause: Retire,
    /// The code that ran its inputs.
    pub code: C,
    /// Its ancestor checkpoint.
    pub checkpoint: Checkpoint,
    /// Its log.
    pub log: EventLog<S>,
    /// The digest of the live state when it was replaced.
    pub end_state: Digest,
}

impl<S: Schema, C: Clone> Clone for Retired<S, C> {
    fn clone(&self) -> Self {
        Retired {
            cause: self.cause,
            code: self.code.clone(),
            checkpoint: self.checkpoint.clone(),
            log: self.log.clone(),
            end_state: self.end_state,
        }
    }
}

impl<S: Schema, C: Code<S>> fmt::Debug for Retired<S, C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Retired")
            .field("cause", &self.cause)
            .field("code", self.code.id())
            .field("log", &self.log)
            .field("end_state", &self.end_state)
            .finish_non_exhaustive()
    }
}

/// Everything a commit or restore replaces, held in one value so that replacing it is one
/// assignment: the pair, the identity, the recording, and whether the head's state digest is
/// current.
struct Live<S: Schema, C> {
    pair: Pair<S, C>,
    identity: TaskIdentity,
    recording: Option<Recording<S>>,
    digest_fresh: bool,
}

impl<S: Schema, C: Clone> Clone for Live<S, C> {
    fn clone(&self) -> Self {
        Live {
            pair: self.pair.clone(),
            identity: self.identity.clone(),
            recording: self.recording.clone(),
            digest_fresh: self.digest_fresh,
        }
    }
}

/// What a commit installs besides the prepared code and state, computed before any store.
struct Publication<S: Schema> {
    head: Head,
    identity: TaskIdentity,
    recording: Recording<S>,
    revision: Revision,
    revision_after: u64,
}

enum Mode<S: Schema> {
    Running,
    Paused {
        expected: Head,
        queue: VecDeque<S::Input>,
    },
}

impl<S: Schema> Clone for Mode<S> {
    fn clone(&self) -> Self {
        match self {
            Mode::Running => Mode::Running,
            Mode::Paused { expected, queue } => Mode::Paused {
                expected: expected.clone(),
                queue: queue.clone(),
            },
        }
    }
}

/// The live path's admit/advance window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pending {
    /// At a step boundary.
    None,
    /// An input was admitted; `recorded` says whether it sits at the end of the current log.
    Admitted { recorded: bool },
}

/// The gate every primitive store of `commit` passes. The production gate never stops
/// (its stop type is uninhabited); the fault-injection gate stops after its `k`-th store.
trait StoreGate {
    type Stop;
    fn store(&mut self) -> Result<(), Self::Stop>;
}

struct Always;

impl StoreGate for Always {
    type Stop = Infallible;
    fn store(&mut self) -> Result<(), Infallible> {
        Ok(())
    }
}

#[cfg(any(test, debug_assertions))]
struct Countdown(usize);

#[cfg(any(test, debug_assertions))]
struct Stopped;

#[cfg(any(test, debug_assertions))]
impl StoreGate for Countdown {
    type Stop = Stopped;
    fn store(&mut self) -> Result<(), Stopped> {
        match self.0.checked_sub(1) {
            Some(left) => {
                self.0 = left;
                Ok(())
            }
            None => Err(Stopped),
        }
    }
}

/// One hosted task's live pair, its recording, its acceptance contract, and the publication protocol over them.
pub struct Boundary<S: Schema, C: Code<S> + Clone, K: Contract<S>> {
    schema: S,
    /// The contract every candidate is judged by, fixed at construction.
    contract: K,
    live: Live<S, C>,
    mode: Mode<S>,
    pending: Pending,
    queue_cap: usize,
    max_events: usize,
    next_revision: u64,
    next_branch: u64,
    stats: Stats,
    last_delivered: Vec<Result<S::Observation, StepRefusal>>,
    retired: VecDeque<Retired<S, C>>,
}

impl<S: Schema + Clone, C: Code<S> + Clone, K: Contract<S> + Clone> Clone for Boundary<S, C, K> {
    fn clone(&self) -> Self {
        Boundary {
            schema: self.schema.clone(),
            contract: self.contract.clone(),
            live: self.live.clone(),
            mode: self.mode.clone(),
            pending: self.pending,
            queue_cap: self.queue_cap,
            max_events: self.max_events,
            next_revision: self.next_revision,
            next_branch: self.next_branch,
            stats: self.stats,
            last_delivered: self.last_delivered.clone(),
            retired: self.retired.clone(),
        }
    }
}

impl<S: Schema, C: Code<S> + Clone, K: Contract<S>> fmt::Debug for Boundary<S, C, K> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Boundary")
            .field("pair", &self.live.pair)
            .field("contract", self.contract.id())
            .field("attached", &self.live.recording.is_some())
            .field("paused", &self.is_paused())
            .field("queued", &self.queued())
            .field("retired", &self.retired.len())
            .field("stats", &self.stats)
            .finish_non_exhaustive()
    }
}

#[inline]
const fn bump(n: &mut u64) {
    *n = n.saturating_add(1);
}

impl<S: Schema, C: Code<S> + Clone, K: Contract<S>> Boundary<S, C, K> {
    /// A running, detached boundary over `state`, at revision 0 on branch 0 with both cursors at 0, judging every candidate by `contract`.
    pub fn new(
        schema: S,
        identity: TaskIdentity,
        code: C,
        contract: K,
        state: S::State,
        queue_cap: usize,
    ) -> Self {
        let head = Head {
            identity: identity.digest(),
            revision: Revision(0),
            branch: BranchId(0),
            state_digest: state_digest(&schema, &state),
            input_cursor: 0,
            effect_cursor: 0,
        };
        Boundary {
            schema,
            contract,
            live: Live {
                pair: Pair { code, state, head },
                identity,
                recording: None,
                digest_fresh: true,
            },
            mode: Mode::Running,
            pending: Pending::None,
            queue_cap,
            max_events: 0,
            next_revision: 1,
            next_branch: 1,
            stats: Stats::default(),
            last_delivered: Vec::new(),
            retired: VecDeque::new(),
        }
    }

    /// The live pair.
    #[inline]
    #[must_use]
    pub const fn pair(&self) -> &Pair<S, C> {
        &self.live.pair
    }

    /// The schema.
    #[inline]
    #[must_use]
    pub const fn schema(&self) -> &S {
        &self.schema
    }

    /// The contract this boundary judges every candidate by.
    #[inline]
    #[must_use]
    pub const fn contract(&self) -> &K {
        &self.contract
    }

    /// The live task identity.
    #[inline]
    #[must_use]
    pub const fn identity(&self) -> &TaskIdentity {
        &self.live.identity
    }

    /// The live head with its state digest brought up to date.
    #[inline]
    pub fn head(&mut self) -> &Head {
        self.refresh_digest();
        &self.live.pair.head
    }

    /// Whether the boundary is paused.
    #[inline]
    #[must_use]
    pub const fn is_paused(&self) -> bool {
        matches!(self.mode, Mode::Paused { .. })
    }

    /// The head captured by [`Boundary::pause`], while paused.
    #[inline]
    #[must_use]
    pub const fn paused_head(&self) -> Option<&Head> {
        match &self.mode {
            Mode::Paused { expected, .. } => Some(expected),
            Mode::Running => None,
        }
    }

    /// How many inputs wait in the queue.
    #[inline]
    #[must_use]
    pub fn queued(&self) -> usize {
        match &self.mode {
            Mode::Paused { queue, .. } => queue.len(),
            Mode::Running => 0,
        }
    }

    /// The queue's capacity.
    #[inline]
    #[must_use]
    pub const fn queue_cap(&self) -> usize {
        self.queue_cap
    }

    /// The input that waits last in the queue, while paused.
    #[inline]
    #[must_use]
    pub fn last_queued(&self) -> Option<&S::Input> {
        match &self.mode {
            Mode::Paused { queue, .. } => queue.back(),
            Mode::Running => None,
        }
    }

    /// Fault injection.
    #[cfg(any(test, debug_assertions))]
    pub fn graft_log(&mut self, log: EventLog<S>) -> Option<EventLog<S>> {
        let rec = self.live.recording.as_mut()?;
        Some(core::mem::replace(&mut rec.log, log))
    }

    /// Live path. Running.
    pub fn admit(&mut self, input: &S::Input) -> Admit {
        match &mut self.mode {
            Mode::Running => {
                if self.pending != Pending::None {
                    self.break_log("an admitted input was neither advanced nor withdrawn");
                    self.pending = Pending::Admitted { recorded: false };
                    return Admit::Run;
                }
                self.ensure_room();
                let recorded = match &mut self.live.recording {
                    Some(rec) => match rec.log.push(input.clone()) {
                        Ok(_) => true,
                        Err(_) => {
                            rec.log
                                .mark_broken("an admitted input could not be recorded");
                            bump(&mut self.stats.breaks);
                            false
                        }
                    },
                    None => false,
                };
                self.pending = Pending::Admitted { recorded };
                Admit::Run
            }
            Mode::Paused { queue, .. } => match self.schema.paused_class(input) {
                PausedClass::Skip => {
                    bump(&mut self.stats.skipped);
                    Admit::Skipped
                }
                PausedClass::Queue if queue.len() < self.queue_cap => {
                    queue.push_back(input.clone());
                    self.stats.queued_max = self.stats.queued_max.max(queue.len());
                    Admit::Queued
                }
                PausedClass::Queue => {
                    let delivered_after = queue.len();
                    let outcomes = self.resume_old(Some(input.clone()));
                    self.last_delivered = outcomes;
                    Admit::Rejected { delivered_after }
                }
            },
        }
    }

    /// The live code and state, for the host's live step.
    #[inline]
    pub const fn pair_mut(&mut self) -> (&C, &mut S::State) {
        self.live.digest_fresh = false;
        (&self.live.pair.code, &mut self.live.pair.state)
    }

    /// One delivered input.
    pub fn advance(&mut self) {
        if self.pending == Pending::None {
            self.break_log("an input was advanced without being admitted");
        }
        self.pending = Pending::None;
        bump(&mut self.stats.delivered);
        self.bump_cursor();
    }

    /// The admitted input's step refused it and left the state untouched.
    pub fn withdraw(&mut self) {
        match self.pending {
            Pending::None => self.break_log("an input was withdrawn without being admitted"),
            Pending::Admitted { recorded: true } => {
                if let Some(rec) = &mut self.live.recording {
                    let _ = rec.log.retract_last();
                }
            }
            Pending::Admitted { recorded: false } => {}
        }
        self.pending = Pending::None;
        bump(&mut self.stats.delivered);
        bump(&mut self.stats.refused);
    }

    /// Marks a recording break (an input the host could not record).
    pub fn mark_broken(&mut self, why: &'static str) {
        self.break_log(why);
    }

    /// Checkpoint C0 of the current state at the current boundary, and an empty log of epoch `log_epoch` holding at most `max_events` inputs.
    ///
    /// # Errors
    /// [`PublishRefusal::MidStep`] between an admitted input and its advance or withdraw.
    pub fn attach(&mut self, log_epoch: u64, max_events: usize) -> Result<(), PublishRefusal> {
        if self.pending != Pending::None {
            return Err(PublishRefusal::MidStep);
        }
        if self.is_paused() {
            self.last_delivered = self.resume_old(None);
        }
        self.max_events = max_events;
        let parent = self.live.recording.as_ref().map(|r| r.checkpoint.digest());
        let rec = self.fresh_recording(log_epoch, parent);
        self.live.recording = Some(rec);
        Ok(())
    }

    /// Stops recording.
    pub fn detach(&mut self) {
        if self.is_paused() {
            self.last_delivered = self.resume_old(None);
        }
        self.live.recording = None;
        if self.pending != Pending::None {
            self.pending = Pending::Admitted { recorded: false };
        }
    }

    /// The current replay ancestor.
    #[inline]
    #[must_use]
    pub fn checkpoint(&self) -> Option<&Checkpoint> {
        self.live.recording.as_ref().map(|r| &r.checkpoint)
    }

    /// The log since the current ancestor.
    #[inline]
    #[must_use]
    pub fn log(&self) -> Option<&EventLog<S>> {
        self.live.recording.as_ref().map(|r| &r.log)
    }

    /// Pauses at the current step boundary and returns the expected live head (with an exact state digest).
    pub fn pause(&mut self) -> Result<Head, PublishRefusal> {
        if self.is_paused() {
            return Err(PublishRefusal::AlreadyPaused);
        }
        if self.live.recording.is_none() {
            return Err(PublishRefusal::NotAttached);
        }
        if self.pending != Pending::None {
            return Err(PublishRefusal::MidStep);
        }
        self.refresh_digest();
        let expected = self.live.pair.head.clone();
        self.mode = Mode::Paused {
            expected: expected.clone(),
            queue: VecDeque::new(),
        };
        Ok(expected)
    }

    /// Replays live and candidate code from the ancestor checkpoint to the paused head, judges both by the boundary contract, and returns a prepared candidate.
    pub fn prepare(&mut self, candidate: C) -> Result<Prepared<S, C>, PublishRefusal> {
        let mut replays = 0u64;
        let r = self.prepare_runs(candidate, &mut replays);
        self.stats.replays = self.stats.replays.saturating_add(replays);
        r
    }

    fn prepare_runs(
        &self,
        candidate: C,
        replays: &mut u64,
    ) -> Result<Prepared<S, C>, PublishRefusal> {
        let contract = &self.contract;
        let Some(rec) = &self.live.recording else {
            return Err(PublishRefusal::NotAttached);
        };
        let Mode::Paused { expected, .. } = &self.mode else {
            return Err(PublishRefusal::NotPaused);
        };
        let identity = &self.live.identity;
        let schema = &self.schema;
        if candidate.id().kind != ArtifactKind::Code {
            return Err(PublishRefusal::NotCode {
                kind: candidate.id().kind,
            });
        }
        let live_schema = identity.schema.digest;
        if schema.id() != &identity.schema {
            return Err(PublishRefusal::SchemaChanged {
                live: live_schema,
                candidate: schema.id().digest,
            });
        }
        let offered = candidate.schema_digest(schema);
        if offered != live_schema {
            return Err(PublishRefusal::SchemaChanged {
                live: live_schema,
                candidate: offered,
            });
        }
        if contract.id() != &identity.contract {
            return Err(PublishRefusal::ContractMismatch {
                live: identity.contract.digest,
                asked: contract.id().digest,
            });
        }
        if let Some(why) = rec.log.broken() {
            return Err(PublishRefusal::TraceIncomplete(why));
        }
        let live_code = &self.live.pair.code;
        let prov = rec.checkpoint.provenance();
        let header = rec.log.header();
        if prov.identity != *identity
            || header.identity != *identity
            || header.log_epoch != prov.log_epoch
            || header.first_cursor != prov.input_cursor
            || live_code.id() != &identity.code
            || live_code.schema_digest(schema) != live_schema
            || expected.identity != identity.digest()
        {
            return Err(PublishRefusal::LineageMismatch);
        }
        let ancestor_digest = rec.checkpoint.digest();
        if header.ancestor != ancestor_digest {
            return Err(PublishRefusal::AncestorMismatch {
                checkpoint: ancestor_digest,
                log: header.ancestor,
            });
        }
        let (from, to) = (prov.input_cursor, expected.input_cursor);
        let inputs = rec.log.range(from, to).map_err(PublishRefusal::Range)?;
        let ancestor = rec
            .checkpoint
            .restore(schema, identity)
            .map_err(PublishRefusal::Restore)?;

        let none: &[EffectRecord] = &[];
        let (live_verdict, live_observation_chain) = {
            let mut live_port = ShadowPort::new(none);
            let live_run = replay(
                schema,
                live_code,
                ancestor.clone(),
                inputs,
                from,
                &mut live_port,
            );
            bump(replays);
            if let Some(d) = live_port.frontier() {
                return Err(PublishRefusal::LiveEffects(*d));
            }
            if let Some((cursor, _)) = &live_run.refused {
                return Err(PublishRefusal::LiveNotReproduced(Divergence::Refused {
                    cursor: *cursor,
                }));
            }
            if live_run.state_digest != expected.state_digest {
                return Err(PublishRefusal::LiveNotReproduced(Divergence::State {
                    a: live_run.state_digest,
                    b: expected.state_digest,
                }));
            }
            live_port.finish().map_err(PublishRefusal::LiveEffects)?;
            (
                contract.judge(schema, inputs, &live_run),
                live_run.observation_chain,
            )
        };

        candidate
            .entry(schema, &ancestor)
            .map_err(PublishRefusal::Entry)?;
        let mut cand_port = ShadowPort::new(none);
        let cand_run = replay(schema, &candidate, ancestor, inputs, from, &mut cand_port);
        bump(replays);
        if let Some(d) = cand_port.frontier() {
            return Err(PublishRefusal::CandidateEffects(*d));
        }
        if let Some((cursor, _)) = &cand_run.refused {
            return Err(PublishRefusal::CandidateRefused { cursor: *cursor });
        }
        cand_port
            .finish()
            .map_err(PublishRefusal::CandidateEffects)?;

        let candidate_verdict = contract.judge(schema, inputs, &cand_run);
        if !candidate_verdict.passed() {
            return Err(PublishRefusal::CandidateFailed(candidate_verdict));
        }
        let Run {
            state: candidate_state,
            state_digest: candidate_state_digest,
            observation_chain: candidate_observation_chain,
            observations: candidate_observations,
            refused: _,
        } = cand_run;
        drop(candidate_observations);
        let trace_digest = rec
            .log
            .trace_digest(schema, from, to)
            .map_err(PublishRefusal::Range)?;
        let witness = Witness::reproduced(WitnessParts {
            trace_digest,
            from,
            to,
            log_epoch: header.log_epoch,
            ancestor: ancestor_digest,
            head: expected.clone(),
            contract: contract.id().clone(),
            live_code: live_code.id().clone(),
            live_verdict,
            live_observation_chain,
            candidate_code: candidate.id().clone(),
            candidate_verdict,
            candidate_observation_chain,
            candidate_state_digest,
        });
        Ok(Prepared {
            candidate,
            state: candidate_state,
            witness,
        })
    }

    /// Publishes a prepared candidate iff the paused live head still equals the head it was prepared against and its witness is this boundary's own.
    pub fn commit(&mut self, prepared: Prepared<S, C>) -> Result<Commit<S>, PublishRefusal> {
        match self.commit_gated(prepared, &mut Always) {
            Ok(r) => r,
            Err(never) => match never {},
        }
    }

    /// Fault injection.
    #[cfg(any(test, debug_assertions))]
    pub fn commit_stopping_after(
        &mut self,
        prepared: Prepared<S, C>,
        stores: usize,
    ) -> Option<Result<Commit<S>, PublishRefusal>> {
        self.commit_gated(prepared, &mut Countdown(stores)).ok()
    }

    fn commit_gated<G: StoreGate>(
        &mut self,
        prepared: Prepared<S, C>,
        gate: &mut G,
    ) -> Result<Result<Commit<S>, PublishRefusal>, G::Stop> {
        let next = match self.next_publication(&prepared) {
            Ok(next) => next,
            Err(r) => return Ok(Err(r)),
        };
        let Prepared {
            candidate,
            state,
            witness,
        } = prepared;
        let end_state = witness.head().state_digest;
        let new_live = Live {
            pair: Pair {
                code: candidate,
                state,
                head: next.head,
            },
            identity: next.identity,
            recording: Some(next.recording),
            digest_fresh: true,
        };
        gate.store()?;
        self.next_revision = next.revision_after;
        gate.store()?;
        let old = core::mem::replace(&mut self.live, new_live);
        if let Some(rec) = old.recording {
            self.retire(Retire::Committed, old.pair.code, rec, end_state);
        }
        gate.store()?;
        bump(&mut self.stats.publications);
        gate.store()?;
        let queue = match core::mem::replace(&mut self.mode, Mode::Running) {
            Mode::Paused { queue, .. } => queue,
            Mode::Running => VecDeque::new(),
        };
        let mut delivered = Vec::with_capacity(queue.len());
        for input in queue {
            delivered.push(self.deliver(input));
        }
        Ok(Ok(Commit {
            revision: next.revision,
            witness,
            delivered,
        }))
    }

    /// Every check commit makes, and everything it installs besides the prepared code and
    /// state.
    fn next_publication(
        &self,
        prepared: &Prepared<S, C>,
    ) -> Result<Publication<S>, PublishRefusal> {
        let Mode::Paused { .. } = &self.mode else {
            return Err(PublishRefusal::NotPaused);
        };
        let Some(rec) = &self.live.recording else {
            return Err(PublishRefusal::NotAttached);
        };
        let w = &prepared.witness;
        let mut live = self.live.pair.head.clone();
        if !self.live.digest_fresh {
            live.state_digest = state_digest(&self.schema, &self.live.pair.state);
        }
        if &live != w.head() {
            return Err(PublishRefusal::StaleHead {
                expected: Box::new(w.head().clone()),
                live: Box::new(live),
            });
        }
        let identity = &self.live.identity;
        if rec.checkpoint.provenance().identity != *identity
            || rec.log.header().identity != *identity
        {
            return Err(PublishRefusal::LineageMismatch);
        }
        if rec.log.header().ancestor != rec.checkpoint.digest() {
            return Err(PublishRefusal::AncestorMismatch {
                checkpoint: rec.checkpoint.digest(),
                log: rec.log.header().ancestor,
            });
        }
        if w.ancestor() != rec.checkpoint.digest() {
            return Err(PublishRefusal::WitnessMismatch("ancestor"));
        }
        if w.log_epoch() != rec.log.header().log_epoch {
            return Err(PublishRefusal::WitnessMismatch("log epoch"));
        }
        let (from, to) = w.range();
        if from != rec.checkpoint.provenance().input_cursor || to != live.input_cursor {
            return Err(PublishRefusal::WitnessMismatch("range"));
        }
        if let Some(why) = rec.log.broken() {
            return Err(PublishRefusal::TraceIncomplete(why));
        }
        let trace = rec
            .log
            .trace_digest(&self.schema, from, to)
            .map_err(PublishRefusal::Range)?;
        if trace != w.trace_digest() {
            return Err(PublishRefusal::WitnessMismatch("trace"));
        }
        if w.contract() != &identity.contract || w.contract() != self.contract.id() {
            return Err(PublishRefusal::WitnessMismatch("contract"));
        }
        if w.live_code() != self.live.pair.code.id() || w.live_code() != &identity.code {
            return Err(PublishRefusal::WitnessMismatch("live code"));
        }
        if w.candidate_code() != prepared.candidate.id() {
            return Err(PublishRefusal::WitnessMismatch("candidate code"));
        }
        if !w.same_code_reproduced() {
            return Err(PublishRefusal::WitnessMismatch("same-code replay"));
        }
        if !w.candidate_verdict().passed() {
            return Err(PublishRefusal::CandidateFailed(
                w.candidate_verdict().clone(),
            ));
        }
        let rev = self.next_revision;
        let revision_after = rev
            .checked_add(1)
            .ok_or(PublishRefusal::Exhausted("revision"))?;
        let epoch = rec
            .log
            .header()
            .log_epoch
            .checked_add(1)
            .ok_or(PublishRefusal::Exhausted("log epoch"))?;
        let new_identity = identity.with_code(prepared.candidate.id().clone());
        let revision = Revision(rev);
        let provenance = Provenance {
            identity: new_identity.clone(),
            log_epoch: epoch,
            input_cursor: live.input_cursor,
            effect_cursor: live.effect_cursor,
            branch: live.branch,
            revision,
            parent: Some(rec.checkpoint.digest()),
        };
        let checkpoint = Checkpoint::capture(&self.schema, &prepared.state, provenance);
        if checkpoint.state_digest() != w.candidate_state_digest() {
            return Err(PublishRefusal::WitnessMismatch("candidate state"));
        }
        let log = EventLog::new(
            LogHeader {
                identity: new_identity.clone(),
                log_epoch: epoch,
                first_cursor: live.input_cursor,
                ancestor: checkpoint.digest(),
            },
            self.max_events,
        );
        let head = Head {
            identity: new_identity.digest(),
            revision,
            branch: live.branch,
            state_digest: checkpoint.state_digest(),
            input_cursor: live.input_cursor,
            effect_cursor: live.effect_cursor,
        };
        Ok(Publication {
            head,
            identity: new_identity,
            recording: Recording { checkpoint, log },
            revision,
            revision_after,
        })
    }

    /// Rejects the pending update.
    pub fn reject(&mut self) -> Vec<Result<S::Observation, StepRefusal>> {
        if !self.is_paused() {
            return Vec::new();
        }
        self.resume_old(None)
    }

    /// Restores a checkpoint under the live identity as a recorded transition.
    pub fn restore(&mut self, checkpoint: &Checkpoint) -> Result<Head, PublishRefusal> {
        if self.is_paused() {
            return Err(PublishRefusal::AlreadyPaused);
        }
        if self.pending != Pending::None {
            return Err(PublishRefusal::MidStep);
        }
        let prov = checkpoint.provenance();
        if let Some(m) = IdentityMismatch::first(&prov.identity, &self.live.identity) {
            return Err(PublishRefusal::Restore(RestoreRefusal::Identity(m)));
        }
        let state = checkpoint
            .restore(&self.schema, &self.live.identity)
            .map_err(PublishRefusal::Restore)?;
        let restored_digest = state_digest(&self.schema, &state);
        let rev = self.next_revision;
        let rev_after = rev
            .checked_add(1)
            .ok_or(PublishRefusal::Exhausted("revision"))?;
        let branch = self.next_branch;
        let branch_after = branch
            .checked_add(1)
            .ok_or(PublishRefusal::Exhausted("branch"))?;
        let identity = self.live.identity.clone();
        let head = Head {
            identity: identity.digest(),
            revision: Revision(rev),
            branch: BranchId(branch),
            state_digest: restored_digest,
            input_cursor: prov.input_cursor,
            effect_cursor: prov.effect_cursor,
        };
        let recording = match &self.live.recording {
            None => None,
            Some(rec) => {
                let epoch = rec
                    .log
                    .header()
                    .log_epoch
                    .checked_add(1)
                    .ok_or(PublishRefusal::Exhausted("log epoch"))?;
                let fresh = Checkpoint::capture(
                    &self.schema,
                    &state,
                    Provenance {
                        identity: identity.clone(),
                        log_epoch: epoch,
                        input_cursor: head.input_cursor,
                        effect_cursor: head.effect_cursor,
                        branch: head.branch,
                        revision: head.revision,
                        parent: Some(checkpoint.digest()),
                    },
                );
                let log = EventLog::new(
                    LogHeader {
                        identity: identity.clone(),
                        log_epoch: epoch,
                        first_cursor: head.input_cursor,
                        ancestor: fresh.digest(),
                    },
                    self.max_events,
                );
                Some(Recording {
                    checkpoint: fresh,
                    log,
                })
            }
        };
        let code = self.live.pair.code.clone();
        let restored = head.clone();
        self.refresh_digest();
        let end_state = self.live.pair.head.state_digest;
        self.next_revision = rev_after;
        self.next_branch = branch_after;
        let old = core::mem::replace(
            &mut self.live,
            Live {
                pair: Pair { code, state, head },
                identity,
                recording,
                digest_fresh: true,
            },
        );
        if let Some(rec) = old.recording {
            self.retire(Retire::Restored, old.pair.code, rec, end_state);
        }
        bump(&mut self.stats.restores);
        Ok(restored)
    }

    /// Takes every retired recording, oldest first.
    pub fn drain_retired(&mut self) -> Vec<Retired<S, C>> {
        self.retired.drain(..).collect()
    }

    /// How many retired recordings wait for [`Boundary::drain_retired`].
    #[inline]
    #[must_use]
    pub fn retired_len(&self) -> usize {
        self.retired.len()
    }

    /// What the boundary has done.
    #[inline]
    #[must_use]
    pub const fn stats(&self) -> Stats {
        self.stats
    }

    /// What each input delivered by the last overflow rejection, `attach` or `detach` returned (once each, in order).
    #[inline]
    pub fn take_delivered(&mut self) -> Vec<Result<S::Observation, StepRefusal>> {
        core::mem::take(&mut self.last_delivered)
    }

    #[inline]
    fn refresh_digest(&mut self) {
        if !self.live.digest_fresh {
            self.live.pair.head.state_digest = state_digest(&self.schema, &self.live.pair.state);
            self.live.digest_fresh = true;
        }
    }

    fn break_log(&mut self, why: &'static str) {
        if let Some(rec) = &mut self.live.recording {
            rec.log.mark_broken(why);
            bump(&mut self.stats.breaks);
        }
    }

    #[inline]
    fn bump_cursor(&mut self) {
        match self.live.pair.head.input_cursor.checked_add(1) {
            Some(c) => self.live.pair.head.input_cursor = c,
            None => self.break_log("the input cursor is exhausted"),
        }
        self.live.digest_fresh = false;
    }

    /// A checkpoint of the live state at the current boundary and an empty log of `epoch`.
    fn fresh_recording(&mut self, epoch: u64, parent: Option<Digest>) -> Recording<S> {
        let head = &self.live.pair.head;
        let provenance = Provenance {
            identity: self.live.identity.clone(),
            log_epoch: epoch,
            input_cursor: head.input_cursor,
            effect_cursor: head.effect_cursor,
            branch: head.branch,
            revision: head.revision,
            parent,
        };
        let first_cursor = head.input_cursor;
        let checkpoint = Checkpoint::capture(&self.schema, &self.live.pair.state, provenance);
        self.live.pair.head.state_digest = checkpoint.state_digest();
        self.live.digest_fresh = true;
        let log = EventLog::new(
            LogHeader {
                identity: self.live.identity.clone(),
                log_epoch: epoch,
                first_cursor,
                ancestor: checkpoint.digest(),
            },
            self.max_events,
        );
        Recording { checkpoint, log }
    }

    /// Before an input is recorded: a broken or full log is replaced by a fresh checkpoint
    /// of the current state (a true boundary) and an empty log of the next epoch, whose
    /// parent is the checkpoint it replaces, and the replaced recording is retired.
    fn ensure_room(&mut self) {
        let Some(rec) = &self.live.recording else {
            return;
        };
        let cause = if rec.log.broken().is_some() {
            Retire::Broken
        } else if rec.log.len() < rec.log.max_events() {
            return;
        } else {
            Retire::Full
        };
        let (parent, epoch) = (
            rec.checkpoint.digest(),
            rec.log.header().log_epoch.checked_add(1),
        );
        let Some(epoch) = epoch else {
            self.break_log("the log epochs are exhausted");
            return;
        };
        let rec = self.fresh_recording(epoch, Some(parent));
        let end_state = rec.checkpoint.state_digest();
        if let Some(old) = self.live.recording.replace(rec) {
            let code = self.live.pair.code.clone();
            self.retire(cause, code, old, end_state);
        }
        bump(&mut self.stats.reanchors);
    }

    /// Moves a replaced recording into the retired ring, dropping the oldest when it is full.
    fn retire(&mut self, cause: Retire, code: C, rec: Recording<S>, end_state: Digest) {
        if self.retired.len() >= RETIRED_CAP {
            self.retired.pop_front();
            bump(&mut self.stats.retired_dropped);
        }
        self.retired.push_back(Retired {
            cause,
            code,
            checkpoint: rec.checkpoint,
            log: rec.log,
            end_state,
        });
        bump(&mut self.stats.retired);
    }

    /// Delivers one input the boundary holds (queued or overflowing) to the live pair through
    /// `C::step`, recording it if the step accepted it.
    fn deliver(&mut self, input: S::Input) -> Result<S::Observation, StepRefusal> {
        self.ensure_room();
        self.live.digest_fresh = false;
        let r = self.live.pair.code.step(
            &self.schema,
            &mut self.live.pair.state,
            &input,
            &mut NoEffects,
        );
        bump(&mut self.stats.delivered);
        match &r {
            Ok(_) => {
                if let Some(rec) = &mut self.live.recording
                    && rec.log.push(input).is_err()
                {
                    rec.log
                        .mark_broken("a delivered input could not be recorded");
                    bump(&mut self.stats.breaks);
                }
                self.bump_cursor();
            }
            Err(_) => bump(&mut self.stats.refused),
        }
        r
    }

    /// Ends a pause without publishing: the old pair resumes and receives the queue, then
    /// `extra`, once each, in order.
    fn resume_old(&mut self, extra: Option<S::Input>) -> Vec<Result<S::Observation, StepRefusal>> {
        let queue = match core::mem::replace(&mut self.mode, Mode::Running) {
            Mode::Paused { queue, .. } => queue,
            Mode::Running => VecDeque::new(),
        };
        bump(&mut self.stats.rejections);
        let mut out = Vec::with_capacity(queue.len().saturating_add(usize::from(extra.is_some())));
        for input in queue.into_iter().chain(extra) {
            out.push(self.deliver(input));
        }
        out
    }
}
