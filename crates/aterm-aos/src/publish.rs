// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Same-schema boundary publication (plan §4, design §3.9): replacing a hosted task's code at
//! a step boundary without ever running new code over old state, or old code over new state.
//!
//! # The protocol
//!
//! * **Construct** with the task's schema, identity, live code, state and **acceptance
//!   contract**: the boundary owns the contract it judges every candidate by, so no caller of
//!   `prepare` or `commit` can hand it another judge (see "What the contract check
//!   guarantees").
//! * **Attach** takes checkpoint C0 of the live state at the current boundary and starts an
//!   empty log; re-attaching re-anchors (a new checkpoint whose parent is the old one). An
//!   attach between an admitted input and its advance is refused ([`PublishRefusal::MidStep`]):
//!   no checkpoint is ever captured mid-step.
//! * **Live**: [`Boundary::admit`] records each input before the host applies it through the
//!   live code and calls [`Boundary::advance`] (or [`Boundary::withdraw`] when the step
//!   refused it, which takes it back out of the log). A full or broken log re-anchors at the
//!   next boundary rather than dropping anything. A second `admit` before the first one's
//!   `advance` breaks the log and captures nothing: the next input admitted at a true boundary
//!   re-anchors.
//! * **Pause** at a boundary captures the expected live head H (revision, branch, identity,
//!   state digest, cursors). While paused, queue-class inputs wait in a bounded queue and
//!   skip-class inputs are skipped.
//! * **Prepare** replays the *live* code from the ancestor checkpoint over the complete log
//!   range `[checkpoint cursor, H cursor)` and requires it to reproduce H's state digest
//!   exactly; checks the candidate's entry invariants on the ancestor state; replays the
//!   candidate from the same ancestor over the same inputs; judges both runs by the
//!   boundary's own contract (which must be the one the task identity names); and builds the
//!   [`Witness`] (private fields), which records that contract. The candidate's new state is
//!   never compared with H: it is meant to differ.
//! * **Commit** re-checks that the paused live head still equals H ([`PublishRefusal::StaleHead`])
//!   and that the witness matches this boundary's own ancestor and log; then installs the new
//!   [`Pair`] — candidate code, replayed state, a new revision, the identity with the
//!   candidate's code, a fresh checkpoint of the accepted state as the next ancestor and a
//!   new log epoch — in **one assignment**, and delivers the queue once, in order, through the
//!   new code.
//! * **Reject** resumes the old pair and delivers the queue once, in order, through it. A
//!   queue-class input that finds the queue full rejects the update **before** anything is
//!   dropped: the queue, then that input, are delivered to the old pair.
//!
//! A second candidate prepared against the same head is stale once the first commits: the
//! head's revision moved. A [`Boundary::restore`] mints a new branch and revision, so a
//! candidate keyed to the pre-restore head is refused even when the restored state's bytes
//! equal it.
//!
//! # What the contract check guarantees
//!
//! The acceptance contract is given once, to [`Boundary::new`], and `prepare` takes only the
//! candidate: after construction no caller can substitute a judge, and every witness a
//! boundary builds records the boundary's own contract artifact. `prepare` refuses
//! ([`PublishRefusal::ContractMismatch`]) when that contract's declared artifact is not the
//! one the task identity names, and `commit` refuses a witness naming any other contract.
//!
//! What is trusted: the constructor. A [`Contract`] is identified by the artifact id it
//! declares, so whoever constructs a boundary is trusted to give it the first-party contract
//! that id names (`TESTED_ASSUMPTIONS`). A second boundary constructed with a lenient
//! contract under the same id, the same identity, ancestor, log and head would build a
//! witness this boundary's `commit` cannot tell from its own; that is the same constructor
//! trust, not a gap a caller of `prepare` or `commit` can open.
//!
//! # What is refused, and what a refusal leaves
//!
//! Every [`PublishRefusal`] leaves the boundary exactly as it was: still paused (if it was),
//! the queue intact, the live pair untouched. `prepare` replays on private values only.
//!
//! # External effects
//!
//! The boundary holds no live effect sink: the inputs it delivers itself (the queue, the
//! overflowing input) run under [`NoEffects`], and its replays run under a [`ShadowPort`]
//! over an empty recording. A task that submits an external effect is therefore refused at
//! `prepare` ([`PublishRefusal::LiveEffects`] / [`PublishRefusal::CandidateEffects`]) rather
//! than published: an effect policy past the frontier is later work (plan §3).
//!
//! # Fault injection
//!
//! Under `cfg(any(test, debug_assertions))`, [`Boundary::commit_stopping_after`] runs the
//! same commit code but stops after its `k`-th primitive store; every `k` leaves either the
//! complete old pair or the complete new pair.

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

/// The live head: everything a publication must find unchanged.
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

/// The live (code, state, head) published as one value: replaced only as a whole.
pub struct Pair<S: Schema, C> {
    /// The live code.
    pub code: C,
    /// The live state.
    pub state: S::State,
    /// Its head. `head.state_digest` is refreshed lazily while running: it is exact after
    /// [`Boundary::pause`], [`Boundary::head`], `attach`, `commit` and `restore`.
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
    /// Running: the input is recorded (if attached); the host applies it through the live
    /// code and then calls [`Boundary::advance`] (or [`Boundary::withdraw`] if the step
    /// refused it).
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

/// A candidate ready to commit: its code, its replayed state, and the witness `prepare`
/// built. Private fields: only [`Boundary::prepare`] builds one.
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
    /// What each queued input returned when delivered, once each, in order, through the new
    /// code.
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

/// What a boundary has done. Counters saturate.
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
}

/// Why a boundary operation was refused. The boundary is untouched: still paused if it was,
/// the queue intact, the live pair and its log unchanged.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PublishRefusal {
    /// No checkpoint and log: attach first.
    NotAttached,
    /// The operation needs a paused boundary.
    NotPaused,
    /// The boundary is already paused.
    AlreadyPaused,
    /// An input was admitted and neither advanced nor withdrawn: not a step boundary.
    MidStep,
    /// The log is broken: some delivered input is missing, so no replay can cross it.
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
    /// The live head moved since the candidate was prepared. Both heads are boxed so a
    /// `Result<_, PublishRefusal>` stays small.
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
    /// ever dropped. It names the condition for a host's own report of that rejection.
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

/// One hosted task's live pair, its recording, its acceptance contract, and the
/// publication protocol over them.
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
            .field("stats", &self.stats)
            .finish_non_exhaustive()
    }
}

#[inline]
fn bump(n: &mut u64) {
    *n = n.saturating_add(1);
}

impl<S: Schema, C: Code<S> + Clone, K: Contract<S>> Boundary<S, C, K> {
    /// A running, detached boundary over `state`, at revision 0 on branch 0 with both
    /// cursors at 0, judging every candidate by `contract`. `queue_cap` bounds the inputs that
    /// may wait during a pause.
    ///
    /// The constructor is trusted to pass the first-party contract the identity's contract
    /// artifact names (module docs, "What the contract check guarantees"); a contract whose
    /// declared artifact is another one makes every `prepare` refuse
    /// ([`PublishRefusal::ContractMismatch`]).
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
        }
    }

    /// The live pair. Its head's state digest may lag while running (see [`Pair::head`]).
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

    /// Fault injection: replaces the current recording's log with `log`, returning the one it
    /// held (`None`, and `log` dropped, when detached). Nothing else changes, so a test can
    /// hold a foreign log beside this boundary's ancestor and watch `prepare` and `commit`
    /// refuse it ([`PublishRefusal::AncestorMismatch`]); no production path replaces a log.
    #[cfg(any(test, debug_assertions))]
    pub fn graft_log(&mut self, log: EventLog<S>) -> Option<EventLog<S>> {
        let rec = self.live.recording.as_mut()?;
        Some(core::mem::replace(&mut rec.log, log))
    }

    /// Live path. Running: records the input if attached, returns Run (the caller then applies
    /// it to `pair_mut().state` through the SAME code functions `C::step` uses, and calls
    /// `advance()`, or `withdraw()` if the step refused it). Paused: Queue-class inputs queue
    /// (Queued), Skip-class ones are Skipped; a full queue rejects the update and returns
    /// Rejected after delivering the queue and this input to the old pair through `C::step`.
    pub fn admit(&mut self, input: &S::Input) -> Admit {
        match &mut self.mode {
            Mode::Running => {
                if self.pending != Pending::None {
                    // Not a step boundary: the previous input may already be applied, so no
                    // checkpoint may be captured now. The log breaks, this input is not
                    // recorded, and the next input admitted at a true boundary re-anchors.
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

    /// The live code and state, for the host's live step. The head's state digest is
    /// refreshed lazily afterwards.
    #[inline]
    pub fn pair_mut(&mut self) -> (&C, &mut S::State) {
        self.live.digest_fresh = false;
        (&self.live.pair.code, &mut self.live.pair.state)
    }

    /// One delivered input: cursor + 1, state digest refresh deferred. Called without an
    /// admitted input it still counts the delivery, and breaks the log (the input is not in
    /// it).
    pub fn advance(&mut self) {
        if self.pending == Pending::None {
            self.break_log("an input was advanced without being admitted");
        }
        self.pending = Pending::None;
        bump(&mut self.stats.delivered);
        self.bump_cursor();
    }

    /// The admitted input's step refused it and left the state untouched: it leaves the log
    /// and the head stays where it was.
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

    /// Marks a recording break (an input the host could not record): no repair replays
    /// across it. The next live input re-anchors at a fresh checkpoint.
    pub fn mark_broken(&mut self, why: &'static str) {
        self.break_log(why);
    }

    /// Checkpoint C0 of the current state at the current boundary, and an empty log of
    /// epoch `log_epoch` holding at most `max_events` inputs. Re-attaching re-anchors (the
    /// new checkpoint's parent is the old one). A pending update is rejected first (the queue
    /// is delivered to the old pair; [`Boundary::take_delivered`] holds what it returned).
    ///
    /// # Errors
    ///
    /// [`PublishRefusal::MidStep`] between an admitted input and its advance or withdraw: the
    /// state may already hold the input the cursor does not count yet, so no checkpoint is
    /// captured, and nothing changes (the recording, the pause and its queue stay as they
    /// were).
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

    /// Stops recording. A pending update is rejected first (the queue is delivered to the old
    /// pair).
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

    /// Pauses at the current step boundary and returns the expected live head (with an exact
    /// state digest).
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

    /// Replays the live code and the candidate from the ancestor checkpoint over the log up
    /// to the paused head, judges both by the boundary's own contract, and returns the
    /// candidate ready to commit with its witness. Changes nothing but [`Stats::replays`].
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

        // The live code, from the ancestor, over the complete range, must land exactly on the
        // paused head: otherwise the log is not the history that produced it.
        let none: &[EffectRecord] = &[];
        // The live run is judged as soon as it is checked, and only its verdict and chain
        // outlive this block: its observations are not held through the candidate's replay,
        // so a repair holds one run's observations at a time. A contract is a pure judgement
        // of (inputs, run), so the verdict is the one a later judgement would give.
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
            // Repair is judged by the corrected contract, never by equivalence to the live
            // code: the live verdict is recorded, not required.
            (
                contract.judge(schema, inputs, &live_run),
                live_run.observation_chain,
            )
        };

        // The candidate continues from the same ancestor over the same inputs.
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

        // Repair is judged by the corrected contract, never by equivalence to the live code.
        let candidate_verdict = contract.judge(schema, inputs, &cand_run);
        if !candidate_verdict.passed() {
            return Err(PublishRefusal::CandidateFailed(candidate_verdict));
        }
        // Judged: the candidate's observations go here too, before the trace digest. They are
        // moved out and dropped by name, since a field a pattern leaves behind would stay
        // allocated in `cand_run` until `prepare` returns. `refused` is `None` here.
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

    /// Publishes a prepared candidate iff the paused live head still equals the head it was
    /// prepared against and its witness is this boundary's own: the new pair (code, replayed
    /// state, new revision, identity with the candidate's code, fresh checkpoint and log
    /// epoch) is installed in one assignment, then the queue is delivered once, in order,
    /// through the new code.
    pub fn commit(&mut self, prepared: Prepared<S, C>) -> Result<Commit<S>, PublishRefusal> {
        match self.commit_gated(prepared, &mut Always) {
            Ok(r) => r,
            Err(never) => match never {},
        }
    }

    /// Fault injection: [`Boundary::commit`], stopped after its `stores`-th primitive store.
    /// `None` when it stopped; the boundary then holds either the complete old pair or the
    /// complete new pair, and the queue is intact unless delivery began.
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
        // The primitive stores. Everything above only read; a stop before the second store
        // leaves the complete old pair, a stop after it the complete new one.
        gate.store()?;
        self.next_revision = next.revision_after;
        gate.store()?;
        self.live = new_live;
        gate.store()?;
        bump(&mut self.stats.publications);
        gate.store()?;
        let queue = match core::mem::replace(&mut self.mode, Mode::Running) {
            Mode::Paused { queue, .. } => queue,
            Mode::Running => VecDeque::new(),
        };
        // The pause is over and the new pair is live: the queue goes to it, once, in order.
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
    /// state. Reads only.
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

    /// Rejects the pending update: the old pair resumes and receives the queue once, in
    /// order. Running: nothing to reject, nothing delivered.
    pub fn reject(&mut self) -> Vec<Result<S::Observation, StepRefusal>> {
        if !self.is_paused() {
            return Vec::new();
        }
        self.resume_old(None)
    }

    /// Restores a checkpoint of this task under the live identity (same code: applying other
    /// code is `prepare`/`commit`) as a recorded transition, never an unchecked rewind: a new
    /// branch and revision are minted, the restored checkpoint stays untouched as the parent
    /// of a fresh one, and (if attached) a new log epoch starts.
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
        // `Checkpoint::restore` re-derived the stored digest under this schema; the head still
        // takes the digest of the state it now holds, never the checkpoint's word for it.
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
        self.next_revision = rev_after;
        self.next_branch = branch_after;
        self.live = Live {
            pair: Pair { code, state, head },
            identity,
            recording,
            digest_fresh: true,
        };
        bump(&mut self.stats.restores);
        Ok(restored)
    }

    /// What the boundary has done.
    #[inline]
    #[must_use]
    pub const fn stats(&self) -> Stats {
        self.stats
    }

    /// What each input delivered by the last overflow rejection, `attach` or `detach` returned
    /// (once each, in order); empties the record.
    #[inline]
    pub fn take_delivered(&mut self) -> Vec<Result<S::Observation, StepRefusal>> {
        core::mem::take(&mut self.last_delivered)
    }

    // ---- internals ----

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
    /// parent is the checkpoint it replaces. That parent's digest is taken only then: it is
    /// read by nothing else here, and an admit that needs no re-anchor (nearly every one)
    /// would hash the checkpoint for nothing.
    fn ensure_room(&mut self) {
        let Some(rec) = &self.live.recording else {
            return;
        };
        if rec.log.broken().is_none() && rec.log.len() < rec.log.max_events() {
            return;
        }
        let (parent, epoch) = (
            rec.checkpoint.digest(),
            rec.log.header().log_epoch.checked_add(1),
        );
        let Some(epoch) = epoch else {
            self.break_log("the log epochs are exhausted");
            return;
        };
        let rec = self.fresh_recording(epoch, Some(parent));
        self.live.recording = Some(rec);
        bump(&mut self.stats.reanchors);
    }

    /// Delivers one input the boundary holds (queued or overflowing) to the live pair through
    /// `C::step`, recording it if the step accepted it.
    fn deliver(&mut self, input: S::Input) -> Result<S::Observation, StepRefusal> {
        self.ensure_room();
        // The step holds `&mut` state whatever it returns: as on the live path
        // (`pair_mut`), the head's digest is re-derived before anyone reads it, so a step that
        // breaks the refusal law (a refusal that still moved the state) is caught by `pause`
        // and `prepare` (`LiveNotReproduced`) instead of hiding behind a stale digest.
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
